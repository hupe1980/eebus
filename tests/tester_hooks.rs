//! The runtime signals a certification laboratory reads off a device.
//!
//! Every High-Level Test Specification carries the same footnote: *"the manufacturer must
//! specify conditions on how the test case can be tested (e.g. via debug interface)"*.
//! Half of the LPC transitions are timers — a duration expiring, a heartbeat that stops —
//! and nothing goes on the wire when one fires, so a tester that can only see the wire
//! cannot tell `unlimited/controlled` from `failsafe state`.
//!
//! These tests pin the shape and the names of that interface, because the names go into
//! the parameter sheet a laboratory is handed and cannot then quietly change.

use core::time::Duration;

use eebus::model::ElectricalConnectionPhaseName as Phase;
use eebus::usecases::limitation::{
    ControllableSystem, CsConfig, LimitWrite, LimitationState, LocalDecision,
};
use eebus::usecases::monitoring::{Measurand, MonitoredUnit, Naming, Quantity};
use eebus::usecases::signals::{SignalValue, Signals};
use eebus::usecases::{lpc, lpp};

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn limited_system() -> ControllableSystem {
    let mut cs = ControllableSystem::new(
        CsConfig::new(4_200.0, secs(2 * 3_600)).with_nominal_max(11_000.0),
        Duration::ZERO,
    );
    cs.on_heartbeat(secs(10));
    cs.on_limit_write(
        &LimitWrite::active_for(3_000.0, secs(600)),
        LocalDecision::Apply,
        secs(11),
    );
    cs
}

/// The three names the tester asks for by name, and the values behind them.
#[test]
fn the_three_signals_the_tester_names() {
    let signals = limited_system().signals(lpc::DIRECTION);

    assert_eq!(
        signals.get("lpc:limit").and_then(SignalValue::as_f64),
        Some(3_000.0)
    );
    assert_eq!(
        signals.get("lpc:duration").and_then(SignalValue::as_f64),
        Some(600.0)
    );
    assert_eq!(
        signals.get("lpc:isActive").and_then(SignalValue::as_bool),
        Some(true)
    );
}

/// The state is reported the way the specification spells it, not the way Rust does: a
/// laboratory compares it against §2.3.2 and against the `CF_CS_…` pre-conditions.
#[test]
fn the_state_is_reported_in_the_specifications_own_words() {
    let signals = limited_system().signals(lpc::DIRECTION);
    assert_eq!(
        signals.get("lpc:state").and_then(SignalValue::as_str),
        Some("limited")
    );

    assert_eq!(LimitationState::Init.as_str(), "init");
    assert_eq!(
        LimitationState::UnlimitedControlled.as_str(),
        "unlimited/controlled"
    );
    assert_eq!(LimitationState::FailsafeState.as_str(), "failsafe state");
    assert_eq!(
        LimitationState::UnlimitedAutonomous.as_str(),
        "unlimited/autonomous"
    );

    assert_eq!(
        LimitationState::FailsafeState.test_configuration(),
        "CF_CS_FS"
    );
    assert_eq!(
        LimitationState::UnlimitedAutonomous.test_configuration(),
        "CF_CS_UnlAuto"
    );
}

/// An absent value is absent, not nought. A tester reading `0` for a limit that was never
/// set would record a device limited to zero watts, which is the opposite of unlimited.
#[test]
fn an_unset_value_is_not_reported_as_zero() {
    let cs = ControllableSystem::new(CsConfig::new(4_200.0, secs(2 * 3_600)), Duration::ZERO);
    let signals = cs.signals(lpc::DIRECTION);

    assert!(signals.get("lpc:limit").is_some_and(SignalValue::is_absent));
    assert!(
        signals
            .get("lpc:duration")
            .is_some_and(SignalValue::is_absent)
    );
    assert!(
        signals
            .get("lpc:nominalMax")
            .is_some_and(SignalValue::is_absent),
        "a device that was never told its nameplate does not invent one"
    );
    assert_eq!(
        signals.get("lpc:isActive").and_then(SignalValue::as_bool),
        Some(false),
        "a flag, though, is simply false"
    );
}

/// LPP reports the same values under its own prefix — the two use cases are separate
/// certifications with separate parameter sheets.
#[test]
fn lpp_reports_the_same_values_under_its_own_prefix() {
    let signals = limited_system().signals(lpp::DIRECTION);
    assert_eq!(
        signals.get("lpp:limit").and_then(SignalValue::as_f64),
        Some(3_000.0)
    );
    assert_eq!(signals.get("lpc:limit"), None);
}

