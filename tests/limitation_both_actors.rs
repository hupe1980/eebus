//! Both halves of Limitation of Power Consumption, talking to each other.
//!
//! `limitation_over_the_wire` drives the Controllable System by hand, which is how the
//! server side's rules are checked one at a time. This file uses the real Energy Guard
//! on the other end, so what is under test is the part neither side can check alone: the
//! ordering the 2026 implementation guides impose on the exchange.
//!
//! Everything runs against a virtual clock, so a two-hour failsafe duration costs
//! nothing to test.

mod common;

use core::time::Duration;

use common::*;
use eebus::usecases::limitation::{
    self, ControllableSystem, CsConfig, CsEvent, EffectiveLimit, GuardEvent, LimitWrite,
    LimitationState, LocalDecision, MIN_WRITE_INTERVAL, NackReason, NominalMax, RETRY_BACKOFF_STEP,
    RejectReason, WriteOutcome,
};
use eebus::usecases::lpc;

/// The whole exchange: the guard takes control, sets a limit, and the pump applies it.
#[test]
fn the_two_actors_run_the_use_case_between_them() {
    let mut pair = Pair::new();
    pair.commission();

    assert!(
        pair.reports
            .iter()
            .any(|r| matches!(r, GuardEvent::Ready { .. })),
        "the guard holds both bindings"
    );
    assert!(
        pair.decisions
            .iter()
            .any(|d| matches!(d, CsEvent::GuardIdentified { .. })),
        "and the pump knows which entity holds them (implementation guide §3.8)"
    );
    // Commissioning is not silent: §2.11 makes the guard owe an opening write on the
    // limit as soon as it is bound, and until that lands the pump is not in a controllable
    // state at all. Nothing has been *required* of it, so what goes out is a deactivation
    // — which §2.13 permits precisely because the grid is asking for nothing.
    assert_eq!(
        pair.pump.system().state(),
        LimitationState::UnlimitedControlled
    );
    assert_eq!(pair.pump.system().effective_limit(), EffectiveLimit::None);
    assert_eq!(pair.accepted().len(), 1, "the opening deactivation");
    pair.forget_history();

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    assert_eq!(pair.accepted().len(), 1);
    assert_eq!(pair.accepted()[0].watts(), 3_000.0);
    assert_eq!(pair.pump.system().state(), LimitationState::Limited);
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::active(3_000.0)
    );
    assert_eq!(
        pair.guard.applied_limit(&device).map(|l| l.watts()),
        Some(3_000.0)
    );
}

/// §2.11: the heartbeat precedes the limit, and it is the actor that arranges it.
///
/// Without one the Controllable System refuses without even reading the value, which is
/// the rule §2.14 states in as many words.
#[test]
fn the_guard_sends_a_heartbeat_before_its_first_limit() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    assert!(
        pair.refused().is_empty(),
        "nothing was refused for want of a heartbeat"
    );
    assert_eq!(pair.pump.system().state(), LimitationState::Limited);
}

/// §2.10: a limit that has not changed is not rewritten every minute.
#[test]
fn an_unchanged_limit_is_not_rewritten() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.accepted().len(), 1);

    // Four minutes of heartbeats, and the same requirement.
    for _ in 0..4 {
        pair.advance(Duration::from_secs(60));
    }
    assert_eq!(
        pair.accepted().len(),
        1,
        "the guard wrote once, not once a minute"
    );
    assert!(MIN_WRITE_INTERVAL > Duration::from_secs(4 * 60));
}

/// §2.5: a refusal is retried rather than treated as the end of the conversation.
#[test]
fn a_refused_limit_is_retried_and_the_peer_is_kept() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();

    // A limit below zero is one the Controllable System must refuse ([LPC-001]).
    pair.guard
        .require(
            &device,
            Some(LimitWrite {
                is_active: true,
                value: eebus::usecases::limitation::LimitWrite::active(1_000.0).value,
                duration: None,
            }),
            pair.now,
        )
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.accepted().len(), 1);

    // Drive the pump into the failsafe state, where a limit arriving without a fresh
    // heartbeat is refused outright (§2.14).
    pair.now += Duration::from_secs(200);
    pair.pump.handle_timeout(&mut pair.pump_engine, pair.now);
    assert_eq!(pair.pump.system().state(), LimitationState::FailsafeState);

    assert!(
        pair.guard.peers().any(|p| p.device == device),
        "the guard keeps the device on its list whatever the answer"
    );
    assert_eq!(RETRY_BACKOFF_STEP, Duration::from_secs(60));
}

