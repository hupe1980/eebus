//! The generation-and-storage family, over the SPINE engine.
//!
//! An inverter with a PV string and a battery behind it, and an energy manager reading all
//! three and controlling the last. What this checks that the unit tests cannot is the part
//! that only exists once the entities are nested the way the hardware is: that a manager
//! can tell one string from another, that a battery's DC measurements arrive without
//! phases, and that a setpoint written over the wire reaches the state machine.

use core::time::Duration;

use eebus::model::{CmdData, DeviceType, EntityType, Function};
use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::cob::{
    self, BatteryControl, CobConfig, CobState, ControlMode, EffectiveControl, InverterKind,
    SetpointWrite,
};
use eebus::usecases::monitoring::{Measurand, Quantity, Readings};
use eebus::usecases::{mob, moi, mps};

/// An inverter with two strings and a battery, and the manager that reads it.
struct Site {
    inverter: Engine,
    manager: Engine,
    now: Duration,
}

impl Site {
    fn new() -> Self {
        let mut device = LocalDevice::new("i:46925", "Inverter-1", DeviceType::Inverter).unwrap();
        device
            .add_entity(
                LocalEntity::new([1], EntityType::Inverter)
                    .with_feature(cob::setpoint_feature(1))
                    .with_feature(cob::device_configuration_feature(2))
                    .with_feature(cob::device_diagnosis_feature(3)),
            )
            .unwrap();
        // The strings and the battery hang underneath it, as the specifications describe.
        for (path, kind) in [
            ([1, 1], EntityType::PVString),
            ([1, 2], EntityType::PVString),
            ([1, 3], EntityType::Battery),
        ] {
            device.add_entity(LocalEntity::new(path, kind)).unwrap();
        }

        let mut inverter = Engine::new(device);
        inverter.add_use_case([1], 1, &cob::INVERTER);
        inverter.add_use_case([1], 0, &moi::INVERTER);
        inverter.add_use_case([1, 1], 0, &mps::PV_STRING);
        inverter.add_use_case([1, 2], 0, &mps::PV_STRING);
        inverter.add_use_case([1, 3], 0, &mob::BATTERY);

        let mut manager_device =
            LocalDevice::new("i:46925", "CEM-1", DeviceType::EnergyManagementSystem).unwrap();
        manager_device
            .add_entity(LocalEntity::new([1], EntityType::CEM))
            .unwrap();
        let mut manager = Engine::new(manager_device);
        manager.add_use_case([1], 0, &cob::CEM);
        manager.add_use_case([1], 0, &moi::MONITORING_APPLIANCE);

        Self {
            inverter,
            manager,
            now: Duration::ZERO,
        }
    }

    fn discover(&mut self) {
        let inverter_nm = node_management(self.inverter.device().address());
        let manager_nm = node_management(self.manager.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager
                .read(&inverter_nm, &manager_nm, function.clone(), self.now);
            self.inverter
                .read(&manager_nm, &inverter_nm, function, self.now);
        }
        self.settle();
    }

    fn settle(&mut self) -> Vec<SpineEvent> {
        let mut events = Vec::new();
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.inverter.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while let Some(datagram) = self.inverter.poll_transmit() {
                self.manager.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while let Some(event) = self.inverter.poll_event() {
                events.push(event);
                moved = true;
            }
            while self.manager.poll_event().is_some() {
                moved = true;
            }
            if !moved {
                return events;
            }
        }
        panic!("the exchange did not settle");
    }
}

/// The manager finds every actor the inverter plays, each on the entity the specification
/// puts it on — which is what lets it tell the south string from the east one.
#[test]
fn a_manager_finds_every_actor_the_inverter_plays() {
    let mut site = Site::new();
    site.discover();

    let device = site.inverter.device().address().clone();
    let remote = site.manager.peer(&device).expect("the inverter");

    assert!(
        remote.use_case(moi::NAME, moi::INVERTER_ACTOR).is_some(),
        "the inverter itself"
    );
    assert!(
        remote.use_case(mob::NAME, mob::BATTERY_ACTOR).is_some(),
        "the battery behind it"
    );
    assert!(
        remote.use_case(cob::NAME, cob::INVERTER_ACTOR).is_some(),
        "and the control use case"
    );

    // Two strings, announced on two entities and told apart by the address each use case
    // is anchored on. That is the point of MPS: a shaded roof is invisible in a building's
    // total and obvious in its own string.
    let strings: Vec<_> = remote
        .use_cases
        .iter()
        .filter(|uc| uc.name.as_str() == mps::NAME && uc.actor.as_str() == mps::PV_STRING_ACTOR)
        .collect();
    assert_eq!(strings.len(), 2);
    let entity_of = |uc: &&eebus::spine::RemoteUseCase| {
        uc.address
            .as_ref()
            .and_then(|a| a.entity.clone())
            .expect("this crate always anchors a use case on an entity")
    };
    assert_ne!(
        entity_of(&strings[0]),
        entity_of(&strings[1]),
        "each on its own entity, or the manager could not tell them apart"
    );
}

