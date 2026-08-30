//! The Limitation of Power Consumption state machine.
//!
//! The tests walk the thirteen transitions of LPC UC TS 1.0.0 §2.3.3 in order, then the
//! rules the 2026 implementation guide added. Each is named after the requirement it
//! covers, so a reviewer — or a certification laboratory — can follow the specification
//! and the test list side by side.

use core::time::Duration;

use eebus::model::ErrorNumber;

use eebus::usecases::lpc::{
    ControllableSystem, CsConfig, EffectiveLimit, LimitWrite, LocalDecision, LpcState, NackReason,
    RejectReason, WriteOutcome,
};

const FAILSAFE_WATTS: f64 = 4_200.0;
const FAILSAFE_DURATION: Duration = Duration::from_secs(2 * 3_600);

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn new_cs() -> ControllableSystem {
    ControllableSystem::new(
        CsConfig::new(FAILSAFE_WATTS, FAILSAFE_DURATION).with_nominal_max(11_000.0),
        Duration::ZERO,
    )
}

/// Runs the opening sequence — heartbeat, then limit — and returns the clock.
fn commission(cs: &mut ControllableSystem, write: LimitWrite) -> Duration {
    let mut now = secs(10);
    cs.on_heartbeat(now);
    now += secs(1);
    assert!(
        cs.on_limit_write(&write, LocalDecision::Apply, now)
            .is_accepted()
    );
    now
}

/// Transition 0, [LPC-901]: a restart lands in `init`, limited by the failsafe value.
///
/// The direction matters: a device that came up unlimited would draw full power for the
/// two minutes before the Energy Guard reaches it, which after a regional outage is
/// exactly when the grid can least afford it.
#[test]
fn lpc_901_restart_starts_limited_by_the_failsafe_value() {
    let cs = new_cs();
    assert_eq!(cs.state(), LpcState::Init);
    assert_eq!(
        cs.effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );
    assert!(!cs.is_limit_active(), "[LPC-009/2]");
}

/// Transition 1, [LPC-902] and [LPC-905]: a deactivated limit after a heartbeat leaves
/// `init` for `unlimited/controlled`.
#[test]
fn lpc_902_905_init_to_unlimited_controlled() {
    let mut cs = new_cs();
    commission(&mut cs, LimitWrite::deactivated());
    assert_eq!(cs.state(), LpcState::UnlimitedControlled);
    assert_eq!(cs.effective_limit(), EffectiveLimit::None);
}

/// Transition 2, [LPC-904]: an activated limit that can be applied leads to `limited`.
#[test]
fn lpc_904_init_to_limited() {
    let mut cs = new_cs();
    commission(&mut cs, LimitWrite::active(3_000.0));
    assert_eq!(cs.state(), LpcState::Limited);
    assert_eq!(cs.effective_limit(), EffectiveLimit::Active(3_000.0));
    assert!(cs.is_limit_active(), "[LPC-009/1]");
}

/// Transition 3, [LPC-906]: silence for two minutes after a restart ends the wait.
#[test]
fn lpc_906_init_to_unlimited_autonomous_after_120_seconds() {
    let mut cs = new_cs();
    assert_eq!(cs.poll_timeout(), Some(secs(120)));

    cs.handle_timeout(secs(119));
    assert_eq!(cs.state(), LpcState::Init, "not yet");

    cs.handle_timeout(secs(120));
    assert_eq!(cs.state(), LpcState::UnlimitedAutonomous);
    assert_eq!(cs.effective_limit(), EffectiveLimit::None);
}

/// Transition 4, [LPC-910]: from `unlimited/controlled`, an applicable limit engages.
#[test]
fn lpc_910_unlimited_controlled_to_limited() {
    let mut cs = new_cs();
    let mut now = commission(&mut cs, LimitWrite::deactivated());
    assert_eq!(cs.state(), LpcState::UnlimitedControlled);

    now += secs(5);
    assert!(
        cs.on_limit_write(&LimitWrite::active(2_000.0), LocalDecision::Apply, now)
            .is_accepted()
    );
    assert_eq!(cs.state(), LpcState::Limited);
    assert_eq!(cs.effective_limit(), EffectiveLimit::Active(2_000.0));
}

/// Transition 5, [LPC-911]: no heartbeat for two minutes falls back to the failsafe.
#[test]
fn lpc_911_unlimited_controlled_to_failsafe() {
    let mut cs = new_cs();
    commission(&mut cs, LimitWrite::deactivated());

    // The window runs from the last heartbeat, at t = 10 s, not from the write that
    // followed it.
    cs.handle_timeout(secs(10) + secs(119));
    assert_eq!(cs.state(), LpcState::UnlimitedControlled);

    cs.handle_timeout(secs(10) + secs(120));
    assert_eq!(cs.state(), LpcState::FailsafeState);
    assert_eq!(
        cs.effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );
}