/// §2.2: a duration of zero never accompanies an activated limit.
#[test]
fn an_activated_limit_never_carries_a_zero_duration() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();
    pair.guard
        .require(
            &device,
            Some(LimitWrite {
                is_active: true,
                value: eebus::usecases::limitation::LimitWrite::active(3_000.0).value,
                duration: Some(Duration::ZERO),
            }),
            pair.now,
        )
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    let accepted = pair.accepted();
    assert_eq!(accepted.len(), 1);
    assert_eq!(
        accepted[0].duration, None,
        "the duration was dropped, not sent as zero"
    );
    assert_eq!(
        pair.pump.system().state(),
        LimitationState::Limited,
        "so the limit is in force rather than immediately deactivated"
    );
}

/// The Energy Guard writes the failsafe values the implementation guide §2.15 requires
/// a Controllable System to accept.
#[test]
fn the_guard_can_change_the_failsafe_values() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    // The sequence of §2.11 is complete, so other data points may now be written.
    pair.guard
        .write_failsafe_limit(&mut pair.guard_engine, &device, 2_000.0, pair.now);
    pair.guard.write_failsafe_duration(
        &mut pair.guard_engine,
        &device,
        Duration::from_secs(4 * 3_600),
        pair.now,
    );
    pair.settle();

    assert_eq!(pair.pump.system().config().failsafe_watts(), 2_000.0);
    assert_eq!(
        pair.pump.system().config().failsafe_duration,
        Duration::from_secs(4 * 3_600)
    );
}

/// A failsafe duration outside the two-to-twenty-four-hour range never reaches the wire.
#[test]
fn an_out_of_range_failsafe_duration_is_refused_locally() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();
    let device = pair.device();

    assert!(
        pair.guard
            .write_failsafe_duration(
                &mut pair.guard_engine,
                &device,
                Duration::from_secs(60),
                pair.now
            )
            .is_none(),
        "[LPC-022/1]: below two hours"
    );
    assert!(
        pair.guard
            .write_failsafe_duration(
                &mut pair.guard_engine,
                &device,
                Duration::from_secs(48 * 3_600),
                pair.now
            )
            .is_none(),
        "[LPC-022/3]: above twenty-four hours"
    );
}

/// A curtailment does not wait for the next minute's heartbeat.
///
/// The guide's ordering rule (§2.11) puts a heartbeat immediately before the limit; it
/// does not put the limit behind the *periodic* heartbeat. An Energy Guard that only
/// wrote on its sixty-second tick would take up to a minute to act on a grid event.
#[test]
fn a_new_requirement_is_due_at_once() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();
    pair.advance(Duration::from_secs(1));

    let quiet = pair.guard.poll_timeout();
    assert!(
        quiet >= pair.now + Duration::from_secs(30),
        "with nothing required, the next wake-up is the heartbeat: {quiet:?}"
    );

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    assert_eq!(
        pair.guard.poll_timeout(),
        pair.now,
        "the moment the grid needs something, the guard wants waking"
    );

    // One tick with no time passing at all is enough to get it out.
    let reports = pair.guard.handle_timeout(&mut pair.guard_engine, pair.now);
    pair.reports.extend(reports);
    pair.settle();
    assert_eq!(pair.accepted().len(), 1);
    assert_eq!(pair.pump.system().state(), LimitationState::Limited);
}

/// §2.10: a second decision inside five minutes waits, and then goes out on its own.
#[test]
fn a_write_held_back_by_the_five_minute_ceiling_goes_out_when_it_lifts() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();
    let device = pair.device();

    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.accepted().len(), 1);

    // A minute later the grid wants something else. Five minutes have not passed.
    pair.advance(Duration::from_secs(60));
    pair.guard
        .require(&device, Some(LimitWrite::active(2_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::ZERO);
    assert_eq!(pair.accepted().len(), 1, "still just the one write");

    // The guard comes back to it without being told to.
    pair.advance(MIN_WRITE_INTERVAL);
    assert_eq!(pair.accepted().len(), 2);
    assert_eq!(pair.accepted()[1].watts(), 2_000.0);
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::active(2_000.0)
    );
}

