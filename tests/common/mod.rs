//! A control box and a heat pump, wired to each other in memory.
//!
//! Shared by `limitation_both_actors.rs` and `conformance.rs`: both need two real actors
//! exchanging real datagrams over a virtual clock, and a second copy of the harness would
//! be a second thing to keep in step with the crate.
//!
//! Every datagram makes the round trip through the wire format on the way across, so what
//! is under test is the encoding as well as the logic.
#![allow(dead_code)]

use core::time::Duration;

use eebus::model::ElectricalConnectionPhaseName as Phase;
use eebus::model::{Datagram, DeviceType, EntityType, FeatureAddress, FeatureType, Function, Role};
use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature, SpineEvent, node_management};
use eebus::usecases::descriptor::UseCaseDescriptor;
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, CsConfig, CsEvent, CsFeatures,
    EnergyGuardActor, GuardEvent, LimitWrite,
};
use eebus::usecases::lpc;
use eebus::usecases::monitoring::{Measurand, MonitoredUnit, Quantity, Readings};
use eebus::usecases::{mgcp, mpc};

pub const FAILSAFE_WATTS: f64 = 4_200.0;
pub const NOMINAL_MAX_WATTS: f64 = 11_000.0;

/// A control box and a heat pump, each driven by its own actor.
pub struct Pair {
    pub guard_engine: Engine,
    pub guard: EnergyGuardActor,
    pub pump_engine: Engine,
    pub pump: ControllableSystemActor,
    pub now: Duration,
    /// Everything the Energy Guard has reported.
    pub reports: Vec<GuardEvent>,
    /// Everything the Controllable System has reported.
    pub decisions: Vec<CsEvent>,
    /// Every datagram that crossed, in order, with the direction it went.
    ///
    /// `"box"` for the Energy Guard's, `"pump"` for the Controllable System's. Filled by
    /// [`Pair::settle`], and what `golden_vectors.rs` compares against a checked-in file.
    pub trace: Vec<(&'static str, Datagram)>,
    /// While set, everything the Controllable System sends is thrown away.
    ///
    /// An unresponsive peer, as SPINE IG §2.6.1 means it: the datagrams the guard sends
    /// arrive, and nothing comes back. The guard cannot tell this from a peer that has
    /// been switched off mid-sentence, which is the point — §2.6.2's escalation path is
    /// what a client has instead of knowing.
    pub pump_is_deaf: bool,
}

impl Pair {
    pub fn new() -> Self {
        let now = Duration::ZERO;

        let mut guard_device = LocalDevice::new(
            "i:12345",
            "ControlBox-1",
            DeviceType::ElectricitySupplySystem,
        )
        .unwrap();
        guard_device
            .add_entity(
                LocalEntity::new([1], EntityType::GridGuard)
                    .with_feature(limitation::client_feature(1))
                    .with_feature(limitation::device_diagnosis_feature(2)),
            )
            .unwrap();
        let client = guard_device.address_of(&[1], 1);
        let guard_diagnosis = guard_device.address_of(&[1], 2);
        let mut guard_engine = Engine::new(guard_device);
        guard_engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);
        let guard = EnergyGuardActor::new(lpc::DIRECTION, client, guard_diagnosis, now);

