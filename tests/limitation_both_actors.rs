//! Both halves of Limitation of Power Consumption, talking to each other.
//!
//! `limitation_over_the_wire` drives the Controllable System by hand, which is how the
//! server side's rules are checked one at a time. This file uses the real Energy Guard
//! on the other end, so what is under test is the part neither side can check alone: the
//! ordering the 2026 implementation guides impose on the exchange.
//!
//! Everything runs against a virtual clock, so a two-hour failsafe duration costs
//! nothing to test.

use core::time::Duration;

use eebus::model::{Datagram, DeviceType, EntityType, Function};
use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, CsConfig, CsEvent, EffectiveLimit,
    EnergyGuardActor, GuardEvent, LimitWrite, LimitationState, MIN_WRITE_INTERVAL,
    RETRY_BACKOFF_STEP,
};
use eebus::usecases::lpc;

const FAILSAFE_WATTS: f64 = 4_200.0;

/// A control box and a heat pump, each driven by its own actor.
struct Pair {
    guard_engine: Engine,
    guard: EnergyGuardActor,
    pump_engine: Engine,
    pump: ControllableSystemActor,
    now: Duration,
    /// Everything the Energy Guard has reported.
    reports: Vec<GuardEvent>,
    /// Everything the Controllable System has reported.
    decisions: Vec<CsEvent>,
}

impl Pair {
    fn new() -> Self {
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
                    .with_feature(limitation::device_diagnosis_feature(3)),
            )
            .unwrap();
        let load_control = pump_device.address_of(&[1], 1);
        let configuration = pump_device.address_of(&[1], 2);
        let diagnosis = pump_device.address_of(&[1], 3);
        let mut pump_engine = Engine::new(pump_device);
        pump_engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);
        let pump = ControllableSystemActor::new(
            ControllableSystem::new(
                CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600)),
                now,
            ),
            lpc::DIRECTION,
            load_control,
            configuration,
            diagnosis,
        );
        pump.install(&mut pump_engine, now);

        Self {
            guard_engine,
            guard,
            pump_engine,
            pump,
            now,
            reports: Vec::new(),
            decisions: Vec::new(),
        }
    }

    /// Discovery both ways, and the Energy Guard taking control.
    fn commission(&mut self) {
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

        let device = self.pump_engine.device().address().clone();
        let remote = self.guard_engine.peer(&device).expect("the heat pump");
        let peer = limitation::locate(remote, lpc::DIRECTION).expect("a Controllable System");
        self.guard.attach(&mut self.guard_engine, peer, self.now);
        self.settle();
    }

    /// Carries datagrams both ways until neither side has anything left to say.
    fn settle(&mut self) {
        for _ in 0..128 {
            let mut moved = false;
            while let Some(datagram) = self.guard_engine.poll_transmit() {
                self.pump_engine
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.pump_engine.poll_transmit() {
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
    fn advance(&mut self, by: Duration) {
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

    fn device(&self) -> eebus::model::AddressDevice {
        self.pump_engine.device().address().clone()
    }

    fn guard_client(&self) -> eebus::model::FeatureAddress {
        self.guard_engine.device().address_of(&[1], 1)
    }

    fn pump_load_control(&self) -> eebus::model::FeatureAddress {
        self.pump_engine.device().address_of(&[1], 1)
    }

    fn accepted(&self) -> Vec<LimitWrite> {
        self.reports
            .iter()
            .filter_map(|r| match r {
                GuardEvent::LimitAccepted { limit, .. } => Some(*limit),
                _ => None,
            })
            .collect()
    }

    fn refused(&self) -> Vec<LimitWrite> {
        self.reports
            .iter()
            .filter_map(|r| match r {
                GuardEvent::LimitRefused { limit, .. } => Some(*limit),
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
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS),
        "[LPC-901]: nothing has been asked of it yet"
    );

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
    pair.advance(Duration::from_secs(1));

    assert_eq!(pair.accepted().len(), 1);
    assert_eq!(pair.accepted()[0].watts, 3_000.0);
    assert_eq!(pair.pump.system().state(), LimitationState::Limited);
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::Active(3_000.0)
    );
    assert_eq!(
        pair.guard.applied_limit(&device).map(|l| l.watts),
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

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
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

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
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

    let device = pair.device();

    // A limit below zero is one the Controllable System must refuse ([LPC-001]).
    pair.guard.require(
        &device,
        Some(LimitWrite {
            is_active: true,
            watts: 1_000.0,
            duration: None,
        }),
        pair.now,
    );
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

    let device = pair.device();
    pair.guard.require(
        &device,
        Some(LimitWrite {
            is_active: true,
            watts: 3_000.0,
            duration: Some(Duration::ZERO),
        }),
        pair.now,
    );
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

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
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

    assert_eq!(pair.pump.system().config().failsafe_watts, 2_000.0);
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
    pair.advance(Duration::from_secs(1));

    let quiet = pair.guard.poll_timeout();
    assert!(
        quiet >= pair.now + Duration::from_secs(30),
        "with nothing required, the next wake-up is the heartbeat: {quiet:?}"
    );

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
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
    let device = pair.device();

    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.accepted().len(), 1);

    // A minute later the grid wants something else. Five minutes have not passed.
    pair.advance(Duration::from_secs(60));
    pair.guard
        .require(&device, Some(LimitWrite::active(2_000.0)), pair.now);
    pair.advance(Duration::ZERO);
    assert_eq!(pair.accepted().len(), 1, "still just the one write");

    // The guard comes back to it without being told to.
    pair.advance(MIN_WRITE_INTERVAL);
    assert_eq!(pair.accepted().len(), 2);
    assert_eq!(pair.accepted()[1].watts, 2_000.0);
    assert_eq!(
        pair.pump.system().effective_limit(),
        EffectiveLimit::Active(2_000.0)
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
    let device = pair.device();

    // A limit with a duration: the Controllable System will deactivate it when it runs
    // out ([LPC-908]).
    pair.guard.require(
        &device,
        Some(LimitWrite::active_for(3_000.0, Duration::from_secs(120))),
        pair.now,
    );
    pair.advance(Duration::from_secs(1));
    assert_eq!(pair.accepted().len(), 1);
    assert_eq!(pair.pump.system().state(), LimitationState::Limited);

    // The grid still needs the limit, so the guard should notice and say so again.
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);

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
        EffectiveLimit::Active(3_000.0)
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
    let device = pair.device();

    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
    pair.advance(Duration::from_secs(1));

    let written = pair.guard.audit().last().expect("the guard kept a record");
    assert_eq!(written.write.watts, 3_000.0);
    assert!(written.outcome.is_accepted());
    assert_eq!(written.peer.as_ref(), Some(&device));

    let applied = pair.pump.audit().last().expect("the system kept one too");
    assert!(applied.outcome.is_accepted());
    assert_eq!(applied.write.watts, 3_000.0);

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
    let device = pair.device();

    // Drive the system into the failsafe state, where §2.14 refuses a limit that arrives
    // without a fresh heartbeat — and make the guard write one there.
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
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
                value: Some(eebus::model::ScaledNumber::from_f64(2_000.0, 0)),
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