/// A limit the Controllable System dropped on its own is re-established.
///
/// §2.6 asks an Energy Guard to keep its Controllable Systems in the controlled states.
/// It cannot do that from a record of what it once sent: when a limit's duration runs
/// out the system deactivates it and says so, and the guard has to hear that and act.
#[test]
fn a_limit_the_system_dropped_is_written_again() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();
    let device = pair.device();

    // A limit with a duration: the Controllable System will deactivate it when it runs
    // out ([LPC-908]).
    pair.guard
        .require(
            &device,
            Some(LimitWrite::active_for(3_000.0, Duration::from_secs(120))),
            pair.now,
        )
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.accepted().len(), 1);
    assert_eq!(pair.pump.system().state(), LimitationState::Limited);

    // The grid still needs the limit, so the guard should notice and say so again.
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");

    // Five minutes on: the duration expired long ago and the write ceiling has lifted.
    pair.advance(MIN_WRITE_INTERVAL);
    assert_eq!(
        pair.pump.system().state(),
        LimitationState::Limited,
        "the guard put the limit back"
    );
    assert!(pair.accepted().len() >= 2);
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::active(3_000.0)
    );
}

/// §14a EnWG, via LPC implementation guide §4.1.5: both ends keep the record that a
/// limitation was received and applied.
///
/// The evidence is the pair — the `msgCounter` of the write and the answer that names
/// it — so both sides' records have to agree on that number.
#[test]
fn both_sides_keep_the_record_section_14a_asks_for() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();
    let device = pair.device();

    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    let written = pair.guard.audit().last().expect("the guard kept a record");
    assert_eq!(written.write.expect("a readable limit").watts(), 3_000.0);
    assert!(written.outcome.is_accepted());
    assert_eq!(written.peer.as_ref(), Some(&device));

    let applied = pair.pump.audit().last().expect("the system kept one too");
    assert!(applied.outcome.is_accepted());
    assert_eq!(applied.write.expect("a readable limit").watts(), 3_000.0);

    assert_eq!(
        written.request, applied.request,
        "the two records name the same message, which is what makes them evidence"
    );
}

/// A refusal is evidence too, and the Energy Guard records what it actually saw rather
/// than a reason it has no way to know.
#[test]
fn a_refusal_is_recorded_without_inventing_a_reason() {
    use eebus::usecases::limitation::NackReason;

    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();
    let device = pair.device();

    // Drive the system into the failsafe state, where §2.14 refuses a limit that arrives
    // without a fresh heartbeat — and make the guard write one there.
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));
    let accepted = pair.guard.audit().len();

    pair.now += Duration::from_secs(400);
    pair.pump.handle_timeout(&mut pair.pump_engine, pair.now);
    assert_eq!(pair.pump.system().state(), LimitationState::FailsafeState);

    // A write with no heartbeat in front of it: the guard's heartbeat producer is not
    // ticked, so the ordering gate refuses.
    let client = pair.guard_client();
    let server = pair.pump_load_control();
    pair.guard_engine.write(
        &server,
        &client,
        eebus::model::CmdData::LoadControlLimitListData(eebus::model::LoadControlLimitListData {
            load_control_limit_data: Some(vec![eebus::model::LoadControlLimitData {
                limit_id: Some(limitation::LIMIT_ID),
                is_limit_active: Some(true),
                value: eebus::model::ScaledNumber::from_f64(2_000.0, 0),
                ..Default::default()
            }]),
        }),
        true,
        pair.now,
    );
    pair.settle();

    let refused: Vec<_> = pair
        .pump
        .audit()
        .records()
        .filter(|r| !r.outcome.is_accepted())
        .cloned()
        .collect();
    assert!(!refused.is_empty(), "the system recorded the refusal");
    assert_eq!(
        refused[0].outcome,
        eebus::usecases::limitation::WriteOutcome::Rejected(NackReason::NoRecentHeartbeat),
        "and it knows why, because it is the one that decided"
    );

    // The guard sees only `errorNumber` 7, so that is all it records.
    let guard_refusals: Vec<_> = pair
        .guard
        .audit()
        .records()
        .filter(|r| !r.outcome.is_accepted())
        .cloned()
        .collect();
    for record in &guard_refusals {
        assert_eq!(
            record.outcome,
            eebus::usecases::limitation::WriteOutcome::Rejected(NackReason::Unstated),
            "a guard that named a reason would be naming one it invented"
        );
        assert!(
            record
                .basis
                .as_deref()
                .is_some_and(|b| b.contains("errorNumber")),
            "what it did see is written down: {record:?}"
        );
    }
    assert!(
        pair.guard.audit().len() >= accepted,
        "the guard's own record only grows"
    );
}

