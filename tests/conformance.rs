//! The EEBUS High-Level Test Specifications, run against this crate.
//!
//! `eebus::conformance::CATALOGUE` is the 203 abstract test cases of the four certifiable
//! use cases as data. This file is the other half: the tests that actually exercise them,
//! each named after the `ATC_…` identifier a laboratory would report, and a coverage
//! number computed from the two.
//!
//! # What a claim here means, and what it does not
//!
//! A test in this file drives *this crate* through the abstract test case's steps and
//! checks the expected result. That is not the same as passing the laboratory's version,
//! which drives a device: a device has a power supply, a factory reset, persistent
//! storage and an actual heat pump attached, and roughly a third of the abstract test
//! cases are about those rather than about the protocol. Those are recorded as
//! [`Claim::Device`] with the reason, and they are *not* counted as covered — a coverage
//! number that quietly counted "we cannot test this" would be worth nothing.
//!
//! # LPC and LPP
//!
//! The two specifications state the same 51 abstract test cases under different prefixes,
//! and this crate answers them with one state machine: `ControllableSystem` has no
//! direction in it, because a limit is a number of watts either way. A test here
//! therefore covers the LPC identifier and its LPP twin, and the part that genuinely does
//! differ — what each use case *publishes* — is checked separately in
//! `limitation_over_the_wire.rs` and by
//! [`lpc_and_lpp_differ_only_in_what_they_publish`].
//!
//! # Running it
//!
//! ```sh
//! cargo test --test conformance -- --nocapture coverage
//! ```
//!
//! prints the table.

// The catalogue is optional — a firmware build leaves tens of kilobytes of test-case
// descriptions out — so the suite that measures against it is optional too.
#![cfg(feature = "conformance")]

mod common;

use core::time::Duration;

use eebus::conformance::{self, Coverage};
use eebus::model::{
    ElectricalConnectionPhaseName as Phase, EntityType, Function, MeasurementValueState,
};
use eebus::usecases::descriptor::UseCaseDescriptor;
use eebus::usecases::descriptor::{actors, names};
use eebus::usecases::limitation::{
    self, ControllableSystem, CsConfig, EffectiveLimit, LimitWrite, LimitationState, LocalDecision,
    NackReason, NominalMax, RejectReason, WriteOutcome,
};
use eebus::usecases::monitoring::{Measurand, MonitoredUnit, Naming, Quantity, ReadingState};
use eebus::usecases::signals::Signals;
use eebus::usecases::{lpc, lpp, mgcp, mpc};

use common::Pair;

// ---- the data sets of LPC/LPP HLTS §6.11 -------------------------------------
//
// The specification derives them from a range the manufacturer declares. This device
// declares [APCLmin, APCLmax] = [0 W, 11 000 W], so delta = 0.05 * 11 000 = 550 W.

const APCL_MIN: f64 = 0.0;
const APCL_MAX: f64 = 11_000.0;
const DELTA: f64 = 0.05 * (APCL_MAX - APCL_MIN);

/// Below the declared minimum, floored at zero because `APCLmin - delta` is negative.
const APCL_01: f64 = 0.0;
/// Just above the minimum.
const APCL_02: f64 = APCL_MIN + DELTA;
/// Freely chosen inside the range.
const APCL_03: f64 = 5_000.0;
/// Just below the maximum.
const APCL_04: f64 = APCL_MAX - DELTA;
/// Above the maximum — still a valid limit, and still to be accepted.
const APCL_05: f64 = APCL_MAX + DELTA;
/// Invalid: minus a thousand watts.
const APCL_06: f64 = -1_000.0;

/// The duration parameter set to sixty seconds.
const APCL_DUR_01: Duration = Duration::from_secs(60);

/// The pre-configured Failsafe Consumption Active Power Limit.
const FCAPL_03: f64 = 4_200.0;
/// An invalid failsafe limit.
const FCAPL_06: f64 = -1_000.0;

/// One hour fifty-four minutes: below the two-hour floor of [LPC/LPP-022].
const FCAPL_DUR_01: Duration = Duration::from_secs(6_840);
/// Between the pre-configured value and this device's maximum.
const FCAPL_DUR_02: Duration = Duration::from_secs(4 * 3_600);
/// Five per cent above this device's maximum.
const FCAPL_DUR_03: Duration = Duration::from_secs(25 * 3_600 + 720);

/// The pre-configured Failsafe Duration Minimum (PFSDM).
const PFSDM: Duration = Duration::from_secs(2 * 3_600);
/// The largest Failsafe Duration Minimum this device processes (MFSDM).
const MFSDM: Duration = Duration::from_secs(24 * 3_600);

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

// ---- the test configurations of LPC/LPP HLTS §6.5.4 ---------------------------

/// `CF_CS_Init`: the state a Controllable System is in after a (re)start.
fn cf_cs_init() -> (ControllableSystem, Duration) {
    let now = Duration::ZERO;
    let cs = ControllableSystem::new(
        CsConfig::new(FCAPL_03, PFSDM).with_nominal_max(APCL_MAX),
        now,
    );
    assert_eq!(cs.state(), LimitationState::Init);
    (cs, now)
}

/// `CF_CS_UnlCntrl`: under the Energy Guard's control, with no limit in force.
fn cf_cs_unl_cntrl() -> (ControllableSystem, Duration) {
    let (mut cs, mut now) = cf_cs_init();
    now += secs(10);
    cs.on_heartbeat(now);
    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
    (cs, now)
}

/// `CF_CS_Limited_wo_dur`: limited, by a limit with no duration on it.
fn cf_cs_limited_wo_dur() -> (ControllableSystem, Duration) {
    let (mut cs, mut now) = cf_cs_init();
    now += secs(10);
    cs.on_heartbeat(now);
    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_03), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::Limited);
    (cs, now)
}

/// `CF_CS_FS`: the failsafe state, reached the way a real one is — by the Energy Guard
/// falling silent for two minutes.
fn cf_cs_fs() -> (ControllableSystem, Duration) {
    let (mut cs, mut now) = cf_cs_unl_cntrl();
    now += limitation::HEARTBEAT_TIMEOUT;
    cs.handle_timeout(now);
    assert_eq!(cs.state(), LimitationState::FailsafeState);
    (cs, now)
}

/// `CF_CS_UnlAuto`: the Energy Guard has been absent long enough to be given up on.
fn cf_cs_unl_auto() -> (ControllableSystem, Duration) {
    let (mut cs, mut now) = cf_cs_fs();
    now += PFSDM;
    cs.handle_timeout(now);
    assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
    (cs, now)
}

// ---- 7 Abstract test cases for EG --------------------------------------------

/// The Energy Guard sends its heartbeats regularly; two consecutive ones are at most
/// sixty seconds apart. [LPC-TS-006]
#[test]
fn atc_lpc_com_pt_egheartbeat_001() {
    let mut pair = Pair::new();
    pair.commission();

    let diagnosis = pair.guard_engine.device().address_of(&[1], 2);
    let mut beats: Vec<(u64, Duration)> = Vec::new();
    for _ in 0..400 {
        pair.advance(secs(1));
        let counter = pair
            .guard_engine
            .device()
            .resolve(&diagnosis)
            .and_then(|feature| feature.data(&eebus::model::Function::DeviceDiagnosisHeartbeatData))
            .and_then(limitation::read_heartbeat);
        if let Some(counter) = counter
            && beats.last().map(|(seen, _)| *seen) != Some(counter)
        {
            beats.push((counter, pair.now));
        }
        if beats.len() >= 5 {
            break;
        }
    }
    assert!(
        beats.len() >= 5,
        "five heartbeats in five minutes: {beats:?}"
    );
    for window in beats.windows(2) {
        let gap = window[1].1.saturating_sub(window[0].1);
        assert!(gap <= secs(60), "{gap:?} between two heartbeats");
    }
}

/// The Controllable System sends its heartbeats regularly too; scenario 3 runs both ways.
/// [LPC-TS-007]
#[test]
fn atc_lpc_com_pt_csheartbeat_001() {
    let mut pair = Pair::new();
    pair.commission();

    let diagnosis = pair.pump_engine.device().address_of(&[1], 3);
    let mut beats: Vec<(u64, Duration)> = Vec::new();
    for _ in 0..400 {
        pair.advance(secs(1));
        let counter = pair
            .pump_engine
            .device()
            .resolve(&diagnosis)
            .and_then(|feature| feature.data(&eebus::model::Function::DeviceDiagnosisHeartbeatData))
            .and_then(limitation::read_heartbeat);
        if let Some(counter) = counter
            && beats.last().map(|(seen, _)| *seen) != Some(counter)
        {
            beats.push((counter, pair.now));
        }
        if beats.len() >= 5 {
            break;
        }
    }
    assert!(
        beats.len() >= 5,
        "five heartbeats in five minutes: {beats:?}"
    );
    for window in beats.windows(2) {
        let gap = window[1].1.saturating_sub(window[0].1);
        assert!(gap <= secs(60), "{gap:?} between two heartbeats");
    }
}

