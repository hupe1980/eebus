//! Both halves of Monitoring of Power Consumption, talking to each other.
//!
//! `monitoring_over_the_wire` drives the reader by hand. This one uses the real
//! Monitoring Appliance, so what is under test is the commissioning it performs: read
//! the descriptions, subscribe to the values, and resolve everything that arrives
//! afterwards against what the descriptions said.

use core::time::Duration;

use eebus::model::{
    DeviceType, ElectricalConnectionPhaseName as Phase, EntityType, FeatureType, Function, Role,
};
use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature, SpineEvent, node_management};
use eebus::usecases::descriptor::actors;
use eebus::usecases::monitoring::{
    self, Measurand, MonitoredUnit, MonitoringApplianceActor, MonitoringEvent, Quantity,
};
use eebus::usecases::mpc;

struct Pair {
    manager: Engine,
    appliance: MonitoringApplianceActor,
    unit_engine: Engine,
    unit: MonitoredUnit,
    electrical_connection: eebus::model::FeatureAddress,
    measurement: eebus::model::FeatureAddress,
    now: Duration,
    events: Vec<MonitoringEvent>,
}

impl Pair {
    fn new() -> Self {
        let now = Duration::ZERO;
        let unit = MonitoredUnit::new(1)
            .with(Measurand::total_power())
            .with(Measurand::on(Quantity::Current, Phase::A))
            .with(Measurand::total(Quantity::EnergyConsumed));

        let mut unit_device =
            LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        unit_device
            .add_entity(
                LocalEntity::new([1], EntityType::HeatPumpAppliance)
                    .with_feature(unit.electrical_connection_feature(1))
                    .with_feature(unit.measurement_feature(2)),
            )
            .unwrap();
        let electrical_connection = unit_device.address_of(&[1], 1);
        let measurement = unit_device.address_of(&[1], 2);
        let mut unit_engine = Engine::new(unit_device);
        unit_engine.add_use_case([1], 1, &mpc::MONITORED_UNIT);
        unit.publish(&mut unit_engine, &electrical_connection, &measurement);

        let mut manager_device = LocalDevice::new(
            "i:12345",
            "EnergyManager-1",
            DeviceType::EnergyManagementSystem,
        )
        .unwrap();
        manager_device
            .add_entity(
                LocalEntity::new([1], EntityType::CEM).with_feature(LocalFeature::new(
                    1,
                    FeatureType::Generic,
                    Role::Client,
                )),
            )
            .unwrap();
        let client = manager_device.address_of(&[1], 1);
        let mut manager = Engine::new(manager_device);
        manager.add_use_case([1], 1, &mpc::MONITORING_APPLIANCE);

        Self {
            manager,
            appliance: MonitoringApplianceActor::new(client),
            unit_engine,
            unit,
            electrical_connection,
            measurement,
            now,
            events: Vec::new(),
        }
    }

    fn commission(&mut self) {
        let manager_nm = node_management(self.manager.device().address());
        let unit_nm = node_management(self.unit_engine.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager.read(&unit_nm, &manager_nm, function, self.now);
        }
        self.settle();

        let device = self.unit_engine.device().address().clone();
        let remote = self.manager.peer(&device).expect("the heat pump");
        let peer = monitoring::locate(
            remote,
            "monitoringOfPowerConsumption",
            actors::MONITORED_UNIT,
        )
        .expect("a Monitored Unit");
        assert_eq!(peer.measurement, self.measurement);
        assert_eq!(peer.electrical_connection, self.electrical_connection);

        self.appliance.attach(&mut self.manager, peer, self.now);
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.unit_engine.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while let Some(datagram) = self.unit_engine.poll_transmit() {
                self.manager.handle_datagram(&datagram, self.now);
                moved = true;
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.manager.poll_event()).collect();
            for event in &events {
                if let Some(event) = self.appliance.handle_event(event) {
                    self.events.push(event);
                    moved = true;
                }
            }
            let _: Vec<SpineEvent> =
                core::iter::from_fn(|| self.unit_engine.poll_event()).collect();
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    fn device(&self) -> eebus::model::AddressDevice {
        self.unit_engine.device().address().clone()
    }
}

/// The commissioning exchange, and then a value that arrives already legible.
#[test]
fn the_appliance_commissions_a_unit_and_then_reads_it() {
    let mut pair = Pair::new();
    pair.commission();

    assert!(
        pair.events
            .iter()
            .any(|e| matches!(e, MonitoringEvent::UnitDescribed { .. })),
        "the descriptions arrived"
    );

    let device = pair.device();
    // The unit measures something new and notifies its subscriber.
    pair.unit.set(&Measurand::total_power(), 2_300.0, pair.now);
    pair.unit
        .set(&Measurand::on(Quantity::Current, Phase::A), 3.5, pair.now);
    let measurement = pair.measurement.clone();
    pair.unit
        .notify(&mut pair.unit_engine, &measurement, pair.now);
    pair.settle();

    let readings = pair.appliance.readings(&device).expect("the unit");
    assert_eq!(readings.total_power(), Some(2_300.0));
    assert_eq!(
        readings.value(&Measurand::on(Quantity::Current, Phase::A)),
        Some(3.5),
        "3.5 A stays 3.5 A — the scale is not rounded away"
    );

    assert!(
        pair.events
            .iter()
            .any(|e| matches!(e, MonitoringEvent::Measured { .. })),
        "and the application was told"
    );
}

/// A reading whose description never arrived is dropped rather than guessed at.
#[test]
fn a_value_without_a_description_is_not_invented() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.device();

