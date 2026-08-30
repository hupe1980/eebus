//! The Controllable System state machine of Limitation of Power Consumption and
//! Production.
//!
//! This is the logic a heat pump, wallbox, battery, inverter or energy manager runs when
//! a grid operator's control box limits it — under §14a EnWG for consumption, under EEG
//! §9 for production. It is safety-relevant in both directions: too permissive and the
//! low-voltage grid is at risk, too strict and a building is throttled when it need not
//! be.
//!
//! One machine serves both use cases. The states and transitions come from LPC and LPP
//! UC TS 1.0.0 §2.2–2.3, which state the same thirteen transitions with the same
//! identifiers under different prefixes; the rules the 2026 implementation guides added —
//! the ordering gate, the meaning of a lost connection, and when a rejection is owed —
//! are marked with their guide section, which is also common to both. Requirements are
//! written `[LPC/LPP-901]` to say exactly that: [LPC-901] and [LPP-901] are one rule.
//!
//! What differs is not here but in [`super::Direction`]: what a device *publishes* about
//! the limit it is under.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::limitation::{
//!     ControllableSystem, CsConfig, EffectiveLimit, LimitWrite, LimitationState, LocalDecision,
//! };
//!
//! let mut cs = ControllableSystem::new(CsConfig::new(4_200.0, Duration::from_secs(7_200)),
//!                                      Duration::ZERO);
//!
//! // A restart starts limited by the failsafe value, not unlimited. [LPC/LPP-901]
//! assert_eq!(cs.state(), LimitationState::Init);
//! assert_eq!(cs.effective_limit(), EffectiveLimit::Failsafe(4_200.0));
//!
//! // The Energy Guard establishes the link, then sets a limit.
//! let mut now = Duration::from_secs(10);
//! cs.on_heartbeat(now);
//! now += Duration::from_secs(1);
//! let outcome = cs.on_limit_write(&LimitWrite::active(3_000.0), LocalDecision::Apply, now);
//!
//! assert!(outcome.is_accepted());
//! assert_eq!(cs.state(), LimitationState::Limited);
//! assert_eq!(cs.effective_limit(), EffectiveLimit::Active(3_000.0));
//! ```

use core::time::Duration;

use crate::model::ErrorNumber;

/// No heartbeat for this long moves the system into the failsafe state
/// ([LPC/LPP-911], [LPC/LPP-912]).
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(120);

/// A limit write counts only if a heartbeat arrived within this window before it
/// ([LPC/LPP-913]; implementation guide §2.11).
pub const WRITE_WINDOW: Duration = Duration::from_secs(60);

/// Without a heartbeat *and* a following limit write within this time, a system that
/// has just restarted stops waiting and runs unlimited ([LPC/LPP-906], [LPC/LPP-921]).
pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The permitted range for the Failsafe Duration Minimum ([LPC/LPP-022/1], [LPC/LPP-022/3]).
pub const FAILSAFE_DURATION_RANGE: core::ops::RangeInclusive<Duration> =
    Duration::from_secs(2 * 3_600)..=Duration::from_secs(24 * 3_600);

/// The states of LPC/LPP UC TS §2.3.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitationState {
    /// Just (re)started: limited by the failsafe value until the Energy Guard is heard
    /// from ([LPC/LPP-901]).
    Init,
    /// Under the Energy Guard's control, with an active limit applied.
    Limited,
    /// Under the Energy Guard's control, with no limit in force.
    UnlimitedControlled,
    /// The Energy Guard has gone quiet; the failsafe value applies.
    FailsafeState,
    /// The Energy Guard has been absent long enough that the system runs on its own.
    UnlimitedAutonomous,
}

/// What the system is actually limited to right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EffectiveLimit {
    /// No limitation.
    None,
    /// The Energy Guard's active power limit, in watts.
    Active(f64),
    /// The failsafe active power limit, in watts.
    Failsafe(f64),
}

impl EffectiveLimit {
    /// The limit in watts, or [`None`] when the device is unrestricted.
    pub fn watts(self) -> Option<f64> {
        match self {
            EffectiveLimit::None => None,
            EffectiveLimit::Active(w) | EffectiveLimit::Failsafe(w) => Some(w),
        }
    }
}