/// [LPC-041] and [LPC-042] are mutually exclusive, and the debug interface has to say so
/// too: `ATC_LPC_COM_PT_CSUnlCntrl_002` and `_003` are the two halves of one either/or,
/// and exactly one of them is executed.
#[test]
fn only_the_nominal_maximum_this_actor_may_publish_is_reported() {
    let appliance = ControllableSystem::new(
        CsConfig::new(4_200.0, secs(2 * 3_600))
            .with_nominal_max(11_000.0)
            .with_contractual_max(30_000.0),
        Duration::ZERO,
    );
    let signals = appliance.signals(lpc::DIRECTION);
    assert_eq!(
        signals.get("lpc:nominalMax").and_then(SignalValue::as_f64),
        Some(11_000.0)
    );
    assert!(
        signals
            .get("lpc:contractualMax")
            .is_some_and(SignalValue::is_absent)
    );

    let manager = ControllableSystem::new(
        CsConfig::new(4_200.0, secs(2 * 3_600))
            .with_nominal_max(11_000.0)
            .with_contractual_max(30_000.0)
            .on_cem(),
        Duration::ZERO,
    );
    let signals = manager.signals(lpc::DIRECTION);
    assert!(
        signals
            .get("lpc:nominalMax")
            .is_some_and(SignalValue::is_absent)
    );
    assert_eq!(
        signals
            .get("lpc:contractualMax")
            .and_then(SignalValue::as_f64),
        Some(30_000.0)
    );
}

/// The timed transitions are exactly what the wire cannot show, so the deadline is a
/// signal: `ATC_LPC_COM_PT_CSTransition5_001` waits 120 seconds for one to fire.
#[test]
fn the_next_deadline_is_visible_so_a_timed_transition_can_be_watched() {
    let cs = limited_system();
    assert_eq!(
        cs.signals(lpc::DIRECTION)
            .get("lpc:nextDeadline")
            .and_then(SignalValue::as_f64),
        Some(130.0),
        "the heartbeat at t=10 plus the 120-second window"
    );

    let mut settled = cs;
    while let Some(deadline) = settled.poll_timeout() {
        settled.handle_timeout(deadline);
    }
    assert!(
        settled
            .signals(lpc::DIRECTION)
            .get("lpc:nextDeadline")
            .is_some_and(SignalValue::is_absent),
        "and nothing is pending once it is autonomous"
    );
}

/// A Monitored Unit reports its measurands under `mpc:` or `mgcp:`, and a value that is
/// not `normal` reports its state instead of a number — which is the whole subject of the
/// `NT_` test cases in MPC and MGCP.
#[test]
fn a_monitored_unit_reports_its_measurands_and_their_value_states() {
    let mut unit = MonitoredUnit::new(1)
        .naming(Naming::GridConnectionPoint)
        .with(Measurand::total_power())
        .with(Measurand::on(Quantity::Current, Phase::A))
        .with_range(Measurand::on(Quantity::Current, Phase::A), 0.0, 32.0);
    unit.set(&Measurand::total_power(), -2_300.0);
    unit.set(&Measurand::on(Quantity::Current, Phase::A), 40.0);

    let signals = unit.signals(());
    assert_eq!(
        signals
            .get("mgcp:totalActivePower")
            .and_then(SignalValue::as_f64),
        Some(-2_300.0),
        "exporting, which at a grid connection point is a negative total"
    );
    assert_eq!(
        signals.get("mgcp:currentA").and_then(SignalValue::as_str),
        Some("outOfRange"),
        "40 A on a 32 A connection is not a reading, it is a fault"
    );
}

/// The rendering is one line per signal, so a debug console or a log needs no formatter
/// of its own.
#[test]
fn the_set_renders_one_signal_per_line() {
    let rendered = limited_system().signals(lpc::DIRECTION).to_string();
    let lines: Vec<&str> = rendered.lines().collect();

    assert!(lines.contains(&"lpc:state limited"), "{rendered}");
    assert!(lines.contains(&"lpc:limit 3000 W"), "{rendered}");
    assert!(lines.contains(&"lpc:duration 600 s"), "{rendered}");
    assert!(lines.contains(&"lpc:isActive true"), "{rendered}");
    assert_eq!(
        lines.len(),
        limited_system().signals(lpc::DIRECTION).len(),
        "one line each, and nothing swallowed"
    );
}
