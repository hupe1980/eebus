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