/// Transition 6, [LPC-908]: an expired duration deactivates the limit.
#[test]
fn lpc_908_limited_to_unlimited_controlled_when_the_duration_expires() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active_for(3_000.0, secs(900)));
    assert_eq!(cs.state(), LpcState::Limited);

    // The heartbeat keeps arriving; only the duration runs out.
    for t in (60..=900).step_by(60) {
        cs.on_heartbeat(now + secs(t));
        cs.handle_timeout(now + secs(t));
    }
    assert_eq!(cs.state(), LpcState::UnlimitedControlled);
    assert_eq!(cs.effective_limit(), EffectiveLimit::None);
}

/// Transition 6, [LPC-909]: a deactivated limit has the same effect, immediately.
#[test]
fn lpc_909_limited_to_unlimited_controlled_on_deactivation() {
    let mut cs = new_cs();
    let mut now = commission(&mut cs, LimitWrite::active(3_000.0));
    now += secs(30);
    cs.on_heartbeat(now);
    assert!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now)
            .is_accepted()
    );
    assert_eq!(cs.state(), LpcState::UnlimitedControlled);
}

/// Transition 7, [LPC-912]: a limited system falls back to the failsafe when the
/// heartbeat stops — which is also what a lost connection looks like.
#[test]
fn lpc_912_limited_to_failsafe() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active(3_000.0));

    cs.handle_timeout(now + secs(200));
    assert_eq!(cs.state(), LpcState::FailsafeState);
    assert_eq!(
        cs.effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );
}

/// Transitions 8 and 9, [LPC-918] to [LPC-920]: the failsafe state is left only on a
/// heartbeat followed by a limit write.
#[test]
fn lpc_918_919_920_leaving_the_failsafe_state() {
    for (write, expected) in [
        (LimitWrite::active(2_500.0), LpcState::Limited),
        (LimitWrite::deactivated(), LpcState::UnlimitedControlled),
    ] {
        let mut cs = new_cs();
        let mut now = commission(&mut cs, LimitWrite::active(3_000.0));
        cs.handle_timeout(now + secs(200));
        assert_eq!(cs.state(), LpcState::FailsafeState);

        now += secs(300);
        cs.on_heartbeat(now);
        now += secs(1);
        assert!(
            cs.on_limit_write(&write, LocalDecision::Apply, now)
                .is_accepted()
        );
        assert_eq!(cs.state(), expected);
    }
}

/// Transition 10, [LPC-922]: after the Failsafe Duration Minimum the system stops
/// waiting for an Energy Guard that never came back.
#[test]
fn lpc_922_failsafe_to_unlimited_autonomous_after_the_minimum_duration() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active(3_000.0));
    cs.handle_timeout(now + secs(200));
    assert_eq!(cs.state(), LpcState::FailsafeState);

    cs.handle_timeout(now + secs(200) + FAILSAFE_DURATION - secs(1));
    assert_eq!(
        cs.state(),
        LpcState::FailsafeState,
        "the minimum still holds"
    );

    cs.handle_timeout(now + secs(200) + FAILSAFE_DURATION);
    assert_eq!(cs.state(), LpcState::UnlimitedAutonomous);
}

/// Transition 10, [LPC-921]: heartbeats resume but no limit follows within two minutes,
/// so the Energy Guard is alive but not controlling.
#[test]
fn lpc_921_failsafe_to_unlimited_autonomous_when_no_limit_follows() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active(3_000.0));
    cs.handle_timeout(now + secs(200));
    assert_eq!(cs.state(), LpcState::FailsafeState);

    let resumed = now + secs(300);
    cs.on_heartbeat(resumed);
    assert_eq!(
        cs.poll_timeout(),
        Some(resumed + secs(120)),
        "the window runs from the heartbeat, not from entering the state"
    );

    cs.handle_timeout(resumed + secs(119));
    assert_eq!(cs.state(), LpcState::FailsafeState);

    cs.handle_timeout(resumed + secs(120));
    assert_eq!(cs.state(), LpcState::UnlimitedAutonomous);
}

/// Transitions 11 and 12, [LPC-918] to [LPC-920]: the autonomous state is left the same
/// way the failsafe state is.
#[test]
fn lpc_919_unlimited_autonomous_to_limited() {
    let mut cs = new_cs();
    cs.handle_timeout(secs(120));
    assert_eq!(cs.state(), LpcState::UnlimitedAutonomous);

    let mut now = secs(500);
    cs.on_heartbeat(now);
    now += secs(1);
    assert!(
        cs.on_limit_write(&LimitWrite::active(1_000.0), LocalDecision::Apply, now)
            .is_accepted()
    );
    assert_eq!(cs.state(), LpcState::Limited);
}