/// A write on the Active Power Consumption or Production Limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimitWrite {
    /// Whether the limit is in force ([LPC/LPP-008]).
    pub is_active: bool,
    /// The limit in watts. Always at or above zero ([LPC/LPP-001]).
    pub watts: f64,
    /// How long the limit is valid ([LPC/LPP-004]).
    ///
    /// The duration starts running when the write arrives, whether or not the limit is
    /// active, and the limit is deactivated when it reaches zero ([LPC/LPP-007]).
    pub duration: Option<Duration>,
}

impl LimitWrite {
    /// An activated limit without a duration.
    pub fn active(watts: f64) -> Self {
        Self {
            is_active: true,
            watts,
            duration: None,
        }
    }

    /// An activated limit valid for `duration`.
    pub fn active_for(watts: f64, duration: Duration) -> Self {
        Self {
            is_active: true,
            watts,
            duration: Some(duration),
        }
    }

    /// A deactivated limit, which lifts an earlier limitation.
    pub fn deactivated() -> Self {
        Self {
            is_active: false,
            watts: 0.0,
            duration: None,
        }
    }
}

/// Whether the device can actually follow a limit it has been sent.
///
/// The specification allows a rejection only for self-protection, safety, a legal or
/// regulatory requirement, or — on an energy manager — uncontrolled loads that make the
/// limit unreachable. The implementation guide §2.16 is blunt about the rest: a value
/// that is merely unchanged, or that does not affect the device, is still accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalDecision {
    /// The device will follow the limit.
    Apply,
    /// The device cannot follow it, for one of the permitted reasons.
    Reject(RejectReason),
}

/// Why a Controllable System may refuse a limit (LPC/LPP UC TS §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RejectReason {
    /// Following the limit would damage the device.
    #[error("self-protection")]
    SelfProtection,
    /// A safety-related activity is under way.
    #[error("safety-related activity")]
    SafetyRelated,
    /// A legal or regulatory requirement takes precedence.
    #[error("legal or regulatory requirement")]
    Regulatory,
    /// Uncontrolled loads prevent an energy manager from reaching the limit.
    #[error("uncontrolled loads prevent achieving the limit")]
    UncontrolledLoads,
}

/// What a write was answered with.
///
/// SPINE carries these as a `result` message: `errorNumber` 0 for an acceptance and 7,
/// "command rejected", for a refusal. Under §14a the pair is also the evidence the
/// operator is required to be able to produce (implementation guide §4.1.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Accepted: `ACK`, [LPC/LPP-002].
    Accepted,
    /// Refused: `NACK`, [LPC/LPP-003].
    Rejected(NackReason),
}

impl WriteOutcome {
    /// True for an acceptance.
    pub fn is_accepted(self) -> bool {
        matches!(self, WriteOutcome::Accepted)
    }

    /// The SPINE `errorNumber` this outcome is reported with.
    ///
    /// [LPC/LPP-002] answers an accepted write with a plain acknowledgement; [LPC/LPP-003]
    /// answers a refused one with `commandRejected`, which is the only refusal the use
    /// case defines — the reason itself is local, and is not sent to the Energy Guard.
    pub fn error_number(self) -> ErrorNumber {
        match self {
            WriteOutcome::Accepted => ErrorNumber::None,
            WriteOutcome::Rejected(_) => ErrorNumber::CommandRejected,
        }
    }
}

/// Why a write was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NackReason {
    /// The device cannot follow the value.
    #[error("cannot apply the value: {0}")]
    CannotApply(RejectReason),
    /// A negative limit, which [LPC/LPP-001] and implementation guide §3.6 forbid.
    #[error("a limit below zero is not permitted")]
    NegativeValue,
    /// A failsafe duration outside the two-to-twenty-four-hour range ([LPC/LPP-022/4]).
    #[error("the failsafe duration minimum must be between 2 and 24 hours")]
    DurationOutOfRange,
    /// A write that arrived without a recent heartbeat.
    ///
    /// Implementation guide §2.14: without one, the system cannot tell that the Energy
    /// Guard is still in control, and leaving the failsafe state on its word would put
    /// the grid connection at risk.
    #[error("no heartbeat within the last 60 seconds")]
    NoRecentHeartbeat,
    /// A write on a data point other than the limit, before the opening sequence of a
    /// heartbeat and a limit write has completed (implementation guide §2.11).
    #[error("the heartbeat-then-limit sequence has not completed")]
    SequenceIncomplete,
}

