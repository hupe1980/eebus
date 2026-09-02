//! Limitation of Power Consumption and Production, end to end over SPINE.
//!
//! A control box and a heat pump run the pre-scenario communication of LPC §3.3 —
//! discovery, binding, subscription — and then scenario 1: a heartbeat, a limit, and the
//! acknowledgement that answers it. Everything travels as real datagrams, encoded and
//! decoded on the way, against a virtual clock.
//!
//! The last tests run the same exchange as LPP against a PV inverter, because the two use
//! cases share one implementation: what has to be checked is that the direction reaches
//! the wire, and that nothing else about the exchange changes.

use core::time::Duration;

use eebus::model::{
    CmdData, DeviceType, EnergyDirection, EntityType, FeatureAddress, FeatureType, Function,
    LoadControlLimitType, Role, ScaledNumber,
};
use eebus::spine::{
    Engine, ErrorNumber, LocalDevice, LocalEntity, LocalFeature, SpineEvent, node_management,
};
use eebus::usecases::descriptor::UseCaseDescriptor;
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, CsConfig, CsFeatures, Direction,
    EffectiveLimit, LimitationState,
};
use eebus::usecases::{lpc, lpp};

const FAILSAFE_WATTS: f64 = 4_200.0;

struct Link {
    guard: Engine,
    pump: Engine,
    actor: ControllableSystemActor,
    now: Duration,
}

impl Link {
    /// A control box and a heat pump, running LPC.
    fn new() -> Self {
        Self::build(
            lpc::DIRECTION,
            &lpc::CONTROLLABLE_SYSTEM,
            &lpc::ENERGY_GUARD,
            EntityType::HeatPumpAppliance,
        )
    }

    /// A control box and a PV inverter, running LPP.
    fn producing() -> Self {
        Self::build(
            lpp::DIRECTION,
            &lpp::CONTROLLABLE_SYSTEM,
            &lpp::ENERGY_GUARD,
            EntityType::Inverter,
        )
    }