/// Scenario 4, both halves. The Controllable System publishes the nominal maximum it can
/// draw and the Energy Guard reads it in the pre-scenario exchange — which is the whole
/// point of the scenario, because an operator that works in percentages has nothing to
/// multiply until it arrives ([LPC-041], LPC Table 27).
#[test]
fn lpc_041_the_guard_learns_the_nominal_maximum_before_it_writes() {
    let mut pair = Pair::new();
    pair.commission();

    let device = pair.device();
    assert_eq!(
        pair.guard.nominal_max(&device),
        Some(NominalMax::Device(NOMINAL_MAX_WATTS)),
        "an appliance publishes its nameplate, not a contract"
    );
    assert!(
        pair.reports
            .iter()
            .any(|report| matches!(report, GuardEvent::ConstraintsLearned { .. })),
        "the guard reports it, so an application can act on it: {:?}",
        pair.reports
    );
}

/// The other half of [LPC-041]/[LPC-042]: an energy manager publishes what its contract
/// allows and *not* a nameplate, because it has none. LPC UC TS §2.6.4.1 says SHALL for
/// both halves of that, and getting it wrong tells the Energy Guard it is limiting a
/// single appliance when it is limiting a household.
#[test]
fn lpc_042_an_energy_manager_publishes_its_contract_and_not_a_nameplate() {
    let device = ControllableSystem::new(
        CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600))
            .with_nominal_max(11_000.0)
            .with_contractual_max(30_000.0)
            .on_cem(),
        Duration::ZERO,
    );
    assert_eq!(
        limitation::nominal_max(&device),
        Some(NominalMax::Contractual(30_000.0)),
        "on a CEM the contract wins, even with a nameplate configured"
    );

    let appliance = ControllableSystem::new(
        CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600))
            .with_nominal_max(11_000.0)
            .with_contractual_max(30_000.0),
        Duration::ZERO,
    );
    assert_eq!(
        limitation::nominal_max(&appliance),
        Some(NominalMax::Device(11_000.0)),
        "off a CEM the nameplate wins"
    );

    // And each publishes under the name its own use case reserves.
    let published = limitation::constraints(&device, lpc::DIRECTION);
    assert_eq!(
        limitation::read_constraints(&published, lpc::DIRECTION),
        Some(NominalMax::Contractual(30_000.0))
    );
    let produced = limitation::constraints(&appliance, eebus::usecases::lpp::DIRECTION);
    assert_eq!(
        limitation::read_constraints(&produced, eebus::usecases::lpp::DIRECTION),
        Some(NominalMax::Device(11_000.0)),
        "LPP names the same value powerProductionNominalMax"
    );
    assert_eq!(
        limitation::read_constraints(&produced, lpc::DIRECTION),
        None,
        "and an LPC guard must not read an LPP characteristic as its own"
    );
}

/// A device that knows neither value publishes an empty list rather than a guess:
/// scenario 4 is `R` for a Controllable System, and a wrong nameplate is worse than none.
#[test]
fn a_device_without_a_nameplate_publishes_nothing_rather_than_zero() {
    let bare = ControllableSystem::new(
        CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600)),
        Duration::ZERO,
    );
    assert_eq!(limitation::nominal_max(&bare), None);
    assert_eq!(
        limitation::read_constraints(
            &limitation::constraints(&bare, lpc::DIRECTION),
            lpc::DIRECTION
        ),
        None
    );
}

/// The invariant a caller's event loop rests on: the instant the guard asks to be woken
/// at always moves.
///
/// A deadline that could never be reached again is not a slow loop, it is a spin — and in
/// a loop that also owns a socket it is a connection that is never read from, which looks
/// exactly like a peer that has gone quiet.
#[test]
fn the_guards_deadline_always_advances() {
    let mut pair = Pair::new();

    // Before anything is attached: the first heartbeat, which §2.11 wants immediately.
    let mut seen = pair.guard.poll_timeout();
    assert_eq!(seen, Duration::ZERO, "due at once, not a minute from now");

    pair.commission();
    let device = pair.device();

    // Through the whole exchange, and past every point where a write is held back.
    for step in 0..40 {
        let due = pair.guard.poll_timeout();
        assert!(
            due >= pair.now || due >= seen,
            "step {step}: the guard asked to be woken at {due:?}, which is behind both \
             the clock ({:?}) and the last answer ({seen:?})",
            pair.now
        );
        seen = due;

        // Drive to whatever it asked for, exactly as a caller would.
        let target = due.max(pair.now + Duration::from_millis(1));
        pair.advance(target - pair.now);

        match step {
            5 => pair
                .guard
                .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
                .expect("a valid limit"),
            20 => pair
                .guard
                .require(&device, None, pair.now)
                .expect("a valid limit"),
            _ => {}
        }
    }
}