/// After the connection to a Controllable System is restored, the Energy Guard sends a
/// heartbeat and a following limit write — whether or not the grid needs anything, because
/// until it lands the system is not controllable at all. [LPC-TS-030]
#[test]
fn atc_lpc_com_pt_egconnection_002() {
    let mut pair = Pair::new();
    pair.commission();

    // The Controllable System loses the Energy Guard and falls into the failsafe state.
    let device = pair.device();
    pair.guard.detach(&device);
    pair.now += secs(200);
    pair.pump.handle_timeout(&mut pair.pump_engine, pair.now);
    assert_eq!(
        pair.pump.system().state(),
        LimitationState::FailsafeState,
        "CF_CS_FS"
    );
    pair.forget_history();

    // The connection comes back, and the guard runs the opening sequence again.
    let remote = pair
        .guard_engine
        .peer(&device)
        .expect("the peer is still known");
    let peer = limitation::locate(remote, lpc::DIRECTION).expect("a Controllable System");
    pair.guard.attach(&mut pair.guard_engine, peer, pair.now);
    pair.settle();
    pair.advance(secs(11));

    assert_eq!(
        pair.pump.system().state(),
        LimitationState::UnlimitedControlled,
        "a heartbeat and a following limit write, within sixty seconds of each other"
    );
    assert_eq!(pair.accepted().len(), 1);
}

/// The Energy Guard causes the Controllable System to leave `unlimited/controlled` for
/// `limited` on an external stimulus, with no duration on the limit. [LPC-TS-001]
#[test]
fn atc_lpc_com_pt_egmessages_001() {
    let mut pair = Pair::new();
    pair.commission();

    // CF_CS_UnlCntrl: the opening deactivation every Energy Guard sends. Nothing here
    // asks for it — implementation guide §2.11 requires the guard to send it as soon as
    // the bindings settle, and it attaches itself to a peer that announces the
    // Controllable System, so the pre-condition establishes itself.
    let device = pair.device();
    pair.advance(secs(1));
    assert_eq!(
        pair.pump.system().state(),
        LimitationState::UnlimitedControlled
    );

    // The external stimulus: the grid needs the house held to 5 kW.
    pair.advance(limitation::MIN_WRITE_INTERVAL);
    pair.guard
        .require(&device, Some(LimitWrite::active(APCL_03)), pair.now);
    pair.advance(secs(1));

    assert_eq!(pair.pump.system().state(), LimitationState::Limited);
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::Active(APCL_03)
    );
    assert_eq!(
        pair.accepted().len(),
        2,
        "the opening deactivation the guard sends itself, and the limit that answers the \
         stimulus"
    );
}

/// The Energy Guard sends valid messages over an extended period, and a Controllable
/// System that switches state on each one keeps up. [LPC-TS-001], [LPC-TS-001/2],
/// [LPC-TS-002]
#[test]
fn atc_lpc_com_pt_egmessages_003() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.device();
    pair.guard.require(&device, None, pair.now);
    pair.advance(secs(1));

    for (round, limit) in [APCL_02, APCL_03, APCL_04, APCL_05].iter().enumerate() {
        pair.guard
            .require(&device, Some(LimitWrite::active(*limit)), pair.now);
        // §2.10 holds a guard to one write every five minutes.
        pair.advance(limitation::MIN_WRITE_INTERVAL + secs(1));
        assert_eq!(
            pair.pump.system().effective_limit(),
            EffectiveLimit::Active(*limit),
            "round {round}"
        );
    }
    assert_eq!(
        pair.accepted().len(),
        5,
        "the opening deactivation, then four limits"
    );
    assert!(pair.refused().is_empty());
}

/// The Energy Guard writes the failsafe values and they take. [LPC-TS-003],
/// [LPC-TS-011/1], [LPC-TS-013/1]
#[test]
fn atc_lpc_com_pt_egmessages_004() {
    let mut pair = Pair::new();
    pair.commission();
    let device = pair.device();

    // The failsafe values may only be written once the opening sequence has completed
    // ([LPC-TS-037]), which is the deactivation the guard sends first.
    pair.guard.require(&device, None, pair.now);
    pair.advance(secs(1));

    pair.guard
        .write_failsafe_limit(&mut pair.guard_engine, &device, 3_500.0, pair.now);
    pair.settle();
    assert_eq!(pair.pump.system().config().failsafe_watts, 3_500.0);

    pair.guard
        .write_failsafe_duration(&mut pair.guard_engine, &device, FCAPL_DUR_02, pair.now);
    pair.settle();
    assert_eq!(pair.pump.system().config().failsafe_duration, FCAPL_DUR_02);
}

// ---- 8 Abstract test cases for CS: connection ---------------------------------

/// A limit write is not evaluated until a heartbeat has been received; once one has, the
/// same write is accepted. [LPC-TS-004], [LPC-TS-036]
#[test]
fn atc_lpc_com_nt_csconnection_001() {
    let (mut cs, mut now) = cf_cs_init();

    now += secs(5);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NoRecentHeartbeat)
    );
    assert_eq!(cs.state(), LimitationState::Init, "and nothing moved");

    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
}

/// No other data point may be written until a heartbeat *and* a limit write have gone
/// through. [LPC-TS-003], [LPC-TS-036], [LPC-TS-037], [LPC-TS-038]
#[test]
fn atc_lpc_com_pt_csconnection_002() {
    let (mut cs, mut now) = cf_cs_init();

    now += secs(5);
    cs.on_heartbeat(now);
    now += secs(1);
    assert_eq!(
        cs.on_failsafe_limit_write(FCAPL_03, now),
        WriteOutcome::Rejected(NackReason::SequenceIncomplete),
        "the heartbeat alone does not open the other data points"
    );

    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);

    now += secs(1);
    assert_eq!(
        cs.on_failsafe_limit_write(3_800.0, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.config().failsafe_watts, 3_800.0);
}

/// Only values at or above zero are accepted, for the limit and for the failsafe limit
/// alike. [LPC-TS-005], [LPC-TS-018], [LPC-TS-038]
#[test]
fn atc_lpc_com_pt_csconnection_003() {
    let (mut cs, mut now) = cf_cs_init();

    now += secs(5);
    cs.on_heartbeat(now);

    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_06), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NegativeValue)
    );
    assert_eq!(
        cs.state(),
        LimitationState::UnlimitedControlled,
        "[LPC-TS-018]: a refused limit still establishes control"
    );

    now += secs(1);
    assert_eq!(
        cs.on_failsafe_limit_write(FCAPL_06, now),
        WriteOutcome::Rejected(NackReason::NegativeValue)
    );

    now += secs(1);
    assert_eq!(
        cs.on_failsafe_limit_write(FCAPL_03, now),
        WriteOutcome::Accepted
    );
}

/// The Failsafe Duration Minimum is gated on the opening sequence too. [LPC-TS-005],
/// [LPC-TS-037]
#[test]
fn atc_lpc_com_pt_csconnection_004() {
    let (mut cs, mut now) = cf_cs_init();

    now += secs(5);
    cs.on_heartbeat(now);
    now += secs(1);
    assert_eq!(
        cs.on_failsafe_duration_write(FCAPL_DUR_02, now),
        WriteOutcome::Rejected(NackReason::SequenceIncomplete)
    );
    assert_eq!(cs.config().failsafe_duration, PFSDM, "unchanged");

    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    now += secs(1);
    assert_eq!(
        cs.on_failsafe_duration_write(FCAPL_DUR_02, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.config().failsafe_duration, FCAPL_DUR_02);
}

/// A Failsafe Duration Minimum above this device's own maximum is *evaluated* — accepted
/// and clamped, or refused, but not ignored. This implementation refuses, which is the
/// second of the two branches the test case permits. [LPC-TS-014], [LPC-TS-015],
/// [LPC-TS-015/1], [LPC-TS-016]
#[test]
fn atc_lpc_com_pt_csconnection_005() {
    let (mut cs, mut now) = cf_cs_unl_cntrl();

    now += secs(1);
    assert_eq!(
        cs.on_failsafe_duration_write(FCAPL_DUR_03, now),
        WriteOutcome::Rejected(NackReason::DurationOutOfRange),
        "{FCAPL_DUR_03:?} is above the 24-hour ceiling of [LPC-022/3]"
    );
    assert_eq!(
        cs.config().failsafe_duration,
        PFSDM,
        "[LPC-022/5]: the device reports the value it actually holds"
    );

    // And the ceiling is the one the device declared, not merely the specification's.
    let mut modest = ControllableSystem::new(
        CsConfig {
            failsafe_duration_max: secs(6 * 3_600),
            ..CsConfig::new(FCAPL_03, PFSDM)
        },
        Duration::ZERO,
    );
    modest.on_heartbeat(secs(1));
    modest.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, secs(2));
    assert_eq!(
        modest.on_failsafe_duration_write(secs(12 * 3_600), secs(3)),
        WriteOutcome::Rejected(NackReason::DurationOutOfRange)
    );
    assert!(
        MFSDM > secs(6 * 3_600),
        "the specification's own ceiling is higher"
    );
}

/// Every valid limit value is evaluated and accepted, including one above the device's
/// declared maximum. [LPC-TS-001], [LPC-TS-035], [LPC-TS-035/4]
#[test]
fn atc_lpc_com_pt_csconnection_007() {
    for value in [APCL_01, APCL_02, APCL_03, APCL_04, APCL_05] {
        let (mut cs, mut now) = cf_cs_unl_cntrl();
        now += secs(1);
        assert_eq!(
            cs.on_limit_write(&LimitWrite::active(value), LocalDecision::Apply, now),
            WriteOutcome::Accepted,
            "{value} W"
        );
        assert_eq!(cs.state(), LimitationState::Limited);
        assert_eq!(cs.effective_limit(), EffectiveLimit::Active(value));
    }
}