    let readings = pair.appliance.readings(&device).expect("the unit");
    assert!(
        readings
            .value(&Measurand::on(Quantity::Voltage, Phase::A))
            .is_none(),
        "the unit never described a voltage"
    );
}

/// The appliance never writes, so it never asks for a binding.
#[test]
fn the_appliance_takes_no_binding() {
    let mut pair = Pair::new();
    pair.commission();

    assert!(
        pair.unit_engine.relations().bindings().is_empty(),
        "a Monitoring Appliance only reads"
    );
    assert!(
        !pair.unit_engine.relations().subscriptions().is_empty(),
        "but it does subscribe (use-case implementation guide §3.2.2)"
    );
}

// ---- MGCP scenario 1: the one row that is not a measurement -----------------------

/// A Grid Connection Point that keeps the curtailment factor under its own `keyId`.
///
/// MGCP Table 23 spells the identifier `<k1#(1..1)>`, so the number is the device's. This
/// one has four other configuration keys and puts the factor on `4`, with a decoy on `1`
/// — a plausible `peakPowerOfPvSystem` in kilowatts, which read as a percentage is a
/// perfectly believable curtailment and a completely wrong export ceiling.
struct GridPair {
    manager: Engine,
    appliance: MonitoringApplianceActor,
    unit_engine: Engine,
    configuration: eebus::model::FeatureAddress,
    now: Duration,
    events: Vec<MonitoringEvent>,
}

/// Not 1, which is what this crate publishes for itself.
const THEIR_KEY: eebus::model::DeviceConfigurationKeyId = eebus::model::DeviceConfigurationKeyId(4);

