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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

impl LimitationState {
    /// The state's name as LPC/LPP UC TS §2.3.2 writes it.
    ///
    /// A laboratory reads these off a debug interface and compares them against the
    /// pre-conditions in the abstract test cases, so they are the specification's
    /// spelling and not this crate's: `unlimited/controlled`, not `UnlimitedControlled`.
    pub const fn as_str(self) -> &'static str {
        match self {
            LimitationState::Init => "init",
            LimitationState::Limited => "limited",
            LimitationState::UnlimitedControlled => "unlimited/controlled",
            LimitationState::FailsafeState => "failsafe state",
            LimitationState::UnlimitedAutonomous => "unlimited/autonomous",
        }
    }

    /// The test configuration the High-Level Test Specifications name this state by.
    ///
    /// `CF_CS_Init`, `CF_CS_Limited`, `CF_CS_UnlCntrl`, `CF_CS_FS`, `CF_CS_UnlAuto`: the
    /// pre-conditions the abstract test cases are written against
    /// (LPC/LPP HLTS 1.0.2 §6.5.4). `limited` is reported without the specification's
    /// `_w_dur`/`_wo_dur` suffix, which is a property of the limit rather than the state.
    pub const fn test_configuration(self) -> &'static str {
        match self {
            LimitationState::Init => "CF_CS_Init",
            LimitationState::Limited => "CF_CS_Limited",
            LimitationState::UnlimitedControlled => "CF_CS_UnlCntrl",
            LimitationState::FailsafeState => "CF_CS_FS",
            LimitationState::UnlimitedAutonomous => "CF_CS_UnlAuto",
        }
    }
}