/// The failsafe limit and duration are evaluated correctly across their data sets:
/// FCAPL_DUR_01 is below the two-hour floor and refused, FCAPL_DUR_02 is inside the range
/// and accepted, FCAPL_DUR_03 is above the ceiling and refused. [LPC-TS-001],
/// [LPC-TS-015/1], [LPC-TS-016], [LPC-TS-038]
#[test]
fn atc_lpc_com_pt_csconnection_008() {
    for limit in [0.0, APCL_02, FCAPL_03, APCL_04, APCL_05] {
        let (mut cs, mut now) = cf_cs_unl_cntrl();
        now += secs(1);
        assert_eq!(
            cs.on_failsafe_limit_write(limit, now),
            WriteOutcome::Accepted,
            "{limit} W"
        );
        assert_eq!(cs.config().failsafe_watts, limit);
    }

    let expectations = [
        (FCAPL_DUR_01, false),
        (FCAPL_DUR_02, true),
        (FCAPL_DUR_03, false),
    ];
    for (duration, accepted) in expectations {
        let (mut cs, mut now) = cf_cs_unl_cntrl();
        now += secs(1);
        assert_eq!(
            cs.on_failsafe_duration_write(duration, now).is_accepted(),
            accepted,
            "{duration:?}"
        );
    }
}

// ---- 9 Abstract test cases for CS: the states ---------------------------------

/// A system that has just started is limited by the failsafe limit and its limit is
/// deactivated. [LPC-TS-009/3], [LPC-TS-011], [LPC-TS-017], [LPC-TS-019]
#[test]
fn atc_lpc_com_pt_csinit_001() {
    let (cs, _) = cf_cs_init();

    assert_eq!(cs.effective_limit(), EffectiveLimit::Failsafe(FCAPL_03));
    assert!(!cs.is_limit_active());
    assert_eq!(
        cs.config().failsafe_watts,
        FCAPL_03,
        "[LPC-TS-019]: the pre-configured value, nothing else"
    );
}

/// A limited system rejects an invalid limit and stays limited. [LPC-TS-009/1],
/// [LPC-TS-024], [LPC-TS-035/1]
#[test]
fn atc_lpc_com_nt_cslimited_001() {
    let (mut cs, mut now) = cf_cs_limited_wo_dur();
    assert!(cs.is_limit_active());

    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_06), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NegativeValue)
    );
    assert_eq!(cs.state(), LimitationState::Limited);
    assert_eq!(cs.effective_limit(), EffectiveLimit::Active(APCL_03));
}

/// A limited system keeps its state and keeps accepting limits even when heartbeats stop
/// — until the 120-second window runs out. [LPC-TS-001/2], [LPC-TS-002]
#[test]
fn atc_lpc_com_pt_cslimited_002() {
    let (mut cs, mut now) = cf_cs_limited_wo_dur();

    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(90);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_04), LocalDecision::Apply, now),
        WriteOutcome::Accepted,
        "ninety seconds of silence does not close a limited system's door"
    );
    assert_eq!(cs.state(), LimitationState::Limited);
    assert_eq!(cs.effective_limit(), EffectiveLimit::Active(APCL_04));

    now += secs(1);
    cs.on_heartbeat(now);
    cs.handle_timeout(now);
    assert_eq!(cs.state(), LimitationState::Limited);
}

/// An unlimited but controlled system rejects an invalid limit and stays where it is.
/// [LPC-TS-009], [LPC-TS-009/3], [LPC-TS-023]
#[test]
fn atc_lpc_com_nt_csunlcntrl_001() {
    let (mut cs, mut now) = cf_cs_unl_cntrl();
    assert!(!cs.is_limit_active());

    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_06), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NegativeValue)
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
    assert_eq!(cs.effective_limit(), EffectiveLimit::None);
}

/// A Controllable System on a CEM provides the Contractual Consumption Nominal Max, and
/// does *not* provide the Power Consumption Nominal Max. [LPC-TS-010/3], [LPC-TS-010/4],
/// [LPC-TS-038], [LPC-TS-039]
#[test]
fn atc_lpc_com_pt_csunlcntrl_002() {
    let manager = ControllableSystem::new(
        CsConfig::new(FCAPL_03, PFSDM)
            .with_nominal_max(APCL_MAX)
            .with_contractual_max(30_000.0)
            .on_cem(),
        Duration::ZERO,
    );

    let reported = limitation::nominal_max(&manager).expect("a CEM publishes its contract");
    assert_eq!(reported, NominalMax::Contractual(30_000.0));
    assert!(
        reported.watts() >= 0.0,
        "[LPC-TS-038]: the value is at or above zero"
    );

    let published = limitation::constraints(&manager, lpc::DIRECTION);
    assert_eq!(
        limitation::read_constraints(&published, lpc::DIRECTION),
        Some(NominalMax::Contractual(30_000.0))
    );
    let signals = manager.signals(lpc::DIRECTION);
    assert!(
        signals
            .get("lpc:nominalMax")
            .is_some_and(|value| value.is_absent()),
        "the Power Consumption Nominal Max is not supported on a CEM"
    );
}

/// A Controllable System that is not a CEM provides the Power Consumption Nominal Max,
/// and not the contractual one. [LPC-TS-010/1], [LPC-TS-010/2], [LPC-TS-038],
/// [LPC-TS-040]
#[test]
fn atc_lpc_com_pt_csunlcntrl_003() {
    let (cs, _) = cf_cs_init();

    assert_eq!(
        limitation::nominal_max(&cs),
        Some(NominalMax::Device(APCL_MAX))
    );
    let published = limitation::constraints(&cs, lpc::DIRECTION);
    assert_eq!(
        limitation::read_constraints(&published, lpc::DIRECTION),
        Some(NominalMax::Device(APCL_MAX))
    );
    let signals = cs.signals(lpc::DIRECTION);
    assert!(
        signals
            .get("lpc:contractualMax")
            .is_some_and(|value| value.is_absent())
    );
}

/// In the failsafe state, nothing is evaluated until a heartbeat and a following limit
/// write arrive within sixty seconds of each other. [LPC-TS-033], [LPC-TS-036],
/// [LPC-TS-037]
#[test]
fn atc_lpc_com_pt_csfs_001() {
    let (mut cs, mut now) = cf_cs_fs();

    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(90);

    // The heartbeat has gone stale, so none of the three writes is evaluated.
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NoRecentHeartbeat)
    );
    assert_eq!(cs.state(), LimitationState::FailsafeState);
    // The failsafe values are refused for the older reason of the two: entering the
    // failsafe state re-armed the sequence gate, and no limit write has completed it.
    assert_eq!(
        cs.on_failsafe_limit_write(FCAPL_03, now),
        WriteOutcome::Rejected(NackReason::SequenceIncomplete)
    );
    assert_eq!(
        cs.on_failsafe_duration_write(FCAPL_DUR_02, now),
        WriteOutcome::Rejected(NackReason::SequenceIncomplete)
    );
    assert_eq!(cs.state(), LimitationState::FailsafeState);

    // A fresh heartbeat, and the same write goes through.
    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
}

/// The failsafe state is held for at least the Failsafe Duration Minimum.
/// [LPC-TS-012], [LPC-TS-013]
#[test]
fn atc_lpc_com_pt_csfs_002() {
    let (mut cs, entered) = cf_cs_fs();

    let mut now = entered;
    while now < entered + PFSDM {
        now += secs(600);
        cs.handle_timeout(now.min(entered + PFSDM - secs(1)));
        assert_eq!(
            cs.state(),
            LimitationState::FailsafeState,
            "left the failsafe state after {:?}",
            now - entered
        );
        assert_eq!(cs.effective_limit(), EffectiveLimit::Failsafe(FCAPL_03));
    }

    cs.handle_timeout(entered + PFSDM);
    assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
}

/// In the failsafe state a Failsafe Duration Minimum write is refused and the state does
/// not move — a secondary data point is not the limit write that establishes control.
/// [LPC-TS-009], [LPC-TS-009/3]
#[test]
fn atc_lpc_com_pt_csfs_003() {
    let (mut cs, mut now) = cf_cs_fs();
    assert!(!cs.is_limit_active());

    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);
    assert_eq!(
        cs.on_failsafe_duration_write(FCAPL_DUR_02, now),
        WriteOutcome::Rejected(NackReason::SequenceIncomplete)
    );
    assert_eq!(cs.state(), LimitationState::FailsafeState);
}

/// In `unlimited/autonomous`, nothing is evaluated until a heartbeat and a following
/// limit write arrive in time. [LPC-TS-033], [LPC-TS-036], [LPC-TS-037]
#[test]
fn atc_lpc_com_nt_csunlauto_001() {
    let (mut cs, mut now) = cf_cs_unl_auto();

    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(90);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NoRecentHeartbeat)
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
    assert_eq!(
        cs.on_failsafe_limit_write(FCAPL_03, now),
        WriteOutcome::Rejected(NackReason::SequenceIncomplete),
        "the sequence gate re-armed when the guard was given up on"
    );

    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);

    now += secs(1);
    assert_eq!(
        cs.on_failsafe_limit_write(FCAPL_03, now),
        WriteOutcome::Accepted
    );
}