/// [LPC-907]: a refusal leaves a controlled state as it was.
#[test]
fn lpc_907_a_rejection_does_not_change_a_controlled_state() {
    let mut cs = new_cs();
    let mut now = commission(&mut cs, LimitWrite::active(3_000.0));
    now += secs(10);
    cs.on_heartbeat(now);

    let outcome = cs.on_limit_write(
        &LimitWrite::active(0.0),
        LocalDecision::Reject(RejectReason::SelfProtection),
        now,
    );
    assert_eq!(
        outcome,
        WriteOutcome::Rejected(NackReason::CannotApply(RejectReason::SelfProtection))
    );
    assert_eq!(
        outcome.error_number(),
        ErrorNumber::CommandRejected,
        "SPINE: command rejected"
    );
    assert_eq!(
        cs.state(),
        LpcState::Limited,
        "the earlier limit still holds"
    );
    assert_eq!(cs.effective_limit(), EffectiveLimit::Active(3_000.0));
}

/// [LPC-918]: a refusal *from the failsafe state* still counts as contact, so the
/// system moves to `unlimited/controlled` rather than staying limited.
#[test]
fn lpc_918_a_rejection_from_failsafe_reaches_unlimited_controlled() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active(3_000.0));
    cs.handle_timeout(now + secs(200));
    assert_eq!(cs.state(), LpcState::FailsafeState);

    let resumed = now + secs(300);
    cs.on_heartbeat(resumed);
    let outcome = cs.on_limit_write(
        &LimitWrite::active(0.0),
        LocalDecision::Reject(RejectReason::SafetyRelated),
        resumed + secs(1),
    );
    assert!(!outcome.is_accepted());
    assert_eq!(cs.state(), LpcState::UnlimitedControlled);
}

/// Implementation guide §2.14: in the failsafe state a write without a heartbeat inside
/// the preceding sixty seconds is refused *without evaluating the limit*, and the
/// failsafe stays in force.
///
/// This is the rule that stops an Energy Guard which has lost its own upstream link
/// from lifting a limitation it can no longer justify.
#[test]
fn ig_2_14_a_write_without_a_recent_heartbeat_is_refused() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active(3_000.0));
    cs.handle_timeout(now + secs(200));
    assert_eq!(cs.state(), LpcState::FailsafeState);

    // A heartbeat, then a write more than sixty seconds later.
    let resumed = now + secs(300);
    cs.on_heartbeat(resumed);
    let outcome = cs.on_limit_write(
        &LimitWrite::deactivated(),
        LocalDecision::Apply,
        resumed + secs(61),
    );

    assert_eq!(
        outcome,
        WriteOutcome::Rejected(NackReason::NoRecentHeartbeat)
    );
    assert_eq!(cs.state(), LpcState::FailsafeState, "the failsafe persists");
    assert_eq!(
        cs.effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );
}

/// Implementation guide §2.11: before the opening heartbeat-then-limit sequence has
/// completed, a write on any other data point is refused.
#[test]
fn ig_2_11_other_data_points_are_refused_before_the_sequence_completes() {
    let mut cs = new_cs();

    // Pre-heartbeat: nothing is accepted.
    assert_eq!(
        cs.on_failsafe_limit_write(5_000.0, secs(1)),
        WriteOutcome::Rejected(NackReason::SequenceIncomplete)
    );

    // Post-heartbeat but pre-limit: the limit would be accepted, the failsafe is not.
    cs.on_heartbeat(secs(10));
    assert_eq!(
        cs.on_failsafe_limit_write(5_000.0, secs(11)),
        WriteOutcome::Rejected(NackReason::SequenceIncomplete)
    );

    // After the limit, the failsafe values open up.
    assert!(
        cs.on_limit_write(&LimitWrite::active(3_000.0), LocalDecision::Apply, secs(12))
            .is_accepted()
    );
    assert!(cs.on_failsafe_limit_write(5_000.0, secs(13)).is_accepted());
    assert_eq!(cs.config().failsafe_watts, 5_000.0);
}

/// Implementation guide §2.15: the failsafe values are mandatory to accept, so that a
/// device cannot be left on a factory default that protects nothing.
#[test]
fn ig_2_15_failsafe_values_are_writeable() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active(3_000.0));

    assert!(cs.on_failsafe_limit_write(2_000.0, now).is_accepted());
    assert_eq!(cs.config().failsafe_watts, 2_000.0);

    assert!(
        cs.on_failsafe_duration_write(secs(4 * 3_600), now)
            .is_accepted()
    );
    assert_eq!(cs.config().failsafe_duration, secs(4 * 3_600));
}