/// A battery's DC measurements arrive with no phases at all, and a reader that waited for
/// `acMeasuredPhases` would never describe them.
#[test]
fn a_batterys_measurements_arrive_without_phases() {
    let mut battery = mob::monitored_unit(1)
        .with(Measurand::unphased(Quantity::DcPower))
        .with(Measurand::unphased(Quantity::StateOfCharge))
        .with(Measurand::unphased(Quantity::StateOfEnergy));
    battery.set(&Measurand::unphased(Quantity::DcPower), -3_200.0);
    battery.set(&Measurand::unphased(Quantity::StateOfCharge), 61.0);
    battery.set(&Measurand::unphased(Quantity::StateOfEnergy), 12_200.0);

    // The manager reads the descriptions and then the values, in that order.
    let mut readings = Readings::new();
    readings.describe(&battery.measurement_descriptions());
    readings.describe(&battery.parameter_descriptions());
    readings.apply(&battery.measurements());

    assert_eq!(
        readings.value(&Measurand::unphased(Quantity::DcPower)),
        Some(-3_200.0),
        "negative: discharging, passive sign convention"
    );
    assert_eq!(
        readings.value(&Measurand::unphased(Quantity::StateOfCharge)),
        Some(61.0)
    );
    assert_eq!(
        readings.value(&Measurand::unphased(Quantity::StateOfEnergy)),
        Some(12_200.0),
        "watt-hours, not a percentage — 12 200 % would be nonsense"
    );

    let CmdData::ElectricalConnectionParameterDescriptionListData(list) =
        battery.parameter_descriptions()
    else {
        unreachable!("built above");
    };
    for entry in list
        .electrical_connection_parameter_description_data
        .iter()
        .flatten()
    {
        assert_eq!(entry.ac_measured_phases, None);
    }
}

/// A setpoint written over the wire reaches the state machine, and the acknowledgement is
/// the inverter's own answer.
#[test]
fn a_setpoint_written_over_the_wire_reaches_the_state_machine() {
    let mut site = Site::new();
    site.discover();

    let mut control = BatteryControl::new(
        CobConfig::new(InverterKind::Hybrid, Duration::from_secs(2 * 3_600))
            .with_failsafe(0.0)
            .with_default(0.0),
        Duration::ZERO,
    );
    control.on_heartbeat(Duration::from_secs(10));
    control.on_control_mode(ControlMode::Power, true, Duration::from_secs(11));
    assert_eq!(control.state(), CobState::PowerControl);

    // What the manager puts on the wire, and what the inverter reads back out of it.
    let payload = CmdData::SetpointListData(eebus::model::SetpointListData {
        setpoint_data: Some(alloc_vec(-2_500.0)),
    });
    let write = cob::read_setpoint_write(&payload).expect("a setpoint");
    assert_eq!(write.watts, -2_500.0);

    let outcome = control.on_setpoint(&write, true, Duration::from_secs(12));
    assert!(outcome.is_accepted());
    assert_eq!(control.effective(), EffectiveControl::Setpoint(-2_500.0));

    // And what the inverter publishes back, which is what the manager reads to confirm.
    let published = cob::setpoint_data(&control);
    let read_back = cob::read_setpoint_write(&published).expect("a setpoint");
    assert_eq!(read_back.watts, -2_500.0);
    assert!(read_back.is_active);
}

fn alloc_vec(watts: f64) -> Vec<eebus::model::SetpointData> {
    vec![eebus::model::SetpointData {
        setpoint_id: Some(cob::SETPOINT_ID),
        is_setpoint_active: Some(true),
        value: Some(eebus::model::ScaledNumber::from_f64(watts, 0)),
        ..Default::default()
    }]
}

/// The whole §COB day, against a virtual clock: control established, a setpoint applied,
/// the manager falls silent, the failsafe takes over, and the inverter eventually runs on
/// its own.
#[test]
fn the_whole_cob_day_runs_against_a_virtual_clock() {
    let mut inverter = BatteryControl::new(
        CobConfig::new(InverterKind::Battery, Duration::from_secs(2 * 3_600))
            .with_failsafe(0.0)
            .with_default(-500.0),
        Duration::ZERO,
    );

    let mut now = Duration::from_secs(10);
    inverter.on_heartbeat(now);
    now += Duration::from_secs(1);
    inverter.on_control_mode(ControlMode::Power, true, now);
    assert_eq!(inverter.effective(), EffectiveControl::Default(-500.0));

    now += Duration::from_secs(1);
    inverter.on_setpoint(&SetpointWrite::active(4_000.0), true, now);
    assert_eq!(inverter.effective(), EffectiveControl::Setpoint(4_000.0));

    // The manager keeps beating for a while.
    for _ in 0..5 {
        now += Duration::from_secs(60);
        inverter.on_heartbeat(now);
        inverter.handle_timeout(now);
        assert_eq!(inverter.state(), CobState::PowerControl);
    }

    // Then it stops. Two minutes later the failsafe applies.
    inverter.handle_timeout(now + cob::HEARTBEAT_TIMEOUT);
    assert_eq!(inverter.state(), CobState::Failsafe);
    assert_eq!(inverter.effective(), EffectiveControl::Failsafe(0.0));

    // And two hours after that, the inverter gives up on it.
    let entered = now + cob::HEARTBEAT_TIMEOUT;
    inverter.handle_timeout(entered + Duration::from_secs(2 * 3_600));
    assert_eq!(inverter.state(), CobState::AutoUncontrolled);
    assert_eq!(inverter.effective(), EffectiveControl::Autonomous);
    assert_eq!(
        inverter.poll_timeout(),
        None,
        "nothing left to wait for, which is the invariant a caller's loop rests on"
    );
}