/// In `unlimited/autonomous` the limit is deactivated and the system stays within its
/// nominal maximum. [LPC-TS-009/3], [LPC-TS-010], [LPC-TS-038]
#[test]
fn atc_lpc_com_pt_csunlauto_002() {
    let (cs, _) = cf_cs_unl_auto();

    assert!(!cs.is_limit_active());
    assert_eq!(cs.effective_limit(), EffectiveLimit::None);

    // A device's own nameplate is a physical ceiling, not something this crate enforces —
    // but an energy manager's contract is, and `power_ceiling` is where it is enforced.
    let mut manager = ControllableSystem::new(
        CsConfig::new(FCAPL_03, PFSDM)
            .with_contractual_max(30_000.0)
            .on_cem(),
        Duration::ZERO,
    );
    manager.on_heartbeat(secs(1));
    manager.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, secs(2));
    manager.handle_timeout(secs(2) + limitation::HEARTBEAT_TIMEOUT);
    manager.handle_timeout(secs(2) + limitation::HEARTBEAT_TIMEOUT + PFSDM);
    assert_eq!(manager.state(), LimitationState::UnlimitedAutonomous);
    assert_eq!(
        manager.power_ceiling(),
        Some(30_000.0),
        "unlimited by the Energy Guard is not unlimited by the contract"
    );
}

// ---- 10 Abstract test cases for CS: the thirteen transitions ------------------

/// `init` → `unlimited/controlled`, by a refused limit. [LPC-TS-018], [LPC-TS-035/1]
#[test]
fn atc_lpc_com_pt_cstransition1_001() {
    let (mut cs, mut now) = cf_cs_init();
    now += secs(5);
    cs.on_heartbeat(now);
    now += secs(1);

    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_06), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NegativeValue)
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
}

/// `init` → `unlimited/controlled`, by an accepted deactivation. [LPC-TS-021]
#[test]
fn atc_lpc_com_pt_cstransition1_002() {
    let (cs, _) = cf_cs_unl_cntrl();
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
}

/// `init` → `limited`, by an accepted activation with a duration. [LPC-TS-020]
#[test]
fn atc_lpc_com_pt_cstransition2_001() {
    for value in [APCL_02, APCL_03, APCL_04] {
        let (mut cs, mut now) = cf_cs_init();
        now += secs(5);
        cs.on_heartbeat(now);
        now += secs(1);
        assert_eq!(
            cs.on_limit_write(
                &LimitWrite::active_for(value, APCL_DUR_01),
                LocalDecision::Apply,
                now
            ),
            WriteOutcome::Accepted,
            "{value} W"
        );
        assert_eq!(cs.state(), LimitationState::Limited);
        assert_eq!(cs.effective_limit(), EffectiveLimit::Active(value));
    }
}

/// `init` → `unlimited/autonomous`, because nothing arrived at all. [LPC-TS-022],
/// [LPC-TS-022/1]
#[test]
fn atc_lpc_com_pt_cstransition3_001() {
    let (mut cs, now) = cf_cs_init();
    cs.handle_timeout(now + secs(130));
    assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
}

/// `init` → `unlimited/autonomous`, because a heartbeat arrived but no limit followed.
/// [LPC-TS-022], [LPC-TS-022/1]
#[test]
fn atc_lpc_com_pt_cstransition3_002() {
    let (mut cs, mut now) = cf_cs_init();
    now += secs(5);
    cs.on_heartbeat(now);
    cs.handle_timeout(now + secs(130));
    assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
}

/// `unlimited/controlled` → `limited`. [LPC-TS-027]
#[test]
fn atc_lpc_com_pt_cstransition4_001() {
    let (mut cs, mut now) = cf_cs_unl_cntrl();
    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_03), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::Limited);
}

/// `unlimited/controlled` → `failsafe state`, after 120 seconds without a heartbeat.
/// [LPC-TS-028]
#[test]
fn atc_lpc_com_pt_cstransition5_001() {
    let (mut cs, _) = cf_cs_unl_cntrl();

    // The window runs from the last heartbeat, not from entering the state, which is what
    // the machine's own deadline reports.
    let due = cs
        .poll_timeout()
        .expect("a controlled system is always waiting");
    cs.handle_timeout(due - secs(1));
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled, "not yet");

    cs.handle_timeout(due);
    assert_eq!(cs.state(), LimitationState::FailsafeState);
    assert_eq!(cs.effective_limit(), EffectiveLimit::Failsafe(FCAPL_03));
}

/// `limited` → `unlimited/controlled`, because the limit's duration expired.
/// [LPC-TS-001/1], [LPC-TS-008], [LPC-TS-008/1], [LPC-TS-025]
#[test]
fn atc_lpc_com_pt_cstransition6_001() {
    let (mut cs, mut now) = cf_cs_init();
    now += secs(5);
    cs.on_heartbeat(now);
    now += secs(1);
    cs.on_limit_write(
        &LimitWrite::active_for(APCL_03, APCL_DUR_01),
        LocalDecision::Apply,
        now,
    );
    assert_eq!(cs.state(), LimitationState::Limited);

    let expiry = now + APCL_DUR_01;
    cs.handle_timeout(expiry - secs(1));
    assert_eq!(cs.state(), LimitationState::Limited, "not yet");

    // The heartbeat has to keep coming, or the failsafe would fire first.
    cs.on_heartbeat(expiry - secs(1));
    cs.handle_timeout(expiry);
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
    assert_eq!(cs.effective_limit(), EffectiveLimit::None);
}

/// `limited` → `unlimited/controlled`, by a deactivation. [LPC-TS-026]
#[test]
fn atc_lpc_com_pt_cstransition6_002() {
    let (mut cs, mut now) = cf_cs_limited_wo_dur();
    now += secs(1);
    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
}

/// `limited` → `failsafe state`, after 120 seconds without a heartbeat. [LPC-TS-029]
#[test]
fn atc_lpc_com_pt_cstransition7_001() {
    let (mut cs, _) = cf_cs_limited_wo_dur();

    let due = cs
        .poll_timeout()
        .expect("a limited system is always waiting");
    cs.handle_timeout(due - secs(1));
    assert_eq!(cs.state(), LimitationState::Limited, "not yet");

    cs.handle_timeout(due);
    assert_eq!(cs.state(), LimitationState::FailsafeState);
}

/// `failsafe state` → `unlimited/controlled`, by a refused limit. [LPC-TS-031],
/// [LPC-TS-035/1]
#[test]
fn atc_lpc_com_pt_cstransition8_001() {
    let (mut cs, mut now) = cf_cs_fs();
    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);

    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_06), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NegativeValue)
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
}

/// `failsafe state` → `unlimited/controlled`, by a deactivation. [LPC-TS-033]
#[test]
fn atc_lpc_com_pt_cstransition8_002() {
    for value in [APCL_02, APCL_03, APCL_04] {
        let (mut cs, mut now) = cf_cs_fs();
        now += secs(1);
        cs.on_heartbeat(now);
        now += secs(1);
        let write = LimitWrite {
            is_active: false,
            watts: value,
            duration: None,
        };
        assert_eq!(
            cs.on_limit_write(&write, LocalDecision::Apply, now),
            WriteOutcome::Accepted,
            "{value} W, deactivated"
        );
        assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
    }
}

/// `failsafe state` → `limited`, by an activation. [LPC-TS-032]
#[test]
fn atc_lpc_com_pt_cstransition9_001() {
    let (mut cs, mut now) = cf_cs_fs();
    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);

    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_03), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::Limited);
}

/// `failsafe state` → `unlimited/autonomous`, on the Failsafe Duration Minimum expiring.
/// [LPC-TS-012], [LPC-TS-022], [LPC-TS-022/3]
#[test]
fn atc_lpc_com_pt_cstransition10_001() {
    let (mut cs, entered) = cf_cs_fs();

    cs.handle_timeout(entered + PFSDM - secs(1));
    assert_eq!(cs.state(), LimitationState::FailsafeState, "not yet");

    cs.handle_timeout(entered + PFSDM);
    assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
}

/// `failsafe state` → `unlimited/autonomous`, because a heartbeat arrived but no limit
/// followed within 120 seconds. [LPC-TS-022], [LPC-TS-022/2]
#[test]
fn atc_lpc_com_pt_cstransition10_002() {
    let (mut cs, mut now) = cf_cs_fs();
    now += secs(1);
    cs.on_heartbeat(now);

    cs.handle_timeout(now + secs(119));
    assert_eq!(cs.state(), LimitationState::FailsafeState, "not yet");

    cs.handle_timeout(now + secs(120));
    assert_eq!(
        cs.state(),
        LimitationState::UnlimitedAutonomous,
        "an Energy Guard that beats but does not write is not controlling anything"
    );
}

/// `unlimited/autonomous` → `unlimited/controlled`, by a refused limit. [LPC-TS-031],
/// [LPC-TS-035/1]
#[test]
fn atc_lpc_com_pt_cstransition11_001() {
    let (mut cs, mut now) = cf_cs_unl_auto();
    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);

    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_06), LocalDecision::Apply, now),
        WriteOutcome::Rejected(NackReason::NegativeValue)
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
}

/// `unlimited/autonomous` → `unlimited/controlled`, by a deactivation. [LPC-TS-033]
#[test]
fn atc_lpc_com_pt_cstransition11_002() {
    let (mut cs, mut now) = cf_cs_unl_auto();
    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);

    assert_eq!(
        cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
}

/// `unlimited/autonomous` → `limited`, by a heartbeat and an activation. [LPC-TS-032]
#[test]
fn atc_lpc_com_pt_cstransition12_001() {
    let (mut cs, mut now) = cf_cs_unl_auto();
    now += secs(1);
    cs.on_heartbeat(now);
    now += secs(1);

    assert_eq!(
        cs.on_limit_write(&LimitWrite::active(APCL_03), LocalDecision::Apply, now),
        WriteOutcome::Accepted
    );
    assert_eq!(cs.state(), LimitationState::Limited);
}