    /// Two nodes with the features the limitation use cases ask of them.
    fn build(
        direction: Direction,
        controllable_system: &'static UseCaseDescriptor,
        energy_guard: &'static UseCaseDescriptor,
        entity: EntityType,
    ) -> Self {
        let mut pump_device =
            LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        let appliance = LocalEntity::new([1], entity)
            .with_feature(limitation::load_control_feature(1))
            .with_feature(limitation::device_configuration_feature(2))
            .with_feature(limitation::device_diagnosis_feature(3))
            .with_feature(limitation::device_diagnosis_client_feature(5));
        pump_device.add_entity(appliance).unwrap();

        let load_control = pump_device.address_of(&[1], 1);
        let configuration = pump_device.address_of(&[1], 2);
        let diagnosis = pump_device.address_of(&[1], 3);
        let diagnosis_client = pump_device.address_of(&[1], 5);

        let mut pump = Engine::new(pump_device);
        pump.add_use_case([1], 1, controllable_system);

        let actor = ControllableSystemActor::builder(
            ControllableSystem::new(
                CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600))
                    .with_nominal_max(11_000.0),
                Duration::ZERO,
            ),
            direction,
            CsFeatures {
                load_control,
                device_configuration: configuration,
                device_diagnosis: diagnosis,
                device_diagnosis_client: diagnosis_client,
            },
        )
        .install(&mut pump, Duration::ZERO);

        let mut guard_device = LocalDevice::new(
            "i:12345",
            "ControlBox-1",
            DeviceType::ElectricitySupplySystem,
        )
        .unwrap();
        guard_device
            .add_entity(LocalEntity::new([1], EntityType::GridGuard).with_feature(
                // The implementation guides §3.3 ask an actor to use one `Generic` client
                // feature for all of its client functionality.
                LocalFeature::new(1, FeatureType::Generic, Role::Client),
            ))
            .unwrap();
        let mut guard = Engine::new(guard_device);
        guard.add_use_case([1], 1, energy_guard);

        Self {
            guard,
            pump,
            actor,
            now: Duration::ZERO,
        }
    }

    fn guard_client(&self) -> FeatureAddress {
        self.guard.device().address_of(&[1], 1)
    }

    fn pump_feature(&self, feature: u32) -> FeatureAddress {
        self.pump.device().address_of(&[1], feature)
    }

    /// Delivers messages both ways, feeding the heat pump's events to its use case.
    fn pump_messages(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.guard.poll_transmit() {
                self.pump.handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.pump.poll_transmit() {
                self.guard.handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            // The use case decides on any write that is waiting, which produces the
            // acknowledgement the control box is expecting.
            let events: Vec<SpineEvent> = core::iter::from_fn(|| self.pump.poll_event()).collect();
            for event in &events {
                if self
                    .actor
                    .handle_event(&mut self.pump, event, self.now)
                    .is_some()
                {
                    moved = true;
                }
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    fn guard_events(&mut self) -> Vec<SpineEvent> {
        core::iter::from_fn(|| self.guard.poll_event()).collect()
    }

    fn last_result(&mut self) -> Option<ErrorNumber> {
        self.guard_events().into_iter().rev().find_map(|e| match e {
            SpineEvent::ResultReceived { error, .. } => Some(error),
            _ => None,
        })
    }

    /// The pre-scenario communication of LPC §3.3: discovery, then binding and
    /// subscription on each feature the Energy Guard needs.
    fn commission(&mut self) {
        let guard_nm = node_management(self.guard.device().address());
        let pump_nm = node_management(self.pump.device().address());

        self.guard.read(
            &pump_nm,
            &guard_nm,
            Function::NodeManagementDetailedDiscoveryData,
            self.now,
        );
        self.guard.read(
            &pump_nm,
            &guard_nm,
            Function::NodeManagementUseCaseData,
            self.now,
        );
        self.pump_messages();

        let client = self.guard_client();
        for feature in [1, 2] {
            let server = self.pump_feature(feature);
            self.guard.request_binding(&client, &server, self.now);
        }
        for feature in [1, 2, 3] {
            let server = self.pump_feature(feature);
            self.guard.request_subscription(&client, &server, self.now);
        }
        self.pump_messages();
        let _ = self.guard_events();
    }

    /// The Energy Guard's heartbeat, which is what keeps the failsafe at bay.
    fn heartbeat(&mut self) {
        self.actor.system_mut().on_heartbeat(self.now);
    }

    /// A limit write from the control box.
    fn write_limit(&mut self, watts: f64, active: bool) -> Option<ErrorNumber> {
        let client = self.guard_client();
        let server = self.pump_feature(1);
        let data = CmdData::LoadControlLimitListData(eebus::model::LoadControlLimitListData {
            load_control_limit_data: Some(vec![eebus::model::LoadControlLimitData {
                limit_id: Some(limitation::LIMIT_ID),
                is_limit_active: Some(active),
                value: Some(ScaledNumber::from_f64(watts, 0)),
                ..Default::default()
            }]),
        });
        self.guard.write(&server, &client, data, true, self.now);
        self.pump_messages();
        self.last_result()
    }

    fn advance(&mut self, by: Duration) {
        self.now += by;
        self.actor.system_mut().handle_timeout(self.now);
        self.guard.handle_timeout(self.now);
        self.pump.handle_timeout(self.now);
    }
}

fn round_trip(datagram: &eebus::model::Datagram) -> eebus::model::Datagram {
    let wire = eebus::model::to_json(datagram).expect("encode");
    let decoded = eebus::model::from_json_str(&wire).expect("decode");
    assert_eq!(&decoded, datagram, "the datagram survives the wire");
    decoded
}

/// The whole of scenario 1: commission, heartbeat, limit, acknowledgement.
#[test]
fn a_limit_reaches_the_heat_pump_and_is_acknowledged() {
    let mut link = Link::new();
    link.commission();

    assert_eq!(
        link.actor.system().effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS),
        "[LPC-901]: a fresh system runs on the failsafe value"
    );

    link.heartbeat();
    let result = link.write_limit(3_000.0, true).expect("an acknowledgement");

    assert_eq!(result, ErrorNumber::None, "ACK, [LPC-002/1]");
    assert_eq!(link.actor.system().state(), LimitationState::Limited);
    assert_eq!(
        link.actor.system().effective_limit(),
        EffectiveLimit::Active(3_000.0)
    );

    // And the heat pump now serves the limit it applied, so a peer reading it sees the
    // truth rather than what it asked for.
    let published = link
        .pump
        .device()
        .resolve(&link.pump_feature(1))
        .unwrap()
        .data(&Function::LoadControlLimitListData)
        .expect("the limit is published");
    let CmdData::LoadControlLimitListData(list) = published else {
        panic!("expected the limit list");
    };
    let entry = &list.load_control_limit_data.as_ref().unwrap()[0];
    assert_eq!(entry.is_limit_active, Some(true));
    assert_eq!(entry.value.as_ref().unwrap().to_f64(), Some(3_000.0));
}

/// LPC implementation guide §2.14: a limit that arrives without a recent heartbeat is
/// refused, and the answer the control box gets is a NACK.
#[test]
fn ig_2_14_a_limit_without_a_heartbeat_is_refused_over_the_wire() {
    let mut link = Link::new();
    link.commission();

    // No heartbeat: the system is still in `init`.
    let result = link.write_limit(3_000.0, true).expect("an acknowledgement");

    assert_eq!(result, ErrorNumber::CommandRejected, "NACK, [LPC-003/1]");
    assert_eq!(link.actor.system().state(), LimitationState::Init);
    assert_eq!(
        link.actor.system().effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS),
        "the failsafe still applies"
    );

    // And nothing was stored: a refused write must not change the served data.
    let published = link
        .pump
        .device()
        .resolve(&link.pump_feature(1))
        .unwrap()
        .data(&Function::LoadControlLimitListData)
        .expect("the description is still published");
    let CmdData::LoadControlLimitListData(list) = published else {
        panic!("expected the limit list");
    };
    assert_eq!(
        list.load_control_limit_data.as_ref().unwrap()[0].is_limit_active,
        Some(false),
        "the published limit is the one in force, not the one refused"
    );
}

/// LPC implementation guide §2.15: the failsafe values are writeable, and the write is
/// reflected in what the heat pump serves.
#[test]
fn ig_2_15_the_failsafe_values_can_be_written_over_the_wire() {
    let mut link = Link::new();
    link.commission();
    link.heartbeat();
    link.write_limit(3_000.0, true).unwrap();

    let client = link.guard_client();
    let server = link.pump_feature(2);
    let data = CmdData::DeviceConfigurationKeyValueListData(
        eebus::model::DeviceConfigurationKeyValueListData {
            device_configuration_key_value_data: Some(vec![
                eebus::model::DeviceConfigurationKeyValueData {
                    key_id: Some(limitation::FAILSAFE_LIMIT_KEY),
                    value: Some(eebus::model::DeviceConfigurationKeyValueValue {
                        scaled_number: Some(ScaledNumber::from_f64(2_000.0, 0)),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ]),
        },
    );
    link.guard.write(&server, &client, data, true, link.now);
    link.pump_messages();

    assert_eq!(link.last_result(), Some(ErrorNumber::None));
    assert_eq!(link.actor.system().config().failsafe_watts, 2_000.0);

    // The new value is what a peer reading the configuration now sees.
    let published = link
        .pump
        .device()
        .resolve(&server)
        .unwrap()
        .data(&Function::DeviceConfigurationKeyValueListData)
        .expect("the values are published");
    let CmdData::DeviceConfigurationKeyValueListData(list) = published else {
        panic!("expected the key values");
    };
    let entry = list
        .device_configuration_key_value_data
        .as_ref()
        .unwrap()
        .iter()
        .find(|e| e.key_id == Some(limitation::FAILSAFE_LIMIT_KEY))
        .unwrap();
    assert_eq!(
        entry
            .value
            .as_ref()
            .unwrap()
            .scaled_number
            .as_ref()
            .unwrap()
            .to_f64(),
        Some(2_000.0)
    );
}

/// LPC implementation guide §2.17: when the control box goes silent the heat pump falls
/// back to the failsafe, and stays there. No disconnection is signalled — the heartbeats
/// simply stop, which is what a lost connection looks like from inside the use case.
#[test]
fn ig_2_17_silence_reaches_the_failsafe_and_holds() {
    let mut link = Link::new();
    link.commission();
    link.heartbeat();
    link.write_limit(3_000.0, true).unwrap();
    assert_eq!(link.actor.system().state(), LimitationState::Limited);

    link.advance(Duration::from_secs(120));
    assert_eq!(link.actor.system().state(), LimitationState::FailsafeState);
    assert_eq!(
        link.actor.system().effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );

    // Two minutes on — the point at which `init` would have gone unlimited.
    link.advance(Duration::from_secs(120));
    assert_eq!(link.actor.system().state(), LimitationState::FailsafeState);
}

/// The heartbeat the heat pump sends reaches the control box, which subscribed to it.
#[test]
fn the_heat_pumps_heartbeat_reaches_its_subscriber() {
    let mut link = Link::new();
    link.commission();

    // Sixty seconds on, the producer puts one on the wire (LPC/LPP Table 26).
    link.now += Duration::from_secs(60);
    link.actor.handle_timeout(&mut link.pump, link.now);
    link.pump_messages();

    let notified = link
        .guard_events()
        .into_iter()
        .find_map(|e| match e {
            SpineEvent::DataNotified { data, .. } => limitation::read_heartbeat(&data),
            _ => None,
        })
        .expect("the heartbeat arrived");
    assert_eq!(notified, 1);
}

/// The control box discovers the heat pump plays the Controllable System, and finds the
/// features the use case needs on the entity that plays it.
#[test]
fn discovery_finds_the_use_case_and_its_features() {
    let mut link = Link::new();
    link.commission();

    let pump_address = link.pump.device().address().clone();
    let remote = link.guard.peer(&pump_address).expect("the peer");

    let use_case = remote
        .use_case("limitationOfPowerConsumption", "ControllableSystem")
        .expect("the use case");
    for scenario in 1..=4 {
        assert!(use_case.supports_scenario(scenario), "scenario {scenario}");
    }

    let load_control = remote
        .feature_for(use_case, &FeatureType::LoadControl, Role::Server)
        .expect("the load control feature");
    assert!(load_control.is_writeable(&Function::LoadControlLimitListData));

    assert!(
        remote
            .feature_for(use_case, &FeatureType::DeviceConfiguration, Role::Server)
            .is_some(),
        "and the failsafe values"
    );
    assert!(
        remote
            .feature_for(use_case, &FeatureType::DeviceDiagnosis, Role::Server)
            .is_some(),
        "and the heartbeat"
    );
}

/// A second energy manager cannot take the binding from the first — the rule SPINE
/// 1.4.0 makes mandatory and the LPC implementation guide §3.5 asks for already.
#[test]
fn a_second_energy_guard_cannot_take_the_binding() {
    let mut link = Link::new();
    link.commission();

    let intruder = eebus::spine::feature_address(
        &eebus::spine::device_address("i:99999", "OtherCem").unwrap(),
        &[1],
        1,
    );
    let server = link.pump_feature(1);
    assert!(
        link.pump
            .relations()
            .is_bound(&link.guard_client(), &server),
        "the first control box holds it"
    );
    assert!(!link.pump.relations().is_bound(&intruder, &server));
}

/// LPP is the same exchange in the other direction. A control box limits what a PV
/// inverter may feed in, and the inverter answers exactly as a heat pump would.
#[test]
fn a_production_limit_reaches_the_inverter_and_is_acknowledged() {
    let mut link = Link::producing();
    link.commission();

    link.heartbeat();
    let result = link.write_limit(1_500.0, true).expect("an acknowledgement");

    assert_eq!(result, ErrorNumber::None, "ACK, [LPP-002/1]");
    assert_eq!(link.actor.system().state(), LimitationState::Limited);
    assert_eq!(
        link.actor.system().effective_limit(),
        EffectiveLimit::Active(1_500.0)
    );
}

/// What LPP changes is what the Controllable System *publishes*: the direction of the
/// limit and the name of the failsafe key. Everything else is the same code, so this is
/// the test that keeps the two use cases from being confused for one another.
#[test]
fn the_two_use_cases_publish_their_own_direction_and_key_name() {
    for (mut link, direction, key) in [
        (
            Link::new(),
            EnergyDirection::Consume,
            "failsafeConsumptionActivePowerLimit",
        ),
        (
            Link::producing(),
            EnergyDirection::Produce,
            "failsafeProductionActivePowerLimit",
        ),
    ] {
        link.commission();

        let load_control = link.pump.device().resolve(&link.pump_feature(1)).unwrap();
        let CmdData::LoadControlLimitDescriptionListData(descriptions) = load_control
            .data(&Function::LoadControlLimitDescriptionListData)
            .expect("the limit description is published")
        else {
            panic!("expected the limit descriptions");
        };
        let description = &descriptions
            .load_control_limit_description_data
            .as_ref()
            .unwrap()[0];
        assert_eq!(description.limit_direction, Some(direction));
        assert_eq!(
            description.limit_type,
            Some(LoadControlLimitType::SignDependentAbsValueLimit),
            "both use cases fix the limit type"
        );

        let configuration = link.pump.device().resolve(&link.pump_feature(2)).unwrap();
        let CmdData::DeviceConfigurationKeyValueDescriptionListData(keys) = configuration
            .data(&Function::DeviceConfigurationKeyValueDescriptionListData)
            .expect("the key descriptions are published")
        else {
            panic!("expected the key descriptions");
        };
        let keys = keys
            .device_configuration_key_value_description_data
            .as_ref()
            .unwrap();
        assert_eq!(
            keys[0].key_name.as_ref().map(|k| k.as_str()),
            Some(key),
            "the failsafe key names its own use case"
        );
        assert_eq!(
            keys[1].key_name.as_ref().map(|k| k.as_str()),
            Some("failsafeDurationMinimum"),
            "the duration key is common to both"
        );
    }
}

/// An Energy Guard discovers which of the two an appliance plays. A device may play
/// both — a battery limits its charging and its discharging — so the name is what tells
/// the two apart, not the features, which are identical.
#[test]
fn discovery_tells_the_two_use_cases_apart() {
    let mut consuming = Link::new();
    consuming.commission();
    let mut producing = Link::producing();
    producing.commission();

    let address = consuming.pump.device().address().clone();
    let peer = consuming.guard.peer(&address).unwrap();
    assert!(
        peer.use_case("limitationOfPowerConsumption", "ControllableSystem")
            .is_some()
    );
    assert!(
        peer.use_case("limitationOfPowerProduction", "ControllableSystem")
            .is_none(),
        "a heat pump running LPC does not claim LPP"
    );

    let address = producing.pump.device().address().clone();
    let peer = producing.guard.peer(&address).unwrap();
    assert!(
        peer.use_case("limitationOfPowerProduction", "ControllableSystem")
            .is_some()
    );
    assert!(
        peer.use_case("limitationOfPowerConsumption", "ControllableSystem")
            .is_none()
    );
}

/// SPINE implementation guide §3.3: in a partial write an omitted element means
/// *unchanged*, so a write that adjusts only `value` leaves the limit active.
///
/// Deciding on the arriving fragment instead would read the absent `isLimitActive` as
/// `false` and release the heat pump back to full power.
#[test]
fn ig_3_3_a_partial_limit_write_that_omits_is_limit_active_stays_active() {
    let mut link = Link::new();
    link.commission();
    link.heartbeat();

    // A limit is in force at 3 kW.
    assert_eq!(link.write_limit(3_000.0, true), Some(ErrorNumber::None));
    assert_eq!(link.actor.system().state(), LimitationState::Limited);

    // The grid tightens it to 2 kW, sending only what changed.
    let client = link.guard_client();
    let server = link.pump_feature(1);
    let data = CmdData::LoadControlLimitListData(eebus::model::LoadControlLimitListData {
        load_control_limit_data: Some(vec![eebus::model::LoadControlLimitData {
            limit_id: Some(limitation::LIMIT_ID),
            is_limit_active: None, // omitted: unchanged
            value: Some(ScaledNumber::from_f64(2_000.0, 0)),
            ..Default::default()
        }]),
    });
    link.guard.write(&server, &client, data, true, link.now);
    link.pump_messages();

    assert_eq!(link.last_result(), Some(ErrorNumber::None));
    assert_eq!(
        link.actor.system().state(),
        LimitationState::Limited,
        "an omitted `isLimitActive` is unchanged, not a deactivation"
    );
    assert_eq!(
        link.actor.system().effective_limit(),
        EffectiveLimit::Active(2_000.0),
        "and the new value is the one in force"
    );
}

/// Use-case implementation guide §3.1: every message carries an entry's identifiers.
///
/// An entry without them names nothing stored, so appending it as new would leave two
/// limits where the use case defines one.
#[test]
fn ig_3_1_a_limit_write_without_its_identifier_is_refused() {
    let mut link = Link::new();
    link.commission();
    link.heartbeat();
    assert_eq!(link.write_limit(3_000.0, true), Some(ErrorNumber::None));

    let client = link.guard_client();
    let server = link.pump_feature(1);
    let data = CmdData::LoadControlLimitListData(eebus::model::LoadControlLimitListData {
        load_control_limit_data: Some(vec![eebus::model::LoadControlLimitData {
            limit_id: None, // no identifier: this entry names nothing
            is_limit_active: Some(false),
            value: Some(ScaledNumber::from_f64(0.0, 0)),
            ..Default::default()
        }]),
    });
    link.guard.write(&server, &client, data, true, link.now);
    link.pump_messages();

    assert_eq!(
        link.last_result(),
        Some(ErrorNumber::CommandRejected),
        "an unidentifiable entry is refused rather than appended"
    );

    // The stored list still holds exactly the one limit the use case defines.
    let published = link
        .pump
        .device()
        .resolve(&server)
        .unwrap()
        .data(&Function::LoadControlLimitListData)
        .expect("the limit is published");
    let CmdData::LoadControlLimitListData(list) = published else {
        panic!("expected the limit list");
    };
    assert_eq!(
        list.load_control_limit_data.as_ref().unwrap().len(),
        1,
        "no phantom entry was appended"
    );
    assert_eq!(
        link.actor.system().state(),
        LimitationState::Limited,
        "and the limit that was in force is untouched"
    );
}
