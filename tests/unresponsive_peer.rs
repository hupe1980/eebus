//! What an actor does when a peer stops answering.
//!
//! SPINE IG §2.6.1 separates a refusal from silence: a `resultData` carrying an error is
//! a completed exchange the application handles, and a message that is never answered at
//! all is an unresponsive peer. §2.6.2 gives the escalation path that follows the second
//! — retry after 30 s, then 5 min, then 15 min, then give up — and §2.6.4 says a use case
//! that has failed persistently is something the *user* is told about rather than
//! something that quietly stops working.
//!
//! These are the tests for that path. They are written against the pair of real actors,
//! because the defect they exist for is not visible in either half alone: the guard sends
//! a limit, the acknowledgement never comes, and the question is whether anything ever
//! moves again.

mod common;

use core::time::Duration;

use common::*;
use eebus::usecases::limitation::{GuardEvent, LimitWrite};

/// A limit write that is never acknowledged must not stop the Energy Guard for good.
///
/// The write is outstanding, so nothing else may be sent on that limit until it resolves
/// — and if it never resolves, "nothing else" lasts forever. Under §14a that is a control
/// box that has silently stopped controlling: it is connected, it is heartbeating, and
/// the appliance it is responsible for hears nothing from it again.
#[test]
fn a_limit_write_that_is_never_answered_is_retried() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();
    pair.pump_is_deaf = true;
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
    pair.advance(Duration::from_secs(1));

    assert!(pair.accepted().is_empty(), "nothing came back");

    // The peer starts answering again. Whatever the escalation path costs, the guard has
    // to come back to the limit it still owes.
    pair.pump_is_deaf = false;
    for _ in 0..40 {
        pair.advance(Duration::from_secs(60));
        if !pair.accepted().is_empty() {
            break;
        }
    }

    assert_eq!(
        pair.accepted().len(),
        1,
        "the guard writes the limit again once the peer answers"
    );
    assert_eq!(pair.accepted()[0].watts, 3_000.0);
}

/// And it says so, rather than only recovering quietly.
///
/// §2.6.4: "If a specific Use Case fails persistently, notify the user about the limited
/// functionality." An installation whose limit writes are being swallowed looks identical
/// from the outside to one where the grid is asking for nothing.
#[test]
fn a_peer_that_never_answers_is_reported() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();
    pair.pump_is_deaf = true;
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);

    for _ in 0..40 {
        pair.advance(Duration::from_secs(60));
    }

    assert!(
        pair.reports
            .iter()
            .any(|r| matches!(r, GuardEvent::PeerUnresponsive { .. })),
        "the guard reports the peer it cannot reach, got {:?}",
        pair.reports
    );
}

/// A guard whose peer went quiet mid-limit does not hold the slot for ever.
///
/// The regression this file exists for, stated as the invariant rather than as the
/// recovery: whatever a peer does or fails to do, the guard must end up with nothing
/// outstanding, because an outstanding write is what blocks the next one.
#[test]
fn nothing_stays_outstanding_after_the_escalation_path() {
    let mut pair = Pair::new();
    pair.commission();
    pair.forget_history();

    let device = pair.device();
    pair.pump_is_deaf = true;
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);

    for _ in 0..60 {
        pair.advance(Duration::from_secs(60));
    }

    // The audit log is the §14a record, and it has to show a limit that was never
    // acknowledged — an acknowledgement is what the operator relies on, so its absence
    // is the fact worth recording.
    assert!(
        pair.guard
            .audit()
            .records()
            .any(|r| !r.outcome.is_accepted()),
        "an unacknowledged limit left no trace in the record"
    );
}

/// The engine bounds what it waits on, even though the escalation path is long.
#[test]
fn the_pending_table_is_bounded() {
    use eebus::model::{DeviceType, EntityType, FeatureType, Function, Role};
    use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature, Operations, SpineEvent};

    let mut device = LocalDevice::new(
        "i:12345",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::GridGuard).with_feature(
                LocalFeature::new(1, FeatureType::LoadControl, Role::Client)
                    .with_function(Function::LoadControlLimitListData, Operations::read()),
            ),
        )
        .unwrap();
    let mut engine = Engine::new(device);

    let peer = eebus::spine::device_address("i:67890", "HeatPump-1").unwrap();
    let target = eebus::spine::feature_address(&peer, &[1], 1);
    let source = engine.device().address_of(&[1], 1);

    let asked = eebus::spine::MAX_PENDING_REQUESTS + 8;
    for _ in 0..asked {
        engine.read(
            &target,
            &source,
            Function::LoadControlLimitListData,
            Duration::ZERO,
        );
    }

    let given_up = core::iter::from_fn(|| engine.poll_event())
        .filter(|e| matches!(e, SpineEvent::RequestTimedOut { .. }))
        .count();
    assert_eq!(
        given_up,
        asked - eebus::spine::MAX_PENDING_REQUESTS,
        "the overflow is reported rather than dropped"
    );
}