/// How this Controllable System is set up.
#[derive(Clone, Debug, PartialEq)]
pub struct CsConfig {
    /// The Failsafe Consumption or Production Active Power Limit in watts ([LPC/LPP-021]).
    ///
    /// Pre-configured by the vendor or installer, and changeable by the Energy Guard —
    /// which the implementation guide §2.15 makes mandatory, because a device stuck on
    /// a factory default cannot protect anything.
    pub failsafe_watts: f64,
    /// The Failsafe Duration Minimum ([LPC/LPP-022]), between two and twenty-four hours.
    pub failsafe_duration: Duration,
    /// The largest Failsafe Duration Minimum this device accepts ([LPC/LPP-022/4]).
    pub failsafe_duration_max: Duration,
    /// The nominal maximum power the device can draw or feed in ([LPC/LPP-041]), if it is a device
    /// rather than an energy manager.
    pub nominal_max_watts: Option<f64>,
    /// The contractual maximum an energy manager is allowed to draw or feed in ([LPC/LPP-042]).
    pub contractual_max_watts: Option<f64>,
}

impl CsConfig {
    /// A configuration with the two mandatory failsafe values.
    pub fn new(failsafe_watts: f64, failsafe_duration: Duration) -> Self {
        Self {
            failsafe_watts,
            failsafe_duration,
            failsafe_duration_max: *FAILSAFE_DURATION_RANGE.end(),
            nominal_max_watts: None,
            contractual_max_watts: None,
        }
    }

    /// Sets the device's nominal maximum ([LPC/LPP-041]).
    #[must_use]
    pub fn with_nominal_max(mut self, watts: f64) -> Self {
        self.nominal_max_watts = Some(watts);
        self
    }

    /// Sets the contractual maximum of an energy manager ([LPC/LPP-042]).
    #[must_use]
    pub fn with_contractual_max(mut self, watts: f64) -> Self {
        self.contractual_max_watts = Some(watts);
        self
    }
}

/// The Controllable System actor of Limitation of Power Consumption or Production.
#[derive(Clone, Debug)]
pub struct ControllableSystem {
    config: CsConfig,
    state: LimitationState,

    /// The active limit, once one has been accepted.
    limit: Option<LimitWrite>,
    /// When the current limit's duration runs out.
    limit_expiry: Option<Duration>,

    last_heartbeat: Option<Duration>,
    /// When the current state was entered, for the settle and failsafe timers.
    entered_state_at: Duration,
    /// When a heartbeat arrived in a state that is waiting for a limit write to follow.
    ///
    /// [LPC/LPP-921] measures its window from that heartbeat, not from entering the state,
    /// so that an Energy Guard which is alive but not controlling is recognised.
    awaiting_write_since: Option<Duration>,
    /// True once a heartbeat has been followed by an evaluated limit write.
    sequence_complete: bool,
}

impl ControllableSystem {
    /// Starts in `init`, limited by the failsafe value ([LPC/LPP-901]).
    pub fn new(config: CsConfig, now: Duration) -> Self {
        Self {
            config,
            state: LimitationState::Init,
            limit: None,
            limit_expiry: None,
            last_heartbeat: None,
            entered_state_at: now,
            awaiting_write_since: None,
            sequence_complete: false,
        }
    }

    /// The current state.
    pub fn state(&self) -> LimitationState {
        self.state
    }

    /// The configuration, including the failsafe values the Energy Guard may read.
    pub fn config(&self) -> &CsConfig {
        &self.config
    }

    /// What the device is limited to right now.
    ///
    /// This is the value to act on; everything else in this type exists to compute it.
    pub fn effective_limit(&self) -> EffectiveLimit {
        match self.state {
            LimitationState::Init | LimitationState::FailsafeState => {
                EffectiveLimit::Failsafe(self.config.failsafe_watts)
            }
            LimitationState::Limited => match &self.limit {
                Some(limit) if limit.is_active => EffectiveLimit::Active(limit.watts),
                _ => EffectiveLimit::None,
            },
            LimitationState::UnlimitedControlled | LimitationState::UnlimitedAutonomous => {
                EffectiveLimit::None
            }
        }
    }

    /// Whether the limit is currently activated, which is what
    /// `loadControlLimitData.isLimitActive` reports ([LPC/LPP-009]).
    pub fn is_limit_active(&self) -> bool {
        self.state == LimitationState::Limited && self.limit.is_some_and(|l| l.is_active)
    }