/// Implementation guide §2.11 is about what the Controllable System *received* in the
/// sixty seconds before the write, not about what the Energy Guard sent.
///
/// A heartbeat is a notification to subscribers, so one emitted before the peer subscribed
/// reached nobody. A guard that counted it would write a limit the peer must then refuse
/// for want of a heartbeat it never got — and from the operator's side that refusal is
/// indistinguishable from a device declining to be limited.
#[test]
fn ig_2_11_the_heartbeat_that_counts_is_one_the_peer_could_receive() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    assert!(
        pair.refused().is_empty(),
        "the limit was refused: {:?}",
        pair.refused()
    );
    assert_eq!(pair.accepted().len(), 1);
    assert_eq!(pair.pump.system().state(), LimitationState::Limited);
}

/// The plausibility floor the limitation docs recommend for an appliance the control box
/// addresses itself: an active limit below the §14a minimum is refused on a regulatory
/// ground, and everything else is followed.
fn below_the_floor_is_refused(write: &LimitWrite) -> LocalDecision {
    const FLOOR_WATTS: f64 = 4_200.0;
    if write.is_active && write.watts() < FLOOR_WATTS {
        LocalDecision::Reject(RejectReason::Regulatory)
    } else {
        LocalDecision::Apply
    }
}

/// The application refuses a limit by policy: the Energy Guard is told no, the pump keeps
/// what it had, and the refusal is on the record with the peer that asked.
///
/// The binding is what authorises the guard to write; whether to follow the value is the
/// application's, through `handle_event_with`.
#[test]
fn a_limit_below_the_floor_is_refused_by_policy_and_recorded() {
    let mut pair = Pair::new();
    pair.pump_policy = below_the_floor_is_refused;
    pair.commission();
    pair.forget_history();
    let device = pair.device();
    let guard_device = pair.guard_engine.device().address().clone();
    let state = pair.pump.system().state();
    let limit = pair.pump.system().effective_limit();

    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    assert_eq!(pair.refused().len(), 1, "the guard was told no");
    assert!(pair.accepted().is_empty());
    assert_eq!(
        pair.pump.system().state(),
        state,
        "[LPC-907]: a refusal keeps the state"
    );
    assert_eq!(pair.pump.system().effective_limit(), limit);
    assert!(pair.decisions.iter().any(|d| matches!(
        d,
        CsEvent::LimitDecided {
            outcome: WriteOutcome::Rejected(NackReason::CannotApply(RejectReason::Regulatory)),
            ..
        }
    )));

    let record = pair
        .pump
        .audit()
        .last()
        .expect("the refusal is on the record");
    assert_eq!(
        record.outcome,
        WriteOutcome::Rejected(NackReason::CannotApply(RejectReason::Regulatory))
    );
    assert_eq!(record.write.expect("a readable limit").watts(), 3_000.0);
    assert_eq!(
        record.peer.as_ref(),
        Some(&guard_device),
        "with the peer that asked"
    );

    let seen = pair.guard.audit().last().expect("the guard kept a record");
    assert_eq!(seen.outcome, WriteOutcome::Rejected(NackReason::Unstated));
    assert_eq!(seen.request, record.request, "of the same message");
}

/// The same policy follows every limit §14a obliges the appliance to follow: the opening
/// deactivation, and a limit at the floor itself.
#[test]
fn the_floor_policy_accepts_a_deactivation_and_a_limit_at_the_floor() {
    let mut pair = Pair::new();
    pair.pump_policy = below_the_floor_is_refused;
    pair.commission();

    assert_eq!(pair.accepted().len(), 1, "the opening deactivation");
    assert!(!pair.accepted()[0].is_active);
    assert!(pair.refused().is_empty());
    pair.forget_history();

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(4_200.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    assert_eq!(pair.accepted().len(), 1);
    assert!(pair.refused().is_empty());
    assert_eq!(pair.pump.system().state(), LimitationState::Limited);
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::active(4_200.0)
    );
}