/// A Controllable System on a CEM may refuse a limit for the four permitted reasons — and
/// uncontrolled loads is one only an energy manager may give. [LPC-TS-035],
/// [LPC-TS-035/2]
#[test]
fn atc_lpc_ins1_pt_cstransition1_001() {
    let permitted = [
        RejectReason::SelfProtection,
        RejectReason::SafetyRelated,
        RejectReason::Regulatory,
        RejectReason::UncontrolledLoads,
    ];
    for reason in permitted {
        let mut cs = ControllableSystem::new(
            CsConfig::new(FCAPL_03, PFSDM)
                .with_contractual_max(30_000.0)
                .on_cem(),
            Duration::ZERO,
        );
        cs.on_heartbeat(secs(10));
        assert_eq!(
            cs.on_limit_write(&LimitWrite::deactivated(), LocalDecision::Apply, secs(11)),
            WriteOutcome::Accepted,
            "the opening deactivation is always accepted"
        );

        cs.on_heartbeat(secs(12));
        assert_eq!(
            cs.on_limit_write(
                &LimitWrite::active(APCL_03),
                LocalDecision::Reject(reason),
                secs(13)
            ),
            WriteOutcome::Rejected(NackReason::CannotApply(reason)),
            "{reason:?}"
        );
        assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
    }
}

/// The same, off a CEM — where uncontrolled loads is *not* a reason a device may give,
/// because a device has no loads but its own. [LPC-TS-035], [LPC-TS-035/3]
#[test]
fn atc_lpc_ins2_pt_cstransition1_001() {
    for reason in [
        RejectReason::SelfProtection,
        RejectReason::SafetyRelated,
        RejectReason::Regulatory,
    ] {
        let (mut cs, mut now) = cf_cs_unl_cntrl();
        now += secs(1);
        assert_eq!(
            cs.on_limit_write(
                &LimitWrite::active(APCL_03),
                LocalDecision::Reject(reason),
                now
            ),
            WriteOutcome::Rejected(NackReason::CannotApply(reason)),
            "{reason:?}"
        );
        assert_eq!(cs.state(), LimitationState::UnlimitedControlled);
    }

    // And the one it may not: `interrupt` refuses to act on it off a CEM.
    let (mut cs, mut now) = cf_cs_limited_wo_dur();
    now += secs(1);
    assert!(
        !cs.interrupt(RejectReason::UncontrolledLoads, now),
        "[LPC-TS-035/3] does not list uncontrolled loads for a device"
    );
    assert!(cs.interrupt(RejectReason::SelfProtection, now));
}

// ---- what makes LPC and LPP two use cases -------------------------------------

/// The claim that one state machine answers both specifications rests on the two
/// differing only in what they publish. This checks the difference, so that the claim
/// above it is not taken on trust.
#[test]
fn lpc_and_lpp_differ_only_in_what_they_publish() {
    assert_eq!(
        lpc::DIRECTION.failsafe_limit_key().as_str(),
        "failsafeConsumptionActivePowerLimit"
    );
    assert_eq!(
        lpp::DIRECTION.failsafe_limit_key().as_str(),
        "failsafeProductionActivePowerLimit"
    );
    assert_eq!(
        lpc::DIRECTION.nominal_max_characteristic().as_str(),
        "powerConsumptionNominalMax"
    );
    assert_eq!(
        lpp::DIRECTION.nominal_max_characteristic().as_str(),
        "powerProductionNominalMax"
    );
    assert_eq!(lpc::DIRECTION.signal_prefix(), "lpc");
    assert_eq!(lpp::DIRECTION.signal_prefix(), "lpp");

    // The timers, the states and the transitions are literally the same code, so the
    // requirement identifiers line up one for one.
    assert_eq!(
        lpc::CONTROLLABLE_SYSTEM.scenarios.len(),
        lpp::CONTROLLABLE_SYSTEM.scenarios.len()
    );
}

// ---- the claims, and the coverage they add up to ------------------------------

/// How this crate answers for one abstract test case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Claim {
    /// The test of the same name in this file drives it.
    Tested,
    /// The library cannot answer for it; the device it is linked into must. The reason is
    /// the catalogue's — `conformance::DEVICE_LEVEL` — so that a consumer's harness and
    /// this suite say the same thing about the same case.
    Device,
}

/// Every abstract test case of LPC's Controllable System, and how this crate answers it.
///
/// LPP's twin of each is claimed with it — see the module documentation.
const CS_CLAIMS: &[(&str, Claim)] = &[
    ("CSHeartbeat_001", Claim::Tested),
    ("CSConnection_001", Claim::Tested),
    ("CSConnection_002", Claim::Tested),
    ("CSConnection_003", Claim::Tested),
    ("CSConnection_004", Claim::Tested),
    ("CSConnection_005", Claim::Tested),
    ("CSConnection_006", Claim::Device),
    ("CSConnection_007", Claim::Tested),
    ("CSConnection_008", Claim::Tested),
    ("CSConnection_009", Claim::Device),
    ("CSInit_001", Claim::Tested),
    ("CSInit_002", Claim::Device),
    ("CSInit_003", Claim::Device),
    ("CSLimited_001", Claim::Tested),
    ("CSLimited_002", Claim::Tested),
    ("CSUnlCntrl_001", Claim::Tested),
    ("CSUnlCntrl_002", Claim::Tested),
    ("CSUnlCntrl_003", Claim::Tested),
    ("CSFS_001", Claim::Tested),
    ("CSFS_002", Claim::Tested),
    ("CSFS_003", Claim::Tested),
    ("CSUnlAuto_001", Claim::Tested),
    ("CSUnlAuto_002", Claim::Tested),
    ("CSTransition1_001", Claim::Tested),
    ("CSTransition1_002", Claim::Tested),
    ("CSTransition2_001", Claim::Tested),
    ("CSTransition3_001", Claim::Tested),
    ("CSTransition3_002", Claim::Tested),
    ("CSTransition4_001", Claim::Tested),
    ("CSTransition5_001", Claim::Tested),
    ("CSTransition6_001", Claim::Tested),
    ("CSTransition6_002", Claim::Tested),
    ("CSTransition7_001", Claim::Tested),
    ("CSTransition8_001", Claim::Tested),
    ("CSTransition8_002", Claim::Tested),
    ("CSTransition9_001", Claim::Tested),
    ("CSTransition10_001", Claim::Tested),
    ("CSTransition10_002", Claim::Tested),
    ("CSTransition11_001", Claim::Tested),
    ("CSTransition11_002", Claim::Tested),
    ("CSTransition12_001", Claim::Tested),
];

/// LPC's Energy Guard.
const EG_CLAIMS: &[(&str, Claim)] = &[
    ("EGHeartbeat_001", Claim::Tested),
    ("EGConnection_001", Claim::Device),
    ("EGConnection_002", Claim::Tested),
    ("EGConnection_003", Claim::Device),
    ("EGMessages_001", Claim::Tested),
    ("EGMessages_002", Claim::Device),
    ("EGMessages_003", Claim::Tested),
    ("EGMessages_004", Claim::Tested),
];

/// The two installation-specific test cases. They carry the same name and differ only in
/// the group — `INS1` is a Controllable System on a CEM, `INS2` one that is not.
const INS_CLAIMS: &[(&str, &str, Claim)] = &[
    ("INS1", "CSTransition1_001", Claim::Tested),
    ("INS2", "CSTransition1_001", Claim::Tested),
];

// ---- MPC and MGCP: the measurement catalogue ----------------------------------
//
// The 101 abstract test cases of these two specifications are far more regular than
// LPC's: every one of them is "this measurand, published or resolved, normal or not".
// Written out as 101 functions they would be 101 copies of four shapes, and the copy
// that drifted would be the one nobody read. So they are a table — one row per `ATC_…`
// identifier, which is the same granularity a laboratory reports at — driven through the
// same in-memory pair the ordinary monitoring tests use.
//
// The `NT_` rows are the interesting half. [MPC-003] says a value whose `valueState` is
// `error` is to be ignored *whatever number came with it*, so what the row asserts is not
// that a number is wrong but that no number is offered at all.

/// What one measurement abstract test case asks of the implementation.
#[derive(Clone, Debug)]
enum Check {
    /// The unit publishes this measurand, and it reaches the peer.
    Publishes(Measurand, f64),
    /// The appliance resolves the measurand to this value, with state `normal`.
    Reads(Measurand, f64),
    /// The value arrives flagged `error`, and the appliance offers no number for it.
    Refuses(Measurand),
    /// A poll — a read of the measurement list — is answered.
    Polling,
    /// A change reaches a subscriber as a notification, without being asked for.
    Notification,
    /// MGCP scenario 1's PV curtailment factor, which is a configuration key rather than
    /// a measurement and so is the one row that does not go through the measurand layer.
    Curtailment(f64),
}

const V_PN: f64 = 230.0;
const V_PP: f64 = 400.0;