impl GridPair {
    fn new() -> Self {
        use eebus::model::{
            CmdData, DeviceConfigurationKeyName, DeviceConfigurationKeyValueData,
            DeviceConfigurationKeyValueDescriptionData,
            DeviceConfigurationKeyValueDescriptionListData, DeviceConfigurationKeyValueListData,
            DeviceConfigurationKeyValueType, DeviceConfigurationKeyValueValue, ScaledNumber,
            UnitOfMeasurement,
        };

        let now = Duration::ZERO;
        let unit = MonitoredUnit::new(1)
            .naming(eebus::usecases::monitoring::Naming::GridConnectionPoint)
            .with(Measurand::total_power());

        let mut unit_device = LocalDevice::new("i:67890", "Meter-1", DeviceType::SubMeter).unwrap();
        unit_device
            .add_entity(
                LocalEntity::new([1], EntityType::GridConnectionPointOfPremises)
                    .with_feature(unit.electrical_connection_feature(1))
                    .with_feature(unit.measurement_feature(2))
                    .with_feature(eebus::usecases::mgcp::curtailment_feature(3)),
            )
            .unwrap();
        let electrical_connection = unit_device.address_of(&[1], 1);
        let measurement = unit_device.address_of(&[1], 2);
        let configuration = unit_device.address_of(&[1], 3);
        let mut unit_engine = Engine::new(unit_device);
        unit_engine.add_use_case_scenarios(
            [1],
            1,
            &eebus::usecases::mgcp::GRID_CONNECTION_POINT,
            &[1, 2],
        );
        unit.publish(&mut unit_engine, &electrical_connection, &measurement);

        let descriptions = CmdData::DeviceConfigurationKeyValueDescriptionListData(
            DeviceConfigurationKeyValueDescriptionListData {
                device_configuration_key_value_description_data: Some(vec![
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(eebus::model::DeviceConfigurationKeyId(1)),
                        key_name: Some(DeviceConfigurationKeyName::PeakPowerOfPvSystem),
                        value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
                        unit: Some(UnitOfMeasurement::W),
                        ..Default::default()
                    },
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(THEIR_KEY),
                        key_name: Some(DeviceConfigurationKeyName::PvCurtailmentLimitFactor),
                        value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
                        unit: Some(UnitOfMeasurement::Pct),
                        ..Default::default()
                    },
                ]),
            },
        );
        let values =
            CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
                device_configuration_key_value_data: Some(vec![DeviceConfigurationKeyValueData {
                    key_id: Some(eebus::model::DeviceConfigurationKeyId(1)),
                    value: Some(DeviceConfigurationKeyValueValue {
                        scaled_number: Some(ScaledNumber::from_f64(12_000.0, 0)),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
            });
        let feature = unit_engine
            .device_mut()
            .resolve_mut(&configuration)
            .expect("the feature");
        feature.set_data(descriptions).expect("publishable");
        feature.set_data(values).expect("publishable");

        let mut manager_device = LocalDevice::new(
            "i:12345",
            "EnergyManager-1",
            DeviceType::EnergyManagementSystem,
        )
        .unwrap();
        manager_device
            .add_entity(
                LocalEntity::new([1], EntityType::CEM).with_feature(LocalFeature::new(
                    1,
                    FeatureType::Generic,
                    Role::Client,
                )),
            )
            .unwrap();
        let client = manager_device.address_of(&[1], 1);
        let mut manager = Engine::new(manager_device);
        manager.add_use_case([1], 1, &eebus::usecases::mgcp::MONITORING_APPLIANCE);

        Self {
            manager,
            appliance: MonitoringApplianceActor::new(client),
            unit_engine,
            configuration,
            now,
            events: Vec::new(),
        }
    }

    fn commission(&mut self) {
        let manager_nm = node_management(self.manager.device().address());
        let unit_nm = node_management(self.unit_engine.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager.read(&unit_nm, &manager_nm, function, self.now);
        }
        self.settle();

        let device = self.unit_engine.device().address().clone();
        let remote = self.manager.peer(&device).expect("the connection point");
        let peer = monitoring::locate(
            remote,
            "monitoringOfGridConnectionPoint",
            actors::GRID_CONNECTION_POINT,
        )
        .expect("a Grid Connection Point");
        assert_eq!(
            peer.curtailment.as_ref(),
            Some(&self.configuration),
            "discovery finds the feature scenario 1 is served from"
        );
        self.appliance.attach(&mut self.manager, peer, self.now);
        self.settle();
    }

    /// The connection point publishes a new factor and notifies its subscribers.
    fn report(&mut self, percent: f64) {
        use eebus::model::{
            CmdData, DeviceConfigurationKeyValueData, DeviceConfigurationKeyValueListData,
            DeviceConfigurationKeyValueValue, ScaledNumber,
        };
        let data =
            CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
                device_configuration_key_value_data: Some(vec![DeviceConfigurationKeyValueData {
                    key_id: Some(THEIR_KEY),
                    value: Some(DeviceConfigurationKeyValueValue {
                        scaled_number: Some(ScaledNumber::from_f64(percent, 2)),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
            });
        self.unit_engine
            .device_mut()
            .resolve_mut(&self.configuration)
            .expect("the feature")
            .set_data(data)
            .expect("publishable");
        let feature = self.configuration.clone();
        self.unit_engine.notify(
            &feature,
            &Function::DeviceConfigurationKeyValueListData,
            self.now,
        );
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.unit_engine.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while let Some(datagram) = self.unit_engine.poll_transmit() {
                self.manager.handle_datagram(&datagram, self.now);
                moved = true;
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.manager.poll_event()).collect();
            for event in &events {
                if let Some(event) = self.appliance.handle_event(event) {
                    self.events.push(event);
                    moved = true;
                }
            }
            let _: Vec<SpineEvent> =
                core::iter::from_fn(|| self.unit_engine.poll_event()).collect();
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    fn device(&self) -> eebus::model::AddressDevice {
        self.unit_engine.device().address().clone()
    }
}

/// [MGCP-011]: the appliance reads the factor from the key the peer named, and turns it
/// into the watts a §9 installation acts on.
#[test]
fn the_curtailment_factor_reaches_the_appliance_under_the_peers_own_key() {
    let mut pair = GridPair::new();
    pair.commission();
    let device = pair.device();

    assert_eq!(
        pair.appliance.curtailment(&device),
        None,
        "no factor has been published yet, and an unread factor is not zero"
    );

    pair.report(70.0);

    assert!(
        pair.events.iter().any(|e| matches!(
            e,
            MonitoringEvent::CurtailmentChanged { factor_percent, .. }
                if (*factor_percent - 70.0).abs() < f64::EPSILON
        )),
        "the change is reported: {:?}",
        pair.events
    );
    assert_eq!(pair.appliance.curtailment(&device), Some(70.0));
    assert_eq!(
        pair.appliance
            .feed_in_limit(&device, 12_000.0)
            .map(|limit| limit.watts()),
        Some(8_400.0),
        "70 % of a 12 kWp array"
    );
}

/// The decoy on key 1 is never read as the factor.
///
/// This is the whole point of resolving the identifier: `peakPowerOfPvSystem` is 12000,
/// which clamps to a perfectly plausible 100 % and would silently lift the ceiling.
#[test]
fn another_key_is_never_mistaken_for_the_factor() {
    let mut pair = GridPair::new();
    pair.commission();
    let device = pair.device();

    // Key 1 was published and notified during commissioning; the factor was not.
    assert_eq!(
        pair.appliance.curtailment(&device),
        None,
        "key 1 holds 12 kW of peak power, not a percentage"
    );
    assert!(
        !pair
            .events
            .iter()
            .any(|e| matches!(e, MonitoringEvent::CurtailmentChanged { .. })),
        "and nothing was reported: {:?}",
        pair.events
    );
}