/// A limit that is not a quantity never reaches the wire ([LPC/LPP-001]).
///
/// NaN used to become `0 × 10⁰` — a full curtailment — and a negative limit went out
/// as asked, for the device to refuse. The guard refuses both itself, tells the
/// application, and leaves the limit it was already holding in force.
#[test]
fn a_limit_that_is_not_a_quantity_is_refused_before_it_is_sent() {
    let mut pair = Pair::new();
    pair.commission();
    pair.guard
        .require(&pair.device(), Some(LimitWrite::active(4_200.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));
    pair.forget_history();

    let device = pair.device();
    for watts in [f64::NAN, f64::INFINITY, -1.0] {
        let refused = pair
            .guard
            .require(&device, Some(LimitWrite::active(watts)), pair.now)
            .expect_err("refused");
        assert!(refused.watts.is_nan() || refused.watts == watts);
        assert!(
            pair.guard
                .require_all(Some(LimitWrite::active(watts)), pair.now)
                .is_err()
        );
        assert_eq!(
            pair.guard
                .write_failsafe_limit(&mut pair.guard_engine, &device, watts, pair.now),
            None,
            "nor as a failsafe value"
        );
    }
    pair.advance(Duration::from_secs(1));
    assert!(pair.accepted().is_empty(), "nothing new was written");
    assert!(
        pair.refused().is_empty(),
        "and nothing was left for the device to refuse"
    );
    assert_eq!(
        pair.pump.system().effective_limit().watts(),
        Some(4_200.0),
        "the limit already in force stands"
    );
}

/// A limit travels exactly: the value the Energy Guard was given is the value on the wire,
/// the value the Controllable System applies, and the value it reports back.
///
/// The guard used to write at whole watts, so 4 200.5 W arrived as 4 201 W.
#[test]
fn a_fractional_limit_arrives_exactly_as_it_was_written() {
    use eebus::model::ScaledNumber;

    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();
    let device = pair.device();

    let exact = ScaledNumber::new(42_005, -1);
    pair.guard
        .require(&device, Some(LimitWrite::active_scaled(exact)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(1));

    assert!(
        pair.wire()
            .iter()
            .any(|line| line.contains(r#"[{"number":42005},{"scale":-1}]"#)),
        "the wire carries 4200.5, not 4201"
    );
    assert_eq!(pair.pump.system().effective_limit().scaled(), Some(exact));
    assert_eq!(pair.accepted()[0].scaled(), exact);

    // The f64 spelling reaches the same value.
    pair.forget_history();
    pair.guard
        .require(&device, Some(LimitWrite::active(4_200.25)), pair.now)
        .expect("a valid limit");
    pair.advance(eebus::usecases::limitation::MIN_WRITE_INTERVAL + Duration::from_secs(1));
    assert_eq!(
        pair.pump.system().effective_limit().scaled(),
        Some(ScaledNumber::new(420_025, -2))
    );
}

/// 018-limitation-commissioning/SR-020. The actor API takes magnitudes, while both
/// writes and peer readback carry the passive wire sign of the active power limit.
#[test]
fn commissioned_guards_send_directional_limits_and_settle_after_readback() {
    use eebus::model::{CmdClassifier, CmdData, Function, ScaledNumber};
    use eebus::usecases::limitation::Direction;

    for (direction, sign) in [(Direction::Consumption, 1), (Direction::Production, -1)] {
        let mut pair = Pair::with_direction(direction);
        pair.commission();
        let device = pair.device();
        for (requirement, active, magnitude) in [
            (Some(LimitWrite::active(4_000.0)), true, 4_000),
            (
                Some(LimitWrite::active_for(4_000.0, Duration::from_secs(1_200))),
                true,
                4_000,
            ),
            (Some(LimitWrite::active(0.0)), true, 0),
            (None, false, 0),
        ] {
            pair.trace.clear();
            pair.forget_history();
            pair.guard.require(&device, requirement, pair.now).unwrap();
            for _ in 0..5 {
                pair.advance(Duration::from_secs(60));
            }
            assert_eq!(pair.accepted().len(), 1);
            let applied = pair.guard.applied_limit(&device).unwrap();
            assert_eq!(applied.is_active, active);
            assert_eq!(applied.value, ScaledNumber::whole(magnitude));

            let writes: Vec<_> = pair
                .trace
                .iter()
                .filter(|(from, datagram)| {
                    *from == "box"
                        && datagram.header.as_ref().unwrap().cmd_classifier
                            == Some(CmdClassifier::Write)
                })
                .collect();
            assert_eq!(writes.len(), 1);
            let write = round_trip(&writes[0].1);
            let command = &write.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
            let server = pair.pump_load_control();
            let stored = pair
                .pump_engine
                .device()
                .resolve(&server)
                .unwrap()
                .data(&Function::LoadControlLimitListData)
                .unwrap();
            for data in [command.data.as_ref().unwrap(), stored] {
                let CmdData::LoadControlLimitListData(list) = data else {
                    panic!("expected limit list")
                };
                let entry = &list.load_control_limit_data.as_ref().unwrap()[0];
                assert_eq!(entry.value, Some(ScaledNumber::whole(sign * magnitude)));
                assert_eq!(entry.is_limit_active, Some(active));
            }

            // Force an actual SPINE read after the ACK, beyond which the guard treats
            // the peer's report as authoritative. Keep heartbeats fresh for a complete
            // write interval to expose even a rate-limited corrective write.
            pair.trace.clear();
            let client = pair.guard_client();
            pair.guard_engine.read(
                &server,
                &client,
                Function::LoadControlLimitListData,
                pair.now,
            );
            pair.settle();
            for _ in 0..5 {
                pair.advance(Duration::from_secs(60));
            }
            assert!(pair.trace.iter().all(|(from, d)| *from != "box"
                || d.header.as_ref().unwrap().cmd_classifier != Some(CmdClassifier::Write)));
        }
    }
}

/// 018-limitation-commissioning/SR-021. A device may reconnect under the same
/// address. Its previous bindings, metadata and outbound packets must not authorize
/// a write in the new session.
#[test]
fn reconnect_recommissions_before_reapplying_the_grid_requirement() {
    let mut pair = Pair::with_direction(eebus::usecases::lpp::DIRECTION);
    pair.commission();
    let device = pair.device();
    let grid_limit = LimitWrite::active(4_000.0);
    pair.guard
        .require(&device, Some(grid_limit), pair.now)
        .unwrap();
    pair.guard.handle_timeout(&mut pair.guard_engine, pair.now);

    pair.guard_engine.remove_peer(&device);
    let guard_device = pair.guard_engine.device().address().clone();
    pair.pump_engine.remove_peer(&guard_device);
    assert!(pair.guard_engine.poll_transmit().is_none());
    pair.settle();
    assert!(!pair.guard.is_ready(&device));
    assert!(pair.pump.guard().is_none());
    assert_eq!(pair.guard.applied_limit(&device), None);
    assert!(
        pair.guard
            .deferred_requirements()
            .any(|(peer, limit)| peer == &device && limit == Some(grid_limit))
    );

    pair.forget_history();
    pair.commission();
    pair.advance(Duration::from_secs(11));
    assert!(pair.guard.is_ready(&device));
    assert_eq!(pair.accepted(), [grid_limit]);
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::active(4_000.0)
    );
}

#[test]
fn only_the_bound_clients_release_changes_the_identified_guard() {
    use eebus::spine::SpineEvent;

    let mut pair = Pair::new();
    pair.commission();
    let client = pair.guard_client();
    let configuration = pair.pump_engine.device().address_of(&[1], 2);
    let another_client = pair.guard_engine.device().address_of(&[1], 2);
    let unrelated_feature = pair.pump_engine.device().address_of(&[1], 3);

    for (client, server) in [
        (another_client, configuration.clone()),
        (client.clone(), unrelated_feature),
    ] {
        let release = SpineEvent::BindingReleased { client, server };
        pair.pump
            .handle_event(&mut pair.pump_engine, &release, pair.now);
        assert!(pair.pump.guard().is_some());
    }
    let release = SpineEvent::BindingReleased {
        client: client.clone(),
        server: configuration.clone(),
    };
    pair.pump
        .handle_event(&mut pair.pump_engine, &release, pair.now);
    assert!(pair.pump.guard().is_none());
    let rebound = SpineEvent::BindingGranted {
        client,
        server: configuration,
    };
    pair.pump
        .handle_event(&mut pair.pump_engine, &rebound, pair.now);
    assert!(pair.pump.guard().is_some());
}