/// [LPC-022/4] and implementation guide §3.1: a failsafe duration outside two to
/// twenty-four hours is refused rather than silently clamped.
#[test]
fn lpc_022_4_a_duration_out_of_range_is_refused() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active(3_000.0));

    for bad in [secs(3_600), secs(25 * 3_600)] {
        assert_eq!(
            cs.on_failsafe_duration_write(bad, now),
            WriteOutcome::Rejected(NackReason::DurationOutOfRange)
        );
    }
    assert_eq!(
        cs.config().failsafe_duration,
        FAILSAFE_DURATION,
        "unchanged"
    );
}

/// Implementation guide §3.6: a limit below zero is refused. Sign conventions make a
/// negative consumption limit meaningless, and accepting one would be indistinguishable
/// from a limit of zero.
#[test]
fn ig_3_6_a_negative_limit_is_refused() {
    let mut cs = new_cs();
    cs.on_heartbeat(secs(10));
    assert_eq!(
        cs.on_limit_write(
            &LimitWrite {
                is_active: true,
                watts: -1.0,
                duration: None
            },
            LocalDecision::Apply,
            secs(11)
        ),
        WriteOutcome::Rejected(NackReason::NegativeValue)
    );
    assert_eq!(cs.state(), LpcState::Init, "still waiting");
}

/// Implementation guide §2.16, example 1: rewriting the same value is accepted. Only an
/// invalid command or one the device cannot follow is refused.
#[test]
fn ig_2_16_an_unchanged_value_is_still_accepted() {
    let mut cs = new_cs();
    let mut now = commission(&mut cs, LimitWrite::active(3_000.0));
    now += secs(5);
    cs.on_heartbeat(now);
    assert!(
        cs.on_limit_write(&LimitWrite::active(3_000.0), LocalDecision::Apply, now)
            .is_accepted()
    );
    assert_eq!(cs.state(), LpcState::Limited);
}

/// Implementation guide §2.16, example 2: an activated limit with a duration of zero is
/// accepted and takes effect as an immediate deactivation.
#[test]
fn ig_2_16_a_zero_duration_deactivates_and_is_accepted() {
    let mut cs = new_cs();
    let mut now = commission(&mut cs, LimitWrite::active(3_000.0));
    now += secs(5);
    cs.on_heartbeat(now);

    let outcome = cs.on_limit_write(
        &LimitWrite::active_for(1_000.0, Duration::ZERO),
        LocalDecision::Apply,
        now,
    );
    assert!(outcome.is_accepted(), "accepted, not refused");
    assert_eq!(cs.state(), LpcState::UnlimitedControlled);
    assert_eq!(cs.effective_limit(), EffectiveLimit::None);
}

/// Implementation guide §2.17: losing the connection is not a restart. The state machine
/// falls to the failsafe and stays there for the Failsafe Duration Minimum; it must not
/// return to `init`, where two minutes of silence would make it unlimited instead.
#[test]
fn ig_2_17_a_lost_connection_reaches_failsafe_not_init() {
    let mut cs = new_cs();
    let now = commission(&mut cs, LimitWrite::active(3_000.0));
    assert_eq!(cs.state(), LpcState::Limited);

    // The connection drops: no notification, the heartbeats simply stop.
    cs.handle_timeout(now + secs(120));
    assert_eq!(cs.state(), LpcState::FailsafeState);

    // Two minutes later — the point at which `init` would have gone unlimited — the
    // failsafe still applies.
    cs.handle_timeout(now + secs(240));
    assert_eq!(cs.state(), LpcState::FailsafeState);
    assert_eq!(
        cs.effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );

    // And it holds for the whole minimum duration.
    cs.handle_timeout(now + secs(120) + FAILSAFE_DURATION - secs(1));
    assert_eq!(cs.state(), LpcState::FailsafeState);
}

/// The state machine never asks to be woken without reason, and never sleeps through a
/// deadline: every state that has a timer reports one.
#[test]
fn every_timed_state_reports_a_deadline() {
    let mut cs = new_cs();
    assert!(cs.poll_timeout().is_some(), "init settles");

    let now = commission(&mut cs, LimitWrite::active_for(3_000.0, secs(900)));
    assert_eq!(
        cs.poll_timeout(),
        Some(now - secs(1) + secs(120)),
        "limited: the heartbeat window comes before the duration"
    );

    cs.handle_timeout(now + secs(200));
    assert_eq!(cs.state(), LpcState::FailsafeState);
    assert!(
        cs.poll_timeout().is_some(),
        "failsafe has a minimum duration"
    );

    cs.handle_timeout(now + secs(200) + FAILSAFE_DURATION);
    assert_eq!(cs.state(), LpcState::UnlimitedAutonomous);
    assert_eq!(
        cs.poll_timeout(),
        None,
        "autonomous waits only for the Energy Guard to speak"
    );
}