/// Every measurand the two specifications name, so one unit can serve every row.
fn all_measurands() -> Vec<Measurand> {
    let mut all = vec![
        Measurand::total_power(),
        Measurand::total(Quantity::EnergyConsumed),
        Measurand::total(Quantity::EnergyProduced),
        Measurand::total(Quantity::Frequency),
    ];
    for phase in [Phase::A, Phase::B, Phase::C] {
        all.push(Measurand::on(Quantity::Power, phase.clone()));
        all.push(Measurand::on(Quantity::Current, phase.clone()));
        all.push(Measurand::on(Quantity::Voltage, phase));
    }
    for phase in [Phase::Ab, Phase::Bc, Phase::Ac] {
        all.push(Measurand::on(Quantity::Voltage, phase));
    }
    all
}

/// A monitored unit measuring everything, and the appliance reading it.
fn measurement_pair(
    entity: EntityType,
    server: &'static UseCaseDescriptor,
    client: &'static UseCaseDescriptor,
    scenarios: &[u32],
    naming: Option<Naming>,
    curtailment: bool,
) -> common::MonitoringPair {
    let mut unit = MonitoredUnit::new(1);
    if let Some(naming) = naming {
        unit = unit.naming(naming);
    }
    for measurand in all_measurands() {
        unit = unit.with(measurand);
    }
    let mut pair =
        common::MonitoringPair::build_with(unit, entity, server, client, scenarios, curtailment);
    pair.commission();
    pair
}

fn mpc_pair() -> common::MonitoringPair {
    measurement_pair(
        EntityType::HeatPumpAppliance,
        &mpc::MONITORED_UNIT,
        &mpc::MONITORING_APPLIANCE,
        &[1, 2, 3, 4, 5],
        None,
        false,
    )
}

fn mgcp_pair() -> common::MonitoringPair {
    measurement_pair(
        EntityType::GridConnectionPointOfPremises,
        &mgcp::GRID_CONNECTION_POINT,
        &mgcp::MONITORING_APPLIANCE,
        &[1, 2, 3, 4, 5, 6, 7],
        Some(mgcp::NAMING),
        true,
    )
}

/// Runs one row and returns the reason it failed, if it did.
fn run_check(pair: &mut common::MonitoringPair, check: Check) -> Result<(), String> {
    match check {
        Check::Publishes(measurand, value) => {
            pair.report(&measurand, value);
            if pair.unit.value(&measurand) != Some(value) {
                return Err(format!("the unit did not hold {measurand:?}"));
            }
            // It reaching the peer is what "sends" means; the unit holding it is not.
            match pair.readings.get(&measurand).and_then(|r| r.usable()) {
                Some(seen) if seen == value => Ok(()),
                other => Err(format!("the peer saw {other:?}, not {value}")),
            }
        }
        Check::Reads(measurand, value) => {
            pair.report(&measurand, value);
            let reading = pair
                .readings
                .get(&measurand)
                .ok_or_else(|| format!("no reading for {measurand:?}"))?;
            if reading.state != ReadingState::Normal {
                return Err(format!("state was {:?}, not normal", reading.state));
            }
            match reading.usable() {
                Some(seen) if seen == value => Ok(()),
                other => Err(format!("resolved to {other:?}, not {value}")),
            }
        }
        Check::Refuses(measurand) => {
            pair.report(&measurand, 1_234.0);
            pair.unit
                .set_state(&measurand, MeasurementValueState::Error);
            let feature = pair.unit_feature(2);
            pair.unit.notify(&mut pair.unit_engine, &feature, pair.now);
            pair.exchange();
            let reading = pair
                .readings
                .get(&measurand)
                .ok_or_else(|| format!("no reading for {measurand:?}"))?;
            if reading.state != ReadingState::Error {
                return Err(format!("state was {:?}, not error", reading.state));
            }
            match reading.usable() {
                None => Ok(()),
                Some(v) => Err(format!("offered {v} for a failed measurement — [MPC-003]")),
            }
        }
        Check::Polling => {
            // A read of the measurement list is answered with the values, without a
            // subscription having been granted.
            let client = pair.manager_client();
            let measurement = pair.unit_feature(2);
            pair.report(&Measurand::total_power(), 2_300.0);
            pair.manager.read(
                &measurement,
                &client,
                Function::MeasurementListData,
                pair.now,
            );
            pair.exchange();
            match pair.readings.total_power() {
                Some(2_300.0) => Ok(()),
                other => Err(format!("a poll answered {other:?}")),
            }
        }
        Check::Notification => {
            // Nothing is read. The value changes and the change arrives on its own,
            // which is what the subscription granted at commissioning is for.
            let before = pair.readings.total_power();
            pair.report(&Measurand::total_power(), 4_242.0);
            if before == Some(4_242.0) {
                return Err("the value did not change, so nothing was proved".into());
            }
            match pair.readings.total_power() {
                Some(4_242.0) => Ok(()),
                other => Err(format!("the notification carried {other:?}")),
            }
        }
        Check::Curtailment(percent) => {
            pair.report_curtailment(percent);
            match pair.curtailment() {
                Some(seen) if (seen - percent).abs() < f64::EPSILON => Ok(()),
                other => Err(format!("the factor read back as {other:?}, not {percent}")),
            }
        }
    }
}

/// The 54 abstract test cases of MPC 1.0.2, one row each.
fn mpc_catalogue() -> Vec<(&'static str, Check)> {
    use Check::*;
    let p = |q, ph| Measurand::on(q, ph);
    let mut rows: Vec<(&'static str, Check)> = vec![
        // --- Monitored Unit: it publishes ---
        ("ATC_MPC_COM_PT_MUPolling_001", Polling),
        ("ATC_MPC_COM_PT_MUNotification_001", Notification),
        (
            "ATC_MPC_SCE1_PT_MUTotalActivePower_001",
            Publishes(Measurand::total_power(), 2_300.0),
        ),
        (
            "ATC_MPC_SCE1_PT_MUPhaseActivePower_001",
            Publishes(p(Quantity::Power, Phase::A), 800.0),
        ),
        (
            "ATC_MPC_SCE1_PT_MUPhaseActivePower_002",
            Publishes(p(Quantity::Power, Phase::B), 760.0),
        ),
        (
            "ATC_MPC_SCE1_PT_MUPhaseActivePower_003",
            Publishes(p(Quantity::Power, Phase::C), 740.0),
        ),
        // Two rows each: the specification drives the value up and then down, so the
        // second is the same measurand at a different value rather than a second point.
        (
            "ATC_MPC_SCE2_PT_MUTotalConsumedEnergy_001",
            Publishes(Measurand::total(Quantity::EnergyConsumed), 12_500.0),
        ),
        (
            "ATC_MPC_SCE2_PT_MUTotalConsumedEnergy_002",
            Publishes(Measurand::total(Quantity::EnergyConsumed), 13_000.0),
        ),
        (
            "ATC_MPC_SCE2_PT_MUTotalProducedEnergy_001",
            Publishes(Measurand::total(Quantity::EnergyProduced), 4_100.0),
        ),
        (
            "ATC_MPC_SCE2_PT_MUTotalProducedEnergy_002",
            Publishes(Measurand::total(Quantity::EnergyProduced), 4_600.0),
        ),
        (
            "ATC_MPC_SCE3_PT_MUActiveACCurrent_001",
            Publishes(p(Quantity::Current, Phase::A), 3.5),
        ),
        (
            "ATC_MPC_SCE3_PT_MUActiveACCurrent_002",
            Publishes(p(Quantity::Current, Phase::B), 3.3),
        ),
        (
            "ATC_MPC_SCE3_PT_MUActiveACCurrent_003",
            Publishes(p(Quantity::Current, Phase::C), 3.2),
        ),
        (
            "ATC_MPC_SCE4_PT_MUACVoltage_001",
            Publishes(p(Quantity::Voltage, Phase::A), V_PN),
        ),
        (
            "ATC_MPC_SCE4_PT_MUACVoltage_002",
            Publishes(p(Quantity::Voltage, Phase::B), V_PN),
        ),
        (
            "ATC_MPC_SCE4_PT_MUACVoltage_003",
            Publishes(p(Quantity::Voltage, Phase::C), V_PN),
        ),
        (
            "ATC_MPC_SCE4_PT_MUACVoltage_004",
            Publishes(p(Quantity::Voltage, Phase::Ab), V_PP),
        ),
        (
            "ATC_MPC_SCE4_PT_MUACVoltage_005",
            Publishes(p(Quantity::Voltage, Phase::Bc), V_PP),
        ),
        (
            "ATC_MPC_SCE4_PT_MUACVoltage_006",
            Publishes(p(Quantity::Voltage, Phase::Ac), V_PP),
        ),
        (
            "ATC_MPC_SCE5_PT_MUFrequency_001",
            Publishes(Measurand::total(Quantity::Frequency), 50.0),
        ),
        // --- Monitoring Appliance: it resolves, and refuses what it must ---
        ("ATC_MPC_COM_PT_MAPolling_001", Polling),
        ("ATC_MPC_COM_PT_MANotification_001", Notification),
        (
            "ATC_MPC_SCE1_PT_MATotalActivePower_001",
            Reads(Measurand::total_power(), 2_300.0),
        ),
        (
            "ATC_MPC_SCE1_NT_MATotalActivePower_002",
            Refuses(Measurand::total_power()),
        ),
        (
            "ATC_MPC_SCE2_PT_MATotalConsumedEnergy_001",
            Reads(Measurand::total(Quantity::EnergyConsumed), 12_500.0),
        ),
        (
            "ATC_MPC_SCE2_NT_MATotalConsumedEnergy_002",
            Refuses(Measurand::total(Quantity::EnergyConsumed)),
        ),
        (
            "ATC_MPC_SCE2_PT_MATotalProducedEnergy_001",
            Reads(Measurand::total(Quantity::EnergyProduced), 4_100.0),
        ),
        (
            "ATC_MPC_SCE2_NT_MATotalProducedEnergy_002",
            Refuses(Measurand::total(Quantity::EnergyProduced)),
        ),
        (
            "ATC_MPC_SCE5_PT_MAFrequency_001",
            Reads(Measurand::total(Quantity::Frequency), 50.0),
        ),
        (
            "ATC_MPC_SCE5_NT_MAFrequency_002",
            Refuses(Measurand::total(Quantity::Frequency)),
        ),
    ];
    // The phase-indexed families: odd numbers are the positive test, even the negative,
    // one pair per phase, which is exactly how the specification numbers them.
    let phases = [Phase::A, Phase::B, Phase::C];
    for (i, phase) in phases.into_iter().enumerate() {
        let pt = 2 * i + 1;
        let nt = 2 * i + 2;
        rows.push((
            leak(format!("ATC_MPC_SCE1_PT_MAPhaseActivePower_{pt:03}")),
            Reads(p(Quantity::Power, phase.clone()), 800.0),
        ));
        rows.push((
            leak(format!("ATC_MPC_SCE1_NT_MAPhaseActivePower_{nt:03}")),
            Refuses(p(Quantity::Power, phase.clone())),
        ));
        rows.push((
            leak(format!("ATC_MPC_SCE3_PT_MAActiveACCurrent_{pt:03}")),
            Reads(p(Quantity::Current, phase.clone()), 3.5),
        ));
        rows.push((
            leak(format!("ATC_MPC_SCE3_NT_MAActiveACCurrent_{nt:03}")),
            Refuses(p(Quantity::Current, phase.clone())),
        ));
    }
    let voltages = [
        Phase::A,
        Phase::B,
        Phase::C,
        Phase::Ab,
        Phase::Bc,
        Phase::Ac,
    ];
    for (i, phase) in voltages.into_iter().enumerate() {
        let pt = 2 * i + 1;
        let nt = 2 * i + 2;
        let value = if i < 3 { V_PN } else { V_PP };
        rows.push((
            leak(format!("ATC_MPC_SCE4_PT_MAACVoltage_{pt:03}")),
            Reads(p(Quantity::Voltage, phase.clone()), value),
        ));
        rows.push((
            leak(format!("ATC_MPC_SCE4_NT_MAACVoltage_{nt:03}")),
            Refuses(p(Quantity::Voltage, phase.clone())),
        ));
    }
    rows
}

