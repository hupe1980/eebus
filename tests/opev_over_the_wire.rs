//! Overload Protection by EV Charging Current Curtailment, both actors, over SPINE.
//!
//! An energy manager and a car run the whole use case between them: discovery, the
//! binding, the band the car can charge in, a per-phase curtailment, and the four-second
//! watchdog that puts the car back on a safe current when the manager goes quiet.
//!
//! Everything travels as real datagrams against a virtual clock, so the four seconds cost
//! nothing to test — and neither does what happens on the fifth.

use core::time::Duration;

use eebus::model::{
    Datagram, DeviceType, ElectricalConnectionPhaseName as Phase, EntityType, FeatureType,
    Function, Role,
};
use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature, SpineEvent, node_management};
use eebus::usecases::emobility::charging::{
    self, ChargingCurrents, CurrentSource, EvActor, EvCharging, EvEvent, GuardEvent,
    OverloadGuardActor,
};
use eebus::usecases::emobility::opev;

/// What the car can charge at: six amperes minimum, sixteen maximum.
const RANGE: (f64, f64) = (6.0, 16.0);

struct Pair {
    manager: Engine,
    guard: OverloadGuardActor,
    car: Engine,
    ev: EvActor,
    now: Duration,
    guard_events: Vec<GuardEvent>,
    ev_events: Vec<EvEvent>,
}

impl Pair {
    fn new() -> Self {
        let now = Duration::ZERO;

        let mut manager_device = LocalDevice::new(
            "i:12345",
            "EnergyManager-1",
            DeviceType::EnergyManagementSystem,
        )
        .unwrap();
        manager_device
            .add_entity(
                LocalEntity::new([1], EntityType::CEM)
                    .with_feature(LocalFeature::new(1, FeatureType::Generic, Role::Client))
                    .with_feature(OverloadGuardActor::device_diagnosis_feature(2)),
            )
            .unwrap();
        let client = manager_device.address_of(&[1], 1);
        let diagnosis = manager_device.address_of(&[1], 2);
        let mut manager = Engine::new(manager_device);
        manager.add_use_case([1], 1, &opev::ENERGY_GUARD);
        let guard = OverloadGuardActor::new(client, diagnosis, now);

        // The car sits behind the wallbox, which is what the resource hierarchy of
        // §3.2.1.1 describes: the `EV` entity is a child of the `EVSE` entity.
        let mut car_device =
            LocalDevice::new("i:67890", "Wallbox-1", DeviceType::ChargingStation).unwrap();
        car_device
            .add_entity(LocalEntity::new([1], EntityType::EVSE))
            .unwrap();
        car_device
            .add_entity(
                LocalEntity::new([1, 1], EntityType::EV)
                    .with_feature(charging::load_control_feature(1))
                    .with_feature(charging::electrical_connection_feature(2)),
            )
            .unwrap();
        let load_control = car_device.address_of(&[1, 1], 1);
        let electrical_connection = car_device.address_of(&[1, 1], 2);
        let mut car = Engine::new(car_device);
        car.add_use_case([1, 1], 1, &opev::EV);

        let ev = EvActor::new(
            opev::PURPOSE,
            EvCharging::new(ChargingCurrents::same(RANGE.0), now).with_range(RANGE.0, RANGE.1),
            load_control,
            electrical_connection,
            charging::PHASES,
            RANGE,
        );
        ev.publish(&mut car, now);

        Self {
            manager,
            guard,
            car,
            ev,
            now,
            guard_events: Vec::new(),
            ev_events: Vec::new(),
        }
    }