    /// Records a heartbeat from the Energy Guard ([LPC/LPP-031]).
    pub fn on_heartbeat(&mut self, now: Duration) {
        self.last_heartbeat = Some(now);
        if self.state == LimitationState::FailsafeState && self.awaiting_write_since.is_none() {
            self.awaiting_write_since = Some(now);
        }
    }

    /// Applies a write on the Active Power Consumption or Production Limit.
    ///
    /// `decision` is the device's own answer to "can I follow this?", which only the
    /// device can give. Everything else — the ordering rules, the value range, the state
    /// transition — is decided here.
    pub fn on_limit_write(
        &mut self,
        write: &LimitWrite,
        decision: LocalDecision,
        now: Duration,
    ) -> WriteOutcome {
        // [LPC/LPP-001], implementation guide §3.6: a limit below zero is never valid.
        if write.watts < 0.0 {
            return WriteOutcome::Rejected(NackReason::NegativeValue);
        }

        // Implementation guide §2.11 and §2.14: outside the controlled states, a write
        // is evaluated only when a heartbeat arrived within the last 60 seconds. Without
        // one there is no evidence the Energy Guard is still in control, so the failsafe
        // state must not be left.
        if self.needs_recent_heartbeat() && !self.heartbeat_is_recent(now) {
            return WriteOutcome::Rejected(NackReason::NoRecentHeartbeat);
        }

        if let LocalDecision::Reject(reason) = decision {
            // [LPC/LPP-907]: a rejection leaves the controlled states untouched. From the
            // failsafe or autonomous states the write still counts as contact, and
            // [LPC/LPP-918] moves the system to unlimited/controlled.
            match self.state {
                LimitationState::Limited | LimitationState::UnlimitedControlled => {}
                _ => self.enter(LimitationState::UnlimitedControlled, now),
            }
            self.sequence_complete = true;
            self.awaiting_write_since = None;
            return WriteOutcome::Rejected(NackReason::CannotApply(reason));
        }

        self.sequence_complete = true;
        self.awaiting_write_since = None;
        self.limit = Some(*write);
        self.limit_expiry = write.duration.map(|d| now + d);

        // [LPC/LPP-007] and implementation guide §2.2: a duration of zero deactivates the
        // limit at once, and is accepted rather than refused.
        let expired_immediately = write.duration == Some(Duration::ZERO);

        if write.is_active && !expired_immediately {
            // [LPC/LPP-904], [LPC/LPP-910], [LPC/LPP-919]
            self.enter(LimitationState::Limited, now);
        } else {
            // [LPC/LPP-902], [LPC/LPP-905], [LPC/LPP-909], [LPC/LPP-920]
            self.enter(LimitationState::UnlimitedControlled, now);
        }
        WriteOutcome::Accepted
    }

    /// Applies a write on the failsafe active power limit ([LPC/LPP-021]).
    pub fn on_failsafe_limit_write(&mut self, watts: f64, now: Duration) -> WriteOutcome {
        if let Some(rejection) = self.check_secondary_write(watts, now) {
            return rejection;
        }
        self.config.failsafe_watts = watts;
        WriteOutcome::Accepted
    }

    /// Applies a write on the Failsafe Duration Minimum ([LPC/LPP-022]).
    ///
    /// The value must lie between two and twenty-four hours and at or below this
    /// device's own maximum; [LPC/LPP-022/5] then requires the device to report the value it
    /// actually holds, which is what the accepted duration reflects.
    pub fn on_failsafe_duration_write(
        &mut self,
        duration: Duration,
        now: Duration,
    ) -> WriteOutcome {
        if let Some(rejection) = self.check_secondary_write(0.0, now) {
            return rejection;
        }
        if !FAILSAFE_DURATION_RANGE.contains(&duration)
            || duration > self.config.failsafe_duration_max
        {
            return WriteOutcome::Rejected(NackReason::DurationOutOfRange);
        }
        self.config.failsafe_duration = duration;
        WriteOutcome::Accepted
    }