/// The 47 abstract test cases of MGCP 1.0.2, one row each.
fn mgcp_catalogue() -> Vec<(&'static str, Check)> {
    use Check::*;
    let p = |q, ph| Measurand::on(q, ph);
    let mut rows: Vec<(&'static str, Check)> = vec![
        // --- the Grid Connection Point publishes ---
        ("ATC_MGCP_COM_PT_GCPPolling_001", Polling),
        ("ATC_MGCP_COM_PT_GCPNotification_001", Notification),
        (
            "ATC_MGCP_SCE1_PT_GCPPowerLimitFactor_001",
            Curtailment(70.0),
        ),
        (
            "ATC_MGCP_SCE2_PT_GCPTotalActivePower_001",
            Publishes(Measurand::total_power(), 2_300.0),
        ),
        // "a negative total feed-in energy while consuming" and its positive twin: the
        // grid's own sign convention, which is why MGCP names these differently from MPC.
        (
            "ATC_MGCP_SCE3_PT_GCPTotalFeedInEnergy_001",
            Publishes(Measurand::total(Quantity::EnergyProduced), 0.0),
        ),
        (
            "ATC_MGCP_SCE3_PT_GCPTotalFeedInEnergy_002",
            Publishes(Measurand::total(Quantity::EnergyProduced), 8_400.0),
        ),
        (
            "ATC_MGCP_SCE4_PT_GCPTotalConsumedEnergy_001",
            Publishes(Measurand::total(Quantity::EnergyConsumed), 0.0),
        ),
        (
            "ATC_MGCP_SCE4_PT_GCPTotalConsumedEnergy_002",
            Publishes(Measurand::total(Quantity::EnergyConsumed), 21_000.0),
        ),
        (
            "ATC_MGCP_SCE5_PT_GCPActiveACCurrent_001",
            Publishes(p(Quantity::Current, Phase::A), 3.5),
        ),
        (
            "ATC_MGCP_SCE5_PT_GCPActiveACCurrent_002",
            Publishes(p(Quantity::Current, Phase::B), 3.3),
        ),
        (
            "ATC_MGCP_SCE5_PT_GCPActiveACCurrent_003",
            Publishes(p(Quantity::Current, Phase::C), 3.2),
        ),
        (
            "ATC_MGCP_SCE6_PT_GCPACVoltage_001",
            Publishes(p(Quantity::Voltage, Phase::A), V_PN),
        ),
        (
            "ATC_MGCP_SCE6_PT_GCPACVoltage_002",
            Publishes(p(Quantity::Voltage, Phase::B), V_PN),
        ),
        (
            "ATC_MGCP_SCE6_PT_GCPACVoltage_003",
            Publishes(p(Quantity::Voltage, Phase::C), V_PN),
        ),
        (
            "ATC_MGCP_SCE6_PT_GCPACVoltage_004",
            Publishes(p(Quantity::Voltage, Phase::Ab), V_PP),
        ),
        (
            "ATC_MGCP_SCE6_PT_GCPACVoltage_005",
            Publishes(p(Quantity::Voltage, Phase::Bc), V_PP),
        ),
        (
            "ATC_MGCP_SCE6_PT_GCPACVoltage_006",
            Publishes(p(Quantity::Voltage, Phase::Ac), V_PP),
        ),
        (
            "ATC_MGCP_SCE7_PT_GCPFrequency_001",
            Publishes(Measurand::total(Quantity::Frequency), 50.0),
        ),
        // --- the Monitoring Appliance resolves ---
        ("ATC_MGCP_COM_PT_MAPolling_001", Polling),
        ("ATC_MGCP_COM_PT_MANotification_001", Notification),
        ("ATC_MGCP_SCE1_PT_MAPowerLimitFactor_001", Curtailment(60.0)),
        (
            "ATC_MGCP_SCE2_PT_MATotalActivePower_001",
            Reads(Measurand::total_power(), 2_300.0),
        ),
        (
            "ATC_MGCP_SCE2_NT_MATotalActivePower_002",
            Refuses(Measurand::total_power()),
        ),
        (
            "ATC_MGCP_SCE3_PT_MATotalFeedInEnergy_001",
            Reads(Measurand::total(Quantity::EnergyProduced), 8_400.0),
        ),
        (
            "ATC_MGCP_SCE3_NT_MATotalFeedInEnergy_002",
            Refuses(Measurand::total(Quantity::EnergyProduced)),
        ),
        (
            "ATC_MGCP_SCE4_PT_MATotalConsumedEnergy_001",
            Reads(Measurand::total(Quantity::EnergyConsumed), 21_000.0),
        ),
        (
            "ATC_MGCP_SCE4_NT_MATotalConsumedEnergy_002",
            Refuses(Measurand::total(Quantity::EnergyConsumed)),
        ),
        (
            "ATC_MGCP_SCE7_PT_MAFrequency_001",
            Reads(Measurand::total(Quantity::Frequency), 50.0),
        ),
        (
            "ATC_MGCP_SCE7_NT_MAFrequency_002",
            Refuses(Measurand::total(Quantity::Frequency)),
        ),
    ];
    let phases = [Phase::A, Phase::B, Phase::C];
    for (i, phase) in phases.into_iter().enumerate() {
        let pt = 2 * i + 1;
        let nt = 2 * i + 2;
        rows.push((
            leak(format!("ATC_MGCP_SCE5_PT_MAActiveACCurrent_{pt:03}")),
            Reads(p(Quantity::Current, phase.clone()), 3.5),
        ));
        rows.push((
            leak(format!("ATC_MGCP_SCE5_NT_MAActiveACCurrent_{nt:03}")),
            Refuses(p(Quantity::Current, phase.clone())),
        ));
    }
    let voltages = [
        Phase::A,
        Phase::B,
        Phase::C,
        Phase::Ab,
        Phase::Bc,
        Phase::Ac,
    ];
    for (i, phase) in voltages.into_iter().enumerate() {
        let pt = 2 * i + 1;
        let nt = 2 * i + 2;
        let value = if i < 3 { V_PN } else { V_PP };
        rows.push((
            leak(format!("ATC_MGCP_SCE6_PT_MAACVoltage_{pt:03}")),
            Reads(p(Quantity::Voltage, phase.clone()), value),
        ));
        rows.push((
            leak(format!("ATC_MGCP_SCE6_NT_MAACVoltage_{nt:03}")),
            Refuses(p(Quantity::Voltage, phase.clone())),
        ));
    }
    rows
}

/// The generated identifiers outlive the table, and a test binary's lifetime is the run.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Every row of the MPC catalogue, run against a real exchange.
///
/// One test rather than fifty-four, because every row is the same four shapes and the
/// identifier is in the failure message — which is the granularity a laboratory reports
/// at. A row that fails names itself.
#[test]
fn every_mpc_abstract_test_case() {
    let failures = run_catalogue(&mpc_catalogue(), mpc_pair);
    assert!(failures.is_empty(), "MPC:\n{}", failures.join("\n"));
}