    /// Discovery both ways, then the manager taking control and the car subscribing to
    /// the manager's heartbeat.
    fn commission(&mut self) {
        let manager_nm = node_management(self.manager.device().address());
        let car_nm = node_management(self.car.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager
                .read(&car_nm, &manager_nm, function.clone(), self.now);
            self.car.read(&manager_nm, &car_nm, function, self.now);
        }
        self.settle();

        let car_device = self.car.device().address().clone();
        let remote = self.manager.peer(&car_device).expect("the car");
        let peer = charging::locate(remote, opev::PURPOSE).expect("it plays the EV actor");
        self.guard.attach(&mut self.manager, peer, self.now);

        let manager_device = self.manager.device().address().clone();
        let remote = self.car.peer(&manager_device).expect("the manager");
        let guard_diagnosis =
            charging::locate_guard(remote, opev::PURPOSE).expect("its DeviceDiagnosis");
        self.ev.watch(&mut self.car, guard_diagnosis, self.now);
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..128 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.car.handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.car.poll_transmit() {
                self.manager
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            let events: Vec<SpineEvent> = core::iter::from_fn(|| self.car.poll_event()).collect();
            for event in &events {
                if let Some(event) = self.ev.handle_event(&mut self.car, event, self.now) {
                    self.ev_events.push(event);
                    moved = true;
                }
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.manager.poll_event()).collect();
            for event in &events {
                if let Some(event) = self.guard.handle_event(&mut self.manager, event, self.now) {
                    self.guard_events.push(event);
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
    fn advance(&mut self, by: Duration) {
        self.now += by;
        self.guard.handle_timeout(&mut self.manager, self.now);
        if let Some(event) = self.ev.handle_timeout(&mut self.car, self.now) {
            self.ev_events.push(event);
        }
        self.manager.handle_timeout(self.now);
        self.car.handle_timeout(self.now);
        self.settle();
    }

    fn car_device(&self) -> eebus::model::AddressDevice {
        self.car.device().address().clone()
    }

    fn accepted(&self) -> Vec<ChargingCurrents> {
        self.guard_events
            .iter()
            .filter_map(|e| match e {
                GuardEvent::Accepted { currents, .. } => Some(*currents),
                _ => None,
            })
            .collect()
    }
}

fn round_trip(datagram: &Datagram) -> Datagram {
    let wire = eebus::model::to_json(datagram).expect("encode");
    let decoded = eebus::model::from_json_str(&wire).expect("decode");
    assert_eq!(&decoded, datagram, "the datagram survives the wire");
    decoded
}

/// The whole use case: the manager learns what the car can take, then curtails it.
#[test]
fn the_manager_learns_the_band_and_then_curtails() {
    let mut pair = Pair::new();
    pair.commission();

    let device = pair.car_device();
    let ready = pair
        .guard_events
        .iter()
        .find_map(|e| match e {
            GuardEvent::Ready { band, .. } => Some(band.clone()),
            _ => None,
        })
        .expect("the binding and the band both arrived");
    assert_eq!(
        ready.narrowest(),
        Some(RANGE),
        "Table 9 round-tripped over the wire"
    );
    // Per phase, resolved through the car's own parameter descriptions.
    for phase in eebus::usecases::emobility::charging::PHASES {
        assert_eq!(ready.for_phase(&phase), Some(RANGE));
    }
    assert_eq!(
        pair.guard.band_of(&device).and_then(|b| b.narrowest()),
        Some(RANGE)
    );

    // [OPEV-002]: a different current on each phase.
    pair.guard
        .require(&device, ChargingCurrents::new(16.0, 10.0, 6.0));
    pair.advance(Duration::from_secs(1));

    assert_eq!(pair.accepted().len(), 1);
    assert_eq!(pair.ev.charging().source(), CurrentSource::Curtailed);
    assert_eq!(pair.ev.charging().effective(Phase::A), Some(16.0));
    assert_eq!(pair.ev.charging().effective(Phase::B), Some(10.0));
    assert_eq!(pair.ev.charging().effective(Phase::C), Some(6.0));
}

/// [OPEV-005]: four seconds of silence and the car is back on its safe current.
///
/// The number is the point. LPC gives a heat pump two minutes because its compressor
/// cannot be asked for a change sooner; a car's pilot signal changes at once, and the
/// fuse it protects does not wait.
#[test]
fn opev_005_four_seconds_of_silence_puts_the_car_back_on_its_safe_current() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.car_device();

    pair.guard.require(&device, ChargingCurrents::same(16.0));
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.ev.charging().effective(Phase::A), Some(16.0));

    // The manager stops beating. Nothing is signalled; it simply goes quiet.
    pair.now += Duration::from_secs(5);
    pair.ev.handle_timeout(&mut pair.car, pair.now);

    assert_eq!(pair.ev.charging().source(), CurrentSource::GuardSilent);
    assert_eq!(
        pair.ev.charging().effective(Phase::A),
        Some(RANGE.0),
        "back to six amperes, which cannot overload anything"
    );

    // And the moment it comes back, so does the curtailment.
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.ev.charging().source(), CurrentSource::Curtailed);
    assert_eq!(pair.ev.charging().effective(Phase::A), Some(16.0));
}

/// [OPEV-007]: a manager that says it has failed is not trusted, even while it beats.
#[test]
fn opev_007_a_failed_manager_is_not_relied_on() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.car_device();

    pair.guard.require(&device, ChargingCurrents::same(16.0));
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.ev.charging().effective(Phase::A), Some(16.0));

    pair.guard.set_failed(&mut pair.manager, true, pair.now);
    pair.advance(Duration::from_secs(1));

    assert_eq!(pair.ev.charging().source(), CurrentSource::GuardFailed);
    assert_eq!(
        pair.ev.charging().effective(Phase::A),
        Some(RANGE.0),
        "the heartbeat is still arriving and is still not enough"
    );

    pair.guard.set_failed(&mut pair.manager, false, pair.now);
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.ev.charging().source(), CurrentSource::Curtailed);
    assert_eq!(pair.ev.charging().effective(Phase::A), Some(16.0));
}

/// A curtailment below what the car can charge at is clamped rather than refused.
///
/// A car that stops charging is worse for the supply than one charging at its minimum,
/// and the manager knows the minimum: it read it from the permitted value sets.
#[test]
fn a_limit_below_the_minimum_becomes_the_minimum() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.car_device();

    pair.guard.require(&device, ChargingCurrents::same(2.0));
    pair.advance(Duration::from_secs(1));

    assert_eq!(
        pair.accepted().last().and_then(|c| c.get(Phase::A)),
        Some(RANGE.0)
    );
    assert_eq!(pair.ev.charging().effective(Phase::A), Some(RANGE.0));
}