impl core::fmt::Display for LimitationState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
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
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize,
)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize,
)]
pub enum NackReason {
    /// The device cannot follow the value.
    #[error("cannot apply the value: {0}")]
    CannotApply(RejectReason),
    /// A negative limit, which [LPC/LPP-001] and implementation guide §3.6 forbid.
    #[error("a limit below zero is not permitted")]
    NegativeValue,
    /// A limit that is not a finite number, which [LPC/LPP-001] does not admit: a
    /// `scaledNumber` whose `scale` overflows `f64` is refused, not read as unrestricted.
    #[error("the limit is not a finite value")]
    NotFinite,
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
    /// The write named this system's limit but carried a value that could not be read.
    ///
    /// A `scaledNumber` whose `scale` overflows `f64`, most plainly. Refusing is the only
    /// safe answer: any substitute would be a limit the Energy Guard never sent.
    #[error("the limit value could not be read")]
    Unreadable,
    /// The peer refused and did not say why.
    ///
    /// This is what an Energy Guard sees, and it is not a gap in the protocol: [LPC-003]
    /// carries `errorNumber` 7 and nothing else, because the reason a Controllable System
    /// cannot follow a limit is local to it. A guard recording *why* would be recording
    /// something it invented, which in an evidence log is worse than recording nothing.
    #[error("the peer refused without stating a reason")]
    Unstated,
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
    /// Whether this Controllable System runs inside a Customer Energy Manager.
    ///
    /// The specification treats the two differently in one place: an energy manager may
    /// interrupt a limitation because uncontrolled loads make it unreachable
    /// ([LPC/LPP-901/2], [LPC/LPP-923]), and a device may not — a device has no loads but
    /// its own.
    pub on_cem: bool,
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
            on_cem: false,
        }
    }

    /// Says this Controllable System runs inside a Customer Energy Manager.
    #[must_use]
    pub fn on_cem(mut self) -> Self {
        self.on_cem = true;
        self
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

    /// The most power the device may draw or feed in right now, in watts.
    ///
    /// This is the number an appliance acts on, and it is not always the Energy Guard's:
    /// an energy manager playing the Controllable System has a Contractual Nominal Max
    /// ([LPC/LPP-042]) it must never exceed whatever the Energy Guard says, including
    /// when the Energy Guard says nothing at all. [`None`] means genuinely unbounded.
    ///
    /// ```
    /// use core::time::Duration;
    /// use eebus::usecases::limitation::{ControllableSystem, CsConfig};
    ///
    /// let cs = ControllableSystem::new(
    ///     CsConfig::new(4_200.0, Duration::from_secs(7_200)).with_contractual_max(11_000.0),
    ///     Duration::ZERO,
    /// );
    /// // In `init` the failsafe applies, and it is below the contract.
    /// assert_eq!(cs.power_ceiling(), Some(4_200.0));
    /// ```
    pub fn power_ceiling(&self) -> Option<f64> {
        match (
            self.effective_limit().watts(),
            self.config.contractual_max_watts,
        ) {
            (Some(limit), Some(contract)) => Some(limit.min(contract)),
            (Some(limit), None) => Some(limit),
            (None, contract) => contract,
        }
    }

    /// What the device is limited to right now.
    ///
    /// The limitation use case's own view: what the Energy Guard asked for, or the
    /// failsafe value standing in for it. [`power_ceiling`](Self::power_ceiling) is the
    /// number to act on, because it also honours a contractual maximum the use case
    /// knows nothing about.
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
    ///
    /// **A heartbeat on its own never establishes control.** It is evidence that the
    /// Energy Guard is alive, not that it is limiting anything, and the specification is
    /// deliberate about the difference: control is established by a *write*, and a
    /// heartbeat only opens the sixty-second window in which one counts (implementation
    /// guide §2.11 and §2.14). A system in `init` fed heartbeats and nothing else stays
    /// in `init` until the settle timer expires and then runs autonomously — which is
    /// right, because an Energy Guard that never writes is not controlling the system.
    ///
    /// The one thing a heartbeat does change is a timer: in `failsafe` it starts the
    /// [LPC/LPP-921] window, after which a guard that is audibly present but silent about
    /// limits releases the system.
    ///
    /// ```
    /// use core::time::Duration;
    /// use eebus::usecases::limitation::{ControllableSystem, CsConfig, LimitationState};
    ///
    /// let mut cs = ControllableSystem::new(
    ///     CsConfig::new(4_200.0, Duration::from_secs(7_200)),
    ///     Duration::ZERO,
    /// );
    /// for second in 1..=30 {
    ///     cs.on_heartbeat(Duration::from_secs(second));
    /// }
    /// assert_eq!(cs.state(), LimitationState::Init, "heartbeats alone establish nothing");
    /// ```
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
        // Table 1 makes the answer depend on whether a heartbeat was received *in time*
        // ([LPC/LPP-914]), which is a question about the clock rather than about this
        // message. Bringing the timers up to date first means the answer does not depend
        // on how punctually the caller happened to tick.
        self.handle_timeout(now);

        // Implementation guide §2.11 and §2.14: outside the controlled states, a write
        // is evaluated only when a heartbeat arrived within the last 60 seconds. Without
        // one there is no evidence the Energy Guard is still in control, so the failsafe
        // state must not be left.
        //
        // This gate comes *first*, and the value checks below come after it, because the
        // two refusals mean different things. A write with no heartbeat behind it is not
        // evaluated at all, and changes nothing; a write that is evaluated and found
        // unusable is a refusal, and a refusal moves the state machine. Checking the
        // value first would silently turn the second into the first
        // ([ATC_LPC_COM_NT_CSConnection_001] against [ATC_LPC_COM_PT_CSTransition1_001]).
        if self.needs_recent_heartbeat() && !self.heartbeat_is_recent(now) {
            return WriteOutcome::Rejected(NackReason::NoRecentHeartbeat);
        }

        // [LPC/LPP-001], implementation guide §3.6: a limit below zero is never valid, nor
        // is one that is not a number. An application can construct a `LimitWrite`
        // directly, so this does not rely on the wire parser having caught it.
        let refusal = if !write.watts.is_finite() {
            Some(NackReason::NotFinite)
        } else if write.watts < 0.0 {
            Some(NackReason::NegativeValue)
        } else if let LocalDecision::Reject(reason) = decision {
            Some(NackReason::CannotApply(reason))
        } else {
            None
        };

        if let Some(reason) = refusal {
            return self.refuse(reason, now);
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

    /// Refuses a limit write whose payload could not be read as a limit.
    ///
    /// A `loadControlLimitListData` that names this system's limit but carries a value it
    /// cannot represent — a `scaledNumber` whose `scale` overflows `f64` — is a limit
    /// write that was evaluated and found unusable, which is a refusal like any other and
    /// moves the state machine the same way ([LPC/LPP-902], [LPC/LPP-918]). Substituting a
    /// number the Energy Guard never sent would apply a limit nobody asked for.
    ///
    /// A write that does not address this system's limit at all is *not* this: it is not
    /// a limit write, and does not reach the state machine.
    pub fn on_unreadable_limit_write(&mut self, now: Duration) -> WriteOutcome {
        self.handle_timeout(now);
        if self.needs_recent_heartbeat() && !self.heartbeat_is_recent(now) {
            return WriteOutcome::Rejected(NackReason::NoRecentHeartbeat);
        }
        self.refuse(NackReason::Unreadable, now)
    }

    /// The state change every refused limit write makes, whatever the reason.
    ///
    /// [LPC/LPP-907]: a refusal leaves the controlled states untouched — a limited system
    /// stays limited, which is the safe answer. From `init`, `failsafe` or
    /// `unlimited/autonomous` the write is still contact with an Energy Guard that is in
    /// control, so [LPC/LPP-902] and [LPC/LPP-918] move the system to
    /// `unlimited/controlled` even though nothing was applied.
    ///
    /// A refused write also completes the opening sequence: implementation guide §2.11
    /// gates the *other* data points on a limit write having been evaluated, not on one
    /// having been accepted.
    fn refuse(&mut self, reason: NackReason, now: Duration) -> WriteOutcome {
        match self.state {
            LimitationState::Limited | LimitationState::UnlimitedControlled => {}
            _ => self.enter(LimitationState::UnlimitedControlled, now),
        }
        self.sequence_complete = true;
        self.awaiting_write_since = None;
        WriteOutcome::Rejected(reason)
    }

    /// Applies a write on the failsafe active power limit ([LPC/LPP-021]).
    pub fn on_failsafe_limit_write(&mut self, watts: f64, now: Duration) -> WriteOutcome {
        self.handle_timeout(now);
        if let Some(rejection) = self.check_secondary_write(now) {
            return rejection;
        }
        if !watts.is_finite() {
            return WriteOutcome::Rejected(NackReason::NotFinite);
        }
        // [LPC/LPP-021]: the failsafe limit is a power, and a power below zero is not one.
        if watts < 0.0 {
            return WriteOutcome::Rejected(NackReason::NegativeValue);
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
        self.handle_timeout(now);
        if let Some(rejection) = self.check_secondary_write(now) {
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

    /// Interrupts an active limitation for a reason the specification permits
    /// ([LPC/LPP-923]).
    ///
    /// The one transition out of `limited` that the device itself makes: a heat pump that
    /// must run to defrost, an inverter a regulator requires to keep feeding in. The
    /// state moves to `unlimited/controlled`, so the Energy Guard sees the limit
    /// deactivated and can act on that.
    ///
    /// The permitted reasons are narrower off a CEM than on one: `UncontrolledLoads` is a
    /// reason an energy manager may give and a device may not, since a device has no
    /// loads but its own — see [`CsConfig::on_cem`]. Returns whether the interruption
    /// took effect; it does nothing outside `limited`, where there is no limitation to
    /// interrupt.
    pub fn interrupt(&mut self, reason: RejectReason, now: Duration) -> bool {
        if self.state != LimitationState::Limited {
            return false;
        }
        if reason == RejectReason::UncontrolledLoads && !self.config.on_cem {
            return false;
        }
        self.limit = None;
        self.limit_expiry = None;
        self.enter(LimitationState::UnlimitedControlled, now);
        true
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
    ///
    /// # Invariant
    ///
    /// **[`None`] is returned in exactly one state, [`LimitationState::UnlimitedAutonomous`].**
    /// Everywhere else a deadline is pending and the answer is [`Some`] — which matters,
    /// because a caller that simply waits on this value would otherwise never reach the
    /// failsafe fallback of [LPC/LPP-911]. In `unlimited/autonomous` there is genuinely
    /// nothing to wait for: no limit to expire, no guard whose silence is being counted.
    /// The system leaves that state on a heartbeat followed by a write, and those are
    /// events, not deadlines.
    ///
    /// [`ControllableSystemActor::poll_timeout`] therefore returns a plain [`Duration`]:
    /// it also has the heartbeat producer's deadline to fold in, and that one is always
    /// pending.
    ///
    /// The value is an *absolute* instant on the same monotonic scale as the `now` passed
    /// in, not a delay: `deadline.saturating_sub(now)` is how long to sleep. A deadline
    /// that has already passed means [`handle_timeout`](Self::handle_timeout) is overdue,
    /// which is why the subtraction saturates rather than panicking.
    ///
    /// ```
    /// use core::time::Duration;
    /// use eebus::usecases::limitation::{ControllableSystem, CsConfig, LimitationState};
    ///
    /// let mut cs = ControllableSystem::new(
    ///     CsConfig::new(4_200.0, Duration::from_secs(7_200)),
    ///     Duration::ZERO,
    /// );
    /// // Waiting on the deadline is enough to drive the machine to its resting state.
    /// while let Some(deadline) = cs.poll_timeout() {
    ///     cs.handle_timeout(deadline);
    /// }
    /// assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
    /// ```
    ///
    /// [`ControllableSystemActor::poll_timeout`]: super::ControllableSystemActor::poll_timeout
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

    /// The gates a write on anything other than the limit has to pass first.
    ///
    /// Ordering matters here for the same reason it does in
    /// [`on_limit_write`](Self::on_limit_write): a write that is not evaluated is a
    /// different answer from one that is evaluated and refused, and the ordering gates
    /// come before the value.
    fn check_secondary_write(&self, now: Duration) -> Option<WriteOutcome> {
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
        if self.state == state {
            return;
        }
        self.state = state;
        self.entered_state_at = now;
        self.awaiting_write_since = None;

        // [LPC/LPP-TS-037] states the ordering gate per *state*, not once per device: "in
        // state init, failsafe state or unlimited/autonomous, only after a heartbeat and a
        // write command from the EG within 60 seconds on the APCL, commands on any other
        // data point SHALL be evaluated". Falling back into one of those states is losing
        // the Energy Guard, so the sequence has to be run again before the failsafe values
        // may be changed — `ATC_LPC_COM_PT_CSFS_003` is the laboratory's test for it, and
        // a system that kept the flag set would let a stale controller rewrite its
        // failsafe limit while nobody is in charge.
        if matches!(
            state,
            LimitationState::FailsafeState | LimitationState::UnlimitedAutonomous
        ) {
            self.sequence_complete = false;
        }
    }
}

impl crate::usecases::signals::Signals<super::Direction> for ControllableSystem {
    /// What a certification laboratory reads off the device.
    ///
    /// The order is the parameter sheet's: the state first, then the limit and its
    /// duration, then the failsafe pair, then the constraints. `lpc:effectiveLimit` and
    /// `lpc:powerCeiling` are not data points of the specification — they are what this
    /// implementation has concluded from them, and they are here because a tester
    /// checking `ATC_LPC_COM_PT_CSUnlAuto_002` ("does not consume higher than the nominal
    /// maximum") is really checking the second one.
    ///
    /// ```
    /// use core::time::Duration;
    /// use eebus::usecases::limitation::{ControllableSystem, CsConfig};
    /// use eebus::usecases::lpc;
    /// use eebus::usecases::signals::Signals;
    ///
    /// let cs = ControllableSystem::new(
    ///     CsConfig::new(4_200.0, Duration::from_secs(7_200)),
    ///     Duration::ZERO,
    /// );
    /// // A fresh system is in `init`, limited by the failsafe value and nothing else.
    /// let signals = cs.signals(lpc::DIRECTION);
    /// assert_eq!(signals.get("lpc:state").and_then(|v| v.as_str()), Some("init"));
    /// assert!(signals.get("lpc:limit").is_some_and(|v| v.is_absent()));
    /// assert_eq!(
    ///     signals.get("lpc:effectiveLimit").and_then(|v| v.as_f64()),
    ///     Some(4_200.0),
    /// );
    /// ```
    fn signals(&self, direction: super::Direction) -> crate::usecases::signals::SignalSet {
        use crate::usecases::signals::{Signal, SignalSet, SignalValue};
        use alloc::borrow::Cow;
        use alloc::format;

        let prefix = direction.signal_prefix();
        let name = |data_point: &str| -> Cow<'static, str> {
            Cow::Owned(format!("{prefix}:{data_point}"))
        };
        let config = &self.config;

        SignalSet::new()
            .with(Signal::new(
                name("state"),
                SignalValue::Text(Cow::Borrowed(self.state.as_str())),
            ))
            .with(
                Signal::new(
                    name("limit"),
                    SignalValue::number(
                        self.limit
                            .filter(|_| self.state == LimitationState::Limited)
                            .map(|l| l.watts),
                    ),
                )
                .in_unit("W"),
            )
            .with(Signal::new(
                name("duration"),
                SignalValue::seconds(self.limit.and_then(|l| l.duration)),
            ))
            .with(Signal::new(
                name("isActive"),
                SignalValue::Bool(self.is_limit_active()),
            ))
            .with(
                Signal::new(
                    name("failsafeLimit"),
                    SignalValue::Number(config.failsafe_watts),
                )
                .in_unit("W"),
            )
            .with(Signal::new(
                name("failsafeDuration"),
                SignalValue::Seconds(config.failsafe_duration.as_secs_f64()),
            ))
            .with(
                Signal::new(
                    name("nominalMax"),
                    SignalValue::number(config.nominal_max_watts.filter(|_| !config.on_cem)),
                )
                .in_unit("W"),
            )
            .with(
                Signal::new(
                    name("contractualMax"),
                    SignalValue::number(config.contractual_max_watts.filter(|_| config.on_cem)),
                )
                .in_unit("W"),
            )
            .with(
                Signal::new(
                    name("effectiveLimit"),
                    SignalValue::number(self.effective_limit().watts()),
                )
                .in_unit("W"),
            )
            .with(
                Signal::new(
                    name("powerCeiling"),
                    SignalValue::number(self.power_ceiling()),
                )
                .in_unit("W"),
            )
            .with(Signal::new(
                name("lastHeartbeat"),
                SignalValue::seconds(self.last_heartbeat),
            ))
            .with(Signal::new(
                name("nextDeadline"),
                SignalValue::seconds(self.poll_timeout()),
            ))
    }
}