/// The same for MGCP's forty-seven.
#[test]
fn every_mgcp_abstract_test_case() {
    let failures = run_catalogue(&mgcp_catalogue(), mgcp_pair);
    assert!(failures.is_empty(), "MGCP:\n{}", failures.join("\n"));
}

/// Runs each row on a pair of its own, so one row cannot leave state behind for the next.
fn run_catalogue(
    rows: &[(&'static str, Check)],
    build: fn() -> common::MonitoringPair,
) -> Vec<String> {
    let mut failures = Vec::new();
    for (id, check) in rows {
        let mut pair = build();
        if let Err(why) = run_check(&mut pair, check.clone()) {
            failures.push(format!("  {id}: {why}"));
        }
    }
    failures
}

/// Every row names a test case the catalogue carries, and every test case has a row.
///
/// The two halves matter equally: a typo inflates the coverage number it was written to
/// justify, and a missing row lets a test case nobody has looked at hide in the gap.
#[test]
fn the_measurement_catalogues_and_the_specification_agree() {
    for (label, use_case, rows) in [
        ("MPC", names::MPC, mpc_catalogue()),
        ("MGCP", names::MGCP, mgcp_catalogue()),
    ] {
        for (id, _) in &rows {
            assert!(
                conformance::find(id).is_some(),
                "{label}: {id} is not in the catalogue"
            );
        }
        for case in conformance::for_use_case(use_case) {
            assert!(
                rows.iter().any(|(id, _)| *id == case.id),
                "{label}: {} has no row in this file",
                case.id
            );
        }
        assert_eq!(
            rows.len(),
            conformance::for_use_case(use_case).count(),
            "{label}: the table and the specification are different sizes"
        );
    }
}

/// Rebuilds a full `ATC_…` identifier from the short name a claim carries.
fn identifier(use_case: &str, group: &str, name: &str) -> String {
    let kind = if name.starts_with("CSConnection_001")
        || name.starts_with("CSLimited_001")
        || name.starts_with("CSUnlCntrl_001")
        || name.starts_with("CSUnlAuto_001")
    {
        "NT"
    } else {
        "PT"
    };
    format!("ATC_{use_case}_{group}_{kind}_{name}")
}

/// Every identifier of one limitation use case, paired with how this crate answers it.
fn claims_for(use_case: &str) -> Vec<(String, Claim)> {
    let mut ids: Vec<(String, Claim)> = CS_CLAIMS
        .iter()
        .chain(EG_CLAIMS)
        .map(|(name, claim)| (identifier(use_case, "COM", name), *claim))
        .collect();
    ids.extend(
        INS_CLAIMS
            .iter()
            .map(|(group, name, claim)| (identifier(use_case, group, name), *claim)),
    );
    ids
}

/// What this suite marks as the device's and what the catalogue publishes as the device's
/// are one list, in both directions.
///
/// `conformance::device_level` is the checklist a consumer's harness works from, and this
/// suite is what keeps it honest: a case marked `Device` here and not there would be a
/// case nobody answers for, and one there and not here would be one this suite drives
/// while telling the consumer it cannot be driven.
#[test]
fn the_device_level_cases_are_the_catalogues_and_the_suites_alike() {
    for use_case in ["LPC", "LPP"] {
        let mut claimed: Vec<String> = claims_for(use_case)
            .into_iter()
            .filter(|(_, claim)| *claim == Claim::Device)
            .map(|(id, _)| id)
            .collect();
        claimed.sort();
        let mut published: Vec<String> = conformance::device_level()
            .map(|(case, _)| case.id.to_string())
            .filter(|id| id.starts_with(&format!("ATC_{use_case}_")))
            .collect();
        published.sort();
        assert_eq!(claimed, published, "{use_case}");
        for id in &claimed {
            let case = conformance::find(id).expect("a real case");
            assert!(case.owed_by_device().is_some(), "{id} carries its reason");
        }
    }
}

/// Every claim names a test case that exists — a typo would otherwise inflate the number
/// it was made to justify.
#[test]
fn every_claim_names_a_real_test_case() {
    for use_case in ["LPC", "LPP"] {
        for (id, _) in claims_for(use_case) {
            assert!(
                conformance::find(&id).is_some(),
                "{id} is not in the catalogue"
            );
        }
    }
}

/// And every test case of the two limitation use cases has a claim: a test case nobody
/// has looked at must not be able to hide in the gap between the two lists.
#[test]
fn every_test_case_of_lpc_and_lpp_has_a_claim() {
    for (use_case, label) in [(names::LPC, "LPC"), (names::LPP, "LPP")] {
        let claimed: Vec<String> = claims_for(label).into_iter().map(|(id, _)| id).collect();
        for case in conformance::for_use_case(use_case) {
            assert!(
                claimed.iter().any(|id| id == case.id),
                "{} has no claim in this file",
                case.id
            );
        }
    }
}

/// The coverage number, printed and asserted, over all four certifiable use cases.
///
/// `cargo test --test conformance -- --nocapture coverage` prints the table.
#[test]
fn coverage_of_the_certifiable_use_cases() {
    let mut report = String::new();
    report.push_str("\nEEBUS High-Level Test Specification coverage\n");
    report.push_str("=============================================\n\n");

    let mut totals = (0usize, 0usize);
    fn row(
        report: &mut String,
        totals: &mut (usize, usize),
        label: &str,
        use_case: &'static str,
        actor: &'static str,
        actor_label: &str,
        claims: &[(String, Claim)],
    ) {
        let scope: Vec<_> = conformance::for_actor(use_case, actor).collect();
        if scope.is_empty() {
            return;
        }
        // Only the claims that fall inside this scope: a Monitored Unit's coverage is not
        // improved by a Monitoring Appliance test.
        let tested: Vec<&str> = claims
            .iter()
            .filter(|(id, claim)| {
                matches!(claim, Claim::Tested) && scope.iter().any(|case| case.id == id)
            })
            .map(|(id, _)| id.as_str())
            .collect();
        let coverage = Coverage::of(scope.iter().copied(), &tested);
        totals.0 += coverage.covered();
        totals.1 += scope.len();
        report.push_str(&format!(
            "{label:<5} {actor_label:<22} {:>3}/{:<3} {:>3}%\n",
            coverage.covered(),
            scope.len(),
            coverage.percent()
        ));
        assert!(
            coverage.unknown().is_empty(),
            "claims naming nothing: {:?}",
            coverage.unknown()
        );
    }

    for (use_case, label) in [(names::LPC, "LPC"), (names::LPP, "LPP")] {
        let claims = claims_for(label);
        for (actor, actor_label) in [
            (actors::CONTROLLABLE_SYSTEM, "ControllableSystem"),
            (actors::ENERGY_GUARD, "EnergyGuard"),
        ] {
            row(
                &mut report,
                &mut totals,
                label,
                use_case,
                actor,
                actor_label,
                &claims,
            );
        }
    }

    // Every row of the two measurement catalogues is driven, so every one is a claim.
    for (label, use_case, rows, dut, dut_label) in [
        (
            "MPC",
            names::MPC,
            mpc_catalogue(),
            actors::MONITORED_UNIT,
            "MonitoredUnit",
        ),
        (
            "MGCP",
            names::MGCP,
            mgcp_catalogue(),
            actors::GRID_CONNECTION_POINT,
            "GridConnectionPoint",
        ),
    ] {
        let claims: Vec<(String, Claim)> = rows
            .iter()
            .map(|(id, _)| ((*id).to_string(), Claim::Tested))
            .collect();
        for (actor, actor_label) in [
            (dut, dut_label),
            (actors::MONITORING_APPLIANCE, "MonitoringAppliance"),
        ] {
            row(
                &mut report,
                &mut totals,
                label,
                use_case,
                actor,
                actor_label,
                &claims,
            );
        }
    }

    report.push_str(&format!(
        "\n      {:<22} {:>3}/{:<3} {:>3}%\n",
        "total",
        totals.0,
        totals.1,
        totals.0 * 100 / totals.1
    ));

    report.push_str("\nNot covered here, and why (the same list for LPP):\n");
    for (case, reason) in conformance::device_level() {
        if case.use_case == names::LPC {
            let name = case.id.trim_start_matches("ATC_LPC_COM_PT_");
            report.push_str(&format!("  {name:<20} {reason}\n"));
        }
    }
    report.push_str(
        "\nCovered here, and still owed by the device:\n\
         \x20 MPC/MGCP notification   the library notifies a change at once, as SPINE IG\n\
         \x20                         \u{a7}2.4 asks. Whether the *measured* value reaches it\n\
         \x20                         inside the 120 s the test allows is the application's\n\
         \x20                         own publish cadence, not the library's.\n",
    );
    println!("{report}");

    // The four specifications have 203 abstract test cases between them, and every one is
    // in scope here — the number this crate quotes has to be the number it measures.
    assert_eq!(totals.1, 203, "the catalogue is not fully in scope");

    // And the gap is exactly the cases no library can answer: seven device-level test
    // cases, counted once for LPC and once for LPP. This is the assertion that keeps the
    // claim honest in both directions — it fails if a test is quietly dropped, and it
    // fails if something is marked `Device` to make the number go up.
    let device_cases = CS_CLAIMS
        .iter()
        .chain(EG_CLAIMS)
        .filter(|(_, claim)| matches!(claim, Claim::Device))
        .count();
    assert_eq!(
        totals.1 - totals.0,
        device_cases * 2,
        "the uncovered set is no longer exactly the device-level cases:\n{report}"
    );
}