    /// Advances the timers.
    ///
    /// Call at or after the instant [`poll_timeout`](Self::poll_timeout) reported. A
    /// lost connection needs no separate notification: the heartbeats simply stop, and
    /// the failsafe timer does the rest (implementation guide §2.17).
    pub fn handle_timeout(&mut self, now: Duration) {
        // [LPC/LPP-911], [LPC/LPP-912]: no heartbeat for 120 seconds, from either controlled
        // state, means the failsafe value applies.
        if matches!(
            self.state,
            LimitationState::Limited | LimitationState::UnlimitedControlled
        ) && now.saturating_sub(self.last_heartbeat.unwrap_or(self.entered_state_at))
            >= HEARTBEAT_TIMEOUT
        {
            self.enter(LimitationState::FailsafeState, now);
            return;
        }

        // [LPC/LPP-908]: an expired duration deactivates the limit.
        if self.state == LimitationState::Limited
            && let Some(expiry) = self.limit_expiry
            && now >= expiry
        {
            self.enter(LimitationState::UnlimitedControlled, now);
            return;
        }

        match self.state {
            // [LPC/LPP-906]: nothing heard in the settle window after a restart.
            LimitationState::Init
                if now.saturating_sub(self.entered_state_at) >= SETTLE_TIMEOUT =>
            {
                self.enter(LimitationState::UnlimitedAutonomous, now);
            }
            LimitationState::FailsafeState => {
                // Two independent reasons to stop waiting for the Energy Guard:
                // [LPC/LPP-922], the Failsafe Duration Minimum has run out, and [LPC/LPP-921],
                // heartbeats resumed but no limit followed — the Energy Guard is
                // present but not in control.
                let minimum_elapsed =
                    now.saturating_sub(self.entered_state_at) >= self.config.failsafe_duration;
                let heard_but_not_controlling = self
                    .awaiting_write_since
                    .is_some_and(|since| now.saturating_sub(since) >= SETTLE_TIMEOUT);
                if minimum_elapsed || heard_but_not_controlling {
                    self.enter(LimitationState::UnlimitedAutonomous, now);
                }
            }
            _ => {}
        }
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called.
    pub fn poll_timeout(&self) -> Option<Duration> {
        let mut next: Option<Duration> = None;
        let mut consider = |deadline: Duration| {
            next = Some(next.map_or(deadline, |current: Duration| current.min(deadline)));
        };

        match self.state {
            LimitationState::Limited | LimitationState::UnlimitedControlled => {
                // Without a heartbeat to measure from, the clock runs from the moment
                // the state was entered, so the failsafe is still reached.
                consider(self.last_heartbeat.unwrap_or(self.entered_state_at) + HEARTBEAT_TIMEOUT);
                if self.state == LimitationState::Limited
                    && let Some(expiry) = self.limit_expiry
                {
                    consider(expiry);
                }
            }
            LimitationState::Init => consider(self.entered_state_at + SETTLE_TIMEOUT),
            LimitationState::FailsafeState => {
                consider(self.entered_state_at + self.config.failsafe_duration);
                if let Some(since) = self.awaiting_write_since {
                    consider(since + SETTLE_TIMEOUT);
                }
            }
            LimitationState::UnlimitedAutonomous => {}
        }
        next
    }

    /// True in the states where the ordering gate applies.
    fn needs_recent_heartbeat(&self) -> bool {
        matches!(
            self.state,
            LimitationState::Init
                | LimitationState::FailsafeState
                | LimitationState::UnlimitedAutonomous
        )
    }

    fn heartbeat_is_recent(&self, now: Duration) -> bool {
        self.heartbeat_within(now, WRITE_WINDOW)
    }

    fn heartbeat_within(&self, now: Duration, window: Duration) -> bool {
        self.last_heartbeat
            .is_some_and(|hb| now.saturating_sub(hb) < window)
    }

    /// The shared checks for a write on anything other than the limit.
    fn check_secondary_write(&self, watts: f64, now: Duration) -> Option<WriteOutcome> {
        if watts < 0.0 {
            return Some(WriteOutcome::Rejected(NackReason::NegativeValue));
        }
        // Implementation guide §2.11: until a heartbeat has been followed by a limit
        // write, no other data point may be changed.
        if !self.sequence_complete {
            return Some(WriteOutcome::Rejected(NackReason::SequenceIncomplete));
        }
        if self.needs_recent_heartbeat() && !self.heartbeat_is_recent(now) {
            return Some(WriteOutcome::Rejected(NackReason::NoRecentHeartbeat));
        }
        None
    }

    fn enter(&mut self, state: LimitationState, now: Duration) {
        if self.state != state {
            self.state = state;
            self.entered_state_at = now;
            self.awaiting_write_since = None;
        }
    }
}