/// [OPEV-004]: "no curtailment needed" is something the manager says, not something it
/// leaves the car to infer.
#[test]
fn opev_004_no_curtailment_needed_is_written_rather_than_implied() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.car_device();

    pair.guard.require(&device, ChargingCurrents::same(10.0));
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.ev.charging().effective(Phase::A), Some(10.0));

    pair.guard.require(&device, ChargingCurrents::default());
    pair.advance(Duration::from_secs(1));

    assert_eq!(
        pair.ev.charging().source(),
        CurrentSource::Curtailed,
        "the manager is still in charge"
    );
    assert_eq!(
        pair.ev.charging().effective(Phase::A),
        None,
        "and it has said no limit is needed"
    );
}

/// The manager beats often enough that a car never falls back while it is working.
///
/// The heartbeat period is half the watchdog, so one lost message is a gap and not a
/// fallback — which at four seconds is the difference between a car that charges and one
/// that drops to six amperes every time a frame is late.
#[test]
fn the_heartbeat_keeps_the_car_curtailed_through_normal_operation() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.car_device();

    pair.guard.require(&device, ChargingCurrents::same(16.0));
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.ev.charging().source(), CurrentSource::Curtailed);

    // Twenty seconds of ordinary operation, driven a second at a time.
    for _ in 0..20 {
        pair.advance(Duration::from_secs(1));
        assert_eq!(
            pair.ev.charging().source(),
            CurrentSource::Curtailed,
            "the car fell back while the manager was working"
        );
        assert_eq!(pair.ev.charging().effective(Phase::A), Some(16.0));
    }

    // And the period really is at most half the watchdog.
    assert!(charging::HEARTBEAT_TIMEOUT / 2 >= Duration::from_secs(2));
}

/// Only the Energy Guard the car subscribed to keeps it curtailed.
///
/// A heartbeat is a claim that someone is watching the supply. Taking one from whatever
/// device happens to send it would let anything on the connection hold the car at full
/// current — which is the opposite of what scenarios 2 and 3 are for.
#[test]
fn a_heartbeat_from_somebody_else_does_not_count() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.car_device();

    pair.guard.require(&device, ChargingCurrents::same(16.0));
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.ev.charging().source(), CurrentSource::Curtailed);

    // Something else on the connection sends a heartbeat while the manager goes quiet.
    let elsewhere = eebus::spine::feature_address(
        &eebus::spine::device_address("i:99999", "Impostor").unwrap(),
        &[1],
        1,
    );
    pair.now += Duration::from_secs(6);
    let beat = eebus::model::CmdData::DeviceDiagnosisHeartbeatData(
        eebus::model::DeviceDiagnosisHeartbeatData {
            heartbeat_counter: Some(1),
            ..Default::default()
        },
    );
    pair.ev.handle_event(
        &mut pair.car,
        &SpineEvent::DataNotified {
            feature: elsewhere,
            data: beat.clone(),
            resolved: beat,
        },
        pair.now,
    );
    pair.ev.handle_timeout(&mut pair.car, pair.now);

    assert_eq!(
        pair.ev.charging().source(),
        CurrentSource::GuardSilent,
        "the impostor's heartbeat did not keep the car curtailed"
    );
    assert_eq!(pair.ev.charging().effective(Phase::A), Some(RANGE.0));
}

/// What the car publishes on `LoadControl` is what the manager set, not what the car is
/// charging at — the two differ exactly when the manager is not to be relied on.
#[test]
fn the_published_limit_is_the_managers_not_the_safe_current() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.car_device();

    pair.guard.require(&device, ChargingCurrents::same(16.0));
    pair.advance(Duration::from_secs(1));

    // Silence: the car drops to six amperes, and says the sixteen is no longer in force.
    pair.now += Duration::from_secs(6);
    pair.ev.handle_timeout(&mut pair.car, pair.now);
    pair.settle();

    let load_control = pair.car.device().address_of(&[1, 1], 1);
    let published = pair
        .car
        .device()
        .resolve(&load_control)
        .and_then(|f| f.data(&Function::LoadControlLimitListData))
        .cloned()
        .expect("the car published its limits");
    let eebus::model::CmdData::LoadControlLimitListData(list) = &published else {
        panic!("expected the limit list");
    };
    let entry = &list.load_control_limit_data.as_ref().unwrap()[0];

    assert_eq!(entry.is_limit_active, Some(false), "no longer in force");
    assert_eq!(
        entry
            .value
            .as_ref()
            .and_then(eebus::model::ScaledNumber::to_f64),
        Some(16.0),
        "and still the number the manager wrote, which Table 7 says to ignore"
    );
    assert_eq!(
        pair.ev.charging().effective(Phase::A),
        Some(RANGE.0),
        "while the car itself is on six"
    );
}
