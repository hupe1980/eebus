//! One convention, asserted across every state machine that publishes a deadline.
//!
//! Everything sans-IO here has the same pair: `poll_timeout` says *when* to come back, and
//! `handle_timeout(now)` is what happens when you do. The contract between them is one
//! line:
//!
//! > **Waiting until exactly `poll_timeout()` is enough.** At that instant the deadline has
//! > passed — `now >= deadline` fires — so a caller that sleeps until the published moment
//! > and no longer sees the transition.
//!
//! Getting that boundary wrong does not delay a transition by a tick. `poll_timeout` is
//! derived from the same stored instant `handle_timeout` compares against, so it answers
//! the *same* value again, and a loop that does nothing but wait on it never terminates.
//!
//! The rule is asserted here rather than per machine, because a convention only some
//! implementations follow is not a convention.

use core::time::Duration;

use eebus::model::{
    DeviceType, ElectricalConnectionPhaseName as Phase, EntityType, FeatureType, Function,
};
use eebus::spine::{Engine, HeartbeatMonitor, LocalDevice, LocalEntity, LocalFeature, Operations};
use eebus::usecases::cob::{BatteryControl, CobConfig, CobState, InverterKind};
use eebus::usecases::emobility::charging::{ChargingCurrents, CurrentSource, EvCharging};
use eebus::usecases::limitation::{ControllableSystem, CsConfig, LimitationState};

/// How many times a machine may be woken on its own deadline before we call it a spin.
///
/// Every machine here reaches its resting state in a handful of transitions; a machine that
/// is still answering after this many has stopped making progress.
const TURNS: usize = 16;

/// Drives `machine` purely from what it publishes, and fails if that never settles.
///
/// `poll` is the machine's `poll_timeout` and `fire` its `handle_timeout`. Nothing else is
/// supplied: no wall clock, no ticks, no slack added to the deadline. That is the point —
/// a caller with a real timer sleeps until the instant it was given, not a millisecond
/// later, and anything that needs the extra millisecond is a machine whose contract is
/// wrong rather than a caller who is impatient.
fn settles<M>(
    what: &str,
    machine: &mut M,
    mut poll: impl FnMut(&M) -> Option<Duration>,
    mut fire: impl FnMut(&mut M, Duration),
) -> usize {
    let mut turns = 0;
    let mut last: Option<Duration> = None;
    while let Some(deadline) = poll(machine) {
        assert!(
            turns < TURNS,
            "{what}: still waking on {deadline:?} after {TURNS} turns — \
             `poll_timeout` is publishing an instant that `handle_timeout` does not act on"
        );
        assert!(
            last != Some(deadline) || turns == 0,
            "{what}: {deadline:?} came back unchanged, so waiting on it made no progress"
        );
        last = Some(deadline);
        fire(machine, deadline);
        turns += 1;
    }
    turns
}

/// LPC's Controllable System reaches `unlimited/autonomous` on its own deadlines.
#[test]
fn the_limitation_state_machine_settles_on_its_own_deadlines() {
    let mut cs = ControllableSystem::new(
        CsConfig::new(4_200.0, Duration::from_secs(2 * 3_600)),
        Duration::ZERO,
    );
    settles(
        "ControllableSystem",
        &mut cs,
        ControllableSystem::poll_timeout,
        ControllableSystem::handle_timeout,
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
}

/// Control of Battery is the same machine's shape, and reaches the same kind of rest.
#[test]
fn the_battery_control_machine_settles_on_its_own_deadlines() {
    let mut battery = BatteryControl::new(
        CobConfig::new(InverterKind::Battery, Duration::from_secs(2 * 3_600)),
        Duration::ZERO,
    );
    settles(
        "BatteryControl",
        &mut battery,
        BatteryControl::poll_timeout,
        BatteryControl::handle_timeout,
    );
    assert_eq!(battery.state(), CobState::AutoUncontrolled);
}

/// OPEV's four-second fallback fires at the instant it published, and then stops.
///
/// This is the one that spun. The car was curtailed, the guard went quiet, and a loop
/// waiting on `poll_timeout` got `last + 4s` back for ever without the safe current ever
/// coming into force.
#[test]
fn the_ev_charging_fallback_fires_on_its_own_deadline() {
    let mut car = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
    car.on_heartbeat(Duration::from_secs(1));
    car.on_limit(ChargingCurrents::same(16.0), Duration::from_secs(1));
    assert_eq!(car.source(), CurrentSource::Curtailed);

    let deadline = car.poll_timeout().expect("a deadline while curtailed");
    car.handle_timeout(deadline);
    assert_eq!(
        car.source(),
        CurrentSource::GuardSilent,
        "waiting until the published instant did not produce the fallback"
    );
    assert_eq!(car.effective_currents().get(Phase::A), Some(6.0));
}

/// A silent peer is reported at the instant the monitor published, not a tick later.
#[test]
fn the_heartbeat_monitor_reports_a_loss_on_its_own_deadline() {
    let mut monitor = HeartbeatMonitor::new();
    monitor.observe(Duration::from_secs(10));
    let deadline = monitor
        .poll_timeout()
        .expect("a deadline once a beat has arrived");

    assert!(
        monitor.is_alive(deadline - Duration::from_millis(1)),
        "a moment before the deadline the peer is still alive"
    );
    assert!(
        !monitor.is_alive(deadline),
        "and at it, it is not — the boundary belongs to silence"
    );
    assert!(
        monitor.handle_timeout(deadline),
        "the loss is reported to a caller that waited exactly this long"
    );
    assert!(
        !monitor.handle_timeout(deadline + Duration::from_secs(60)),
        "and reported once, not on every later tick"
    );
}

/// The engine expires a request, and a write nobody decided, on the instants it publishes.
#[test]
fn the_engine_expires_on_its_own_deadlines() {
    let mut device = LocalDevice::new(
        "i:12345",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::GridGuard).with_feature(
                LocalFeature::new(1, FeatureType::LoadControl, eebus::model::Role::Client)
                    .with_function(Function::LoadControlLimitListData, Operations::read()),
            ),
        )
        .unwrap();
    let mut engine = Engine::new(device);

    let peer = eebus::spine::device_address("i:67890", "HeatPump-1").unwrap();
    let target = eebus::spine::feature_address(&peer, &[1], 1);
    let source = engine.device().address_of(&[1], 1);
    engine.read(
        &target,
        &source,
        Function::LoadControlLimitListData,
        Duration::ZERO,
    );

    let turns = settles(
        "Engine",
        &mut engine,
        Engine::poll_timeout,
        Engine::handle_timeout,
    );
    // One request, but seven deadlines: the response deadline and then, for each rung of
    // the SPINE implementation guide §2.6.2 ladder, the delay before the retry and the
    // response deadline of the retry itself. The last one has no rung left and gives up.
    assert_eq!(turns, 1 + 2 * eebus::spine::RETRY_SCHEDULE.len());

    let events: Vec<_> = core::iter::from_fn(|| engine.poll_event()).collect();
    let retries: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            eebus::spine::SpineEvent::RequestRetried { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .collect();
    assert_eq!(retries, vec![1, 2, 3], "the three retries, in order");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, eebus::spine::SpineEvent::RequestTimedOut { .. })),
        "and only then is the request reported timed out"
    );
}