        let mut pump_device =
            LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        pump_device
            .add_entity(
                LocalEntity::new([1], EntityType::HeatPumpAppliance)
                    .with_feature(limitation::load_control_feature(1))
                    .with_feature(limitation::device_configuration_feature(2))
                    .with_feature(limitation::device_diagnosis_feature(3))
                    .with_feature(limitation::device_diagnosis_client_feature(5))
                    .with_feature(limitation::electrical_connection_feature(4)),
            )
            .unwrap();
        let load_control = pump_device.address_of(&[1], 1);
        let configuration = pump_device.address_of(&[1], 2);
        let diagnosis = pump_device.address_of(&[1], 3);
        let diagnosis_client = pump_device.address_of(&[1], 5);
        let electrical = pump_device.address_of(&[1], 4);
        let mut pump_engine = Engine::new(pump_device);
        pump_engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);
        let pump = ControllableSystemActor::builder(
            ControllableSystem::new(
                CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600))
                    .with_nominal_max(NOMINAL_MAX_WATTS),
                now,
            ),
            lpc::DIRECTION,
            CsFeatures {
                load_control,
                device_configuration: configuration,
                device_diagnosis: diagnosis,
                device_diagnosis_client: diagnosis_client,
            },
        )
        .with_electrical_connection(electrical)
        .install(&mut pump_engine, now);

        Self {
            guard_engine,
            guard,
            pump_engine,
            pump,
            now,
            reports: Vec::new(),
            decisions: Vec::new(),
            trace: Vec::new(),
            pump_is_deaf: false,
        }
    }

    /// Discovery both ways, and the Energy Guard taking control.
    pub fn commission(&mut self) {
        let guard_nm = node_management(self.guard_engine.device().address());
        let pump_nm = node_management(self.pump_engine.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.guard_engine
                .read(&pump_nm, &guard_nm, function.clone(), self.now);
            self.pump_engine
                .read(&guard_nm, &pump_nm, function, self.now);
        }
        self.settle();

        // No `attach` here: the guard attaches itself to a peer that announces the
        // Controllable System, which is what implementation guide §2.11 needs — the
        // opening limit write has to go out as soon as the bindings settle, whether or
        // not anybody has asked for a limit.
        let device = self.pump_engine.device().address().clone();
        let remote = self.guard_engine.peer(&device).expect("the heat pump");
        limitation::locate(remote, lpc::DIRECTION).expect("a Controllable System");
        self.settle();
    }

    /// Carries datagrams both ways until neither side has anything left to say.
    pub fn settle(&mut self) {
        for _ in 0..128 {
            let mut moved = false;
            while let Some(datagram) = self.guard_engine.poll_transmit() {
                self.trace.push(("box", datagram.clone()));
                self.pump_engine
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.pump_engine.poll_transmit() {
                self.trace.push(("pump", datagram.clone()));
                if self.pump_is_deaf {
                    moved = true;
                    continue;
                }
                self.guard_engine
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.pump_engine.poll_event()).collect();
            for event in &events {
                if let Some(decision) =
                    self.pump
                        .handle_event(&mut self.pump_engine, event, self.now)
                {
                    self.decisions.push(decision);
                    moved = true;
                }
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.guard_engine.poll_event()).collect();
            for event in &events {
                if let Some(report) =
                    self.guard
                        .handle_event(&mut self.guard_engine, event, self.now)
                {
                    self.reports.push(report);
                    moved = true;
                }
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    /// Fires both actors' timers, then settles.
    pub fn advance(&mut self, by: Duration) {
        self.now += by;
        let reports = self.guard.handle_timeout(&mut self.guard_engine, self.now);
        self.reports.extend(reports);
        if let Some(decision) = self.pump.handle_timeout(&mut self.pump_engine, self.now) {
            self.decisions.push(decision);
        }
        self.guard_engine.handle_timeout(self.now);
        self.pump_engine.handle_timeout(self.now);
        self.settle();
    }

    pub fn device(&self) -> eebus::model::AddressDevice {
        self.pump_engine.device().address().clone()
    }

    pub fn guard_client(&self) -> eebus::model::FeatureAddress {
        self.guard_engine.device().address_of(&[1], 1)
    }

    pub fn pump_load_control(&self) -> eebus::model::FeatureAddress {
        self.pump_engine.device().address_of(&[1], 1)
    }

    /// Drops what has been reported so far, so that a test's counts are about what the
    /// test did.
    ///
    /// Commissioning is not silent: implementation guide §2.11 makes the Energy Guard owe
    /// an opening write on the limit as soon as it is bound, whether or not the
    /// application has decided anything, so a freshly commissioned pair already has one
    /// accepted deactivation behind it.
    pub fn forget_history(&mut self) {
        self.reports.clear();
        self.decisions.clear();
    }

    /// The datagrams that have crossed, as they appeared on the wire.
    pub fn wire(&self) -> Vec<String> {
        self.trace
            .iter()
            .map(|(from, datagram)| {
                let json = eebus::model::to_json(datagram).expect("encodable");
                alloc_format(from, &json)
            })
            .collect()
    }

    pub fn accepted(&self) -> Vec<LimitWrite> {
        self.reports
            .iter()
            .filter_map(|r| match r {
                GuardEvent::LimitAccepted { limit, .. } => Some(*limit),
                _ => None,
            })
            .collect()
    }

    pub fn refused(&self) -> Vec<LimitWrite> {
        self.reports
            .iter()
            .filter_map(|r| match r {
                GuardEvent::LimitRefused { limit, .. } => Some(*limit),
                _ => None,
            })
            .collect()
    }
}

pub fn round_trip(datagram: &Datagram) -> Datagram {
    let wire = eebus::model::to_json(datagram).expect("encode");
    let decoded = eebus::model::from_json_str(&wire).expect("decode");
    assert_eq!(&decoded, datagram, "the datagram survives the wire");
    decoded
}
/// One traced datagram as a line: who sent it, and exactly what went out.
///
/// The column is four wide because every label is: `box`/`pump` for the limitation pair,
/// `cem`/`unit` for the monitoring one. Widening it would re-indent every line of every
/// recorded exchange, and a golden vector whose diff is mostly whitespace is a golden
/// vector nobody reads.
fn alloc_format(from: &str, json: &str) -> String {
    format!("{from:>4} | {json}")
}

// ---- MPC / MGCP: a monitored unit and the appliance reading it --------------------
//
// Shared by `monitoring_over_the_wire.rs` and `conformance.rs`. The second is the reason
// this moved here: driving 101 abstract test cases needs the same commissioning sequence
// the ordinary tests use, and a second copy of it would be a second thing to keep in step.

pub struct MonitoringPair {
    pub manager: Engine,
    pub unit_engine: Engine,
    pub unit: MonitoredUnit,
    pub readings: Readings,
    /// MGCP scenario 1, as the *manager* has resolved it.
    ///
    /// The factor's `keyId` is the connection point's own, so this holds the description
    /// beside the value exactly as a real Monitoring Appliance has to.
    pub curtailment: mgcp::Curtailment,
    /// The unit's `DeviceConfiguration` feature, where it serves scenario 1.
    pub curtailment_feature: Option<FeatureAddress>,
    pub now: Duration,
    /// Every datagram that crossed, in order, for the golden vector.
    pub trace: Vec<(&'static str, Datagram)>,
}

impl MonitoringPair {
    /// An energy manager and a heat pump running MPC.
    pub fn consuming() -> Self {
        let unit = MonitoredUnit::new(1)
            .with(Measurand::total_power())
            .with(Measurand::on(Quantity::Power, Phase::A))
            .with(Measurand::total(Quantity::EnergyConsumed))
            .with(Measurand::on(Quantity::Current, Phase::A))
            .with(Measurand::total(Quantity::Frequency));
        // Power, energy, current and frequency — everything but voltage.
        Self::build(
            unit,
            EntityType::HeatPumpAppliance,
            &mpc::MONITORED_UNIT,
            &mpc::MONITORING_APPLIANCE,
            &[1, 2, 3, 5],
        )
    }

    /// An energy manager and a grid connection point running MGCP.
    pub fn at_the_grid() -> Self {
        let unit = MonitoredUnit::new(1)
            .naming(mgcp::NAMING)
            .with(Measurand::total_power())
            .with(Measurand::total(Quantity::EnergyConsumed))
            .with(Measurand::total(Quantity::EnergyProduced));
        // Power and the two energies: scenarios 2, 3 and 4.
        Self::build(
            unit,
            EntityType::GridConnectionPointOfPremises,
            &mgcp::GRID_CONNECTION_POINT,
            &mgcp::MONITORING_APPLIANCE,
            &[2, 3, 4],
        )
    }

    pub fn build(
        unit: MonitoredUnit,
        entity: EntityType,
        server: &'static UseCaseDescriptor,
        client: &'static UseCaseDescriptor,
        scenarios: &[u32],
    ) -> Self {
        Self::build_with(unit, entity, server, client, scenarios, false)
    }

    /// The same, with MGCP scenario 1's PV curtailment factor served from feature 3.
    ///
    /// It is a `DeviceConfiguration` key rather than a measurement, which is why it needs
    /// a feature of its own and why it is the one row of the MGCP catalogue that does not
    /// go through the measurement layer.
    pub fn build_with(
        unit: MonitoredUnit,
        entity: EntityType,
        server: &'static UseCaseDescriptor,
        client: &'static UseCaseDescriptor,
        scenarios: &[u32],
        curtailment: bool,
    ) -> Self {
        assert!(
            server.permits_entity(&entity),
            "the specification permits this actor on this entity"
        );

        let mut device = LocalDevice::new("i:67890", "Meter-1", DeviceType::SubMeter).unwrap();
        let mut local = LocalEntity::new([1], entity)
            .with_feature(unit.electrical_connection_feature(1))
            .with_feature(unit.measurement_feature(2));
        if curtailment {
            local = local.with_feature(mgcp::curtailment_feature(3));
        }
        device.add_entity(local).unwrap();
        let electrical_connection = device.address_of(&[1], 1);
        let measurement = device.address_of(&[1], 2);

        let mut unit_engine = Engine::new(device);
        // The device announces every scenario it actually implements, not only the ones
        // the specification refuses to leave optional.
        unit_engine.add_use_case_scenarios([1], 1, server, scenarios);
        unit.publish(&mut unit_engine, &electrical_connection, &measurement);
        if curtailment {
            let feature = unit_engine.device().address_of(&[1], 3);
            if let Some(f) = unit_engine.device_mut().resolve_mut(&feature) {
                let _ = f.set_data(mgcp::curtailment_description());
            }
        }

        let mut manager_device =
            LocalDevice::new("i:12345", "Manager-1", DeviceType::EnergyManagementSystem).unwrap();
        manager_device
            .add_entity(
                LocalEntity::new([1], EntityType::CEM).with_feature(LocalFeature::new(
                    1,
                    FeatureType::Generic,
                    Role::Client,
                )),
            )
            .unwrap();
        let mut manager = Engine::new(manager_device);
        manager.add_use_case([1], 1, client);

        let curtailment_feature = curtailment.then(|| unit_engine.device().address_of(&[1], 3));

        Self {
            manager,
            unit_engine,
            unit,
            readings: Readings::new(),
            curtailment: mgcp::Curtailment::new(),
            curtailment_feature,
            now: Duration::ZERO,
            trace: Vec::new(),
        }
    }

    /// The recorded exchange, one line per datagram.
    pub fn wire(&self) -> Vec<String> {
        self.trace
            .iter()
            .map(|(from, datagram)| {
                let json = eebus::model::to_json(datagram).expect("encodable");
                alloc_format(from, &json)
            })
            .collect()
    }

    pub fn manager_client(&self) -> FeatureAddress {
        self.manager.device().address_of(&[1], 1)
    }

    pub fn unit_feature(&self, feature: u32) -> FeatureAddress {
        self.unit_engine.device().address_of(&[1], feature)
    }

    /// Carries datagrams both ways, resolving everything the manager receives.
    pub fn exchange(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.trace.push(("cem", datagram.clone()));
                self.unit_engine
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.unit_engine.poll_transmit() {
                self.trace.push(("unit", datagram.clone()));
                self.manager
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(event) = self.manager.poll_event() {
                // `resolved`, not `data`: what a peer notified may be a fragment, and
                // this harness stands in for a Monitoring Appliance, which reads the
                // merged value.
                if let SpineEvent::ReplyReceived {
                    feature, resolved, ..
                }
                | SpineEvent::DataNotified {
                    feature, resolved, ..
                } = &event
                {
                    if self.curtailment_feature.as_ref() == Some(feature) {
                        // Scenario 1 is a `DeviceConfiguration` key, not a measurement,
                        // and the manager learns its identifier from the description the
                        // connection point published — never from this crate's own.
                        if !self.curtailment.describe(resolved) {
                            self.curtailment.apply(resolved);
                        }
                    } else {
                        self.readings.describe(resolved);
                        self.readings.apply(resolved);
                    }
                }
                moved = true;
            }
            while self.unit_engine.poll_event().is_some() {
                moved = true;
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    /// Discovery, then the two descriptions and a subscription, as §3.3 asks.
    pub fn commission(&mut self) {
        let client_nm = node_management(self.manager.device().address());
        let unit_nm = node_management(self.unit_engine.device().address());
        self.manager.read(
            &unit_nm,
            &client_nm,
            Function::NodeManagementDetailedDiscoveryData,
            self.now,
        );
        self.manager.read(
            &unit_nm,
            &client_nm,
            Function::NodeManagementUseCaseData,
            self.now,
        );
        self.exchange();

        let client = self.manager_client();
        let electrical_connection = self.unit_feature(1);
        let measurement = self.unit_feature(2);
        self.manager.read(
            &electrical_connection,
            &client,
            Function::ElectricalConnectionParameterDescriptionListData,
            self.now,
        );
        self.manager.read(
            &measurement,
            &client,
            Function::MeasurementDescriptionListData,
            self.now,
        );
        self.manager
            .request_subscription(&client, &measurement, self.now);
        // MGCP scenario 1, where the connection point serves it. Both functions and a
        // subscription, which is what a Monitoring Appliance actually does — the previous
        // harness read the factor out of the *unit's* own store, so nothing about
        // scenario 1 ever crossed the wire and every `ATC_MGCP_SCE1_…` case was scored on
        // an exchange that did not happen.
        if let Some(configuration) = self.curtailment_feature.clone() {
            for function in [
                Function::DeviceConfigurationKeyValueDescriptionListData,
                Function::DeviceConfigurationKeyValueListData,
            ] {
                self.manager
                    .read(&configuration, &client, function, self.now);
            }
            self.manager
                .request_subscription(&client, &configuration, self.now);
        }
        self.exchange();
    }

    /// The grid connection point publishes MGCP scenario 1's curtailment factor.
    pub fn report_curtailment(&mut self, percent: f64) {
        let feature = self.unit_engine.device().address_of(&[1], 3);
        if let Some(f) = self.unit_engine.device_mut().resolve_mut(&feature) {
            let _ = f.set_data(mgcp::curtailment_value(percent));
        }
        self.unit_engine.notify(
            &feature,
            &Function::DeviceConfigurationKeyValueListData,
            self.now,
        );
        self.exchange();
    }

    /// The curtailment factor **the manager** holds, resolved from what crossed the wire.
    pub fn curtailment(&self) -> Option<f64> {
        self.curtailment.factor_percent()
    }

    /// The unit takes a reading and notifies its subscribers.
    pub fn report(&mut self, measurand: &Measurand, value: f64) {
        self.unit.set(measurand, value);
        let measurement = self.unit_feature(2);
        self.unit
            .notify(&mut self.unit_engine, &measurement, self.now);
        self.exchange();
    }
}
