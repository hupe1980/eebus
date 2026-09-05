//! Control of Battery (COB).
//!
//! The counterpart of [MOB](crate::usecases::mob): that one reads a battery, this one
//! drives it. An energy manager tells an inverter how much its battery should charge or
//! discharge — and, exactly as in [LPC](crate::usecases::lpc), the inverter falls back to
//! a safe value when the manager falls silent.
//!
//! Five scenarios, all mandatory for both actors: the two control modes, the configuration
//! parameters, the failsafe values, and the heartbeat that ties them together.
//!
//! # Two ways to say the same thing
//!
//! The specification's central idea is that a manager may control a battery in either of
//! two frames, and says which with the **Active Control Mode** ([COB-031]):
//!
//! * **`power`** — "charge at 3 kW". The setpoint is the battery's own power, and which
//!   one depends on the machine: a *battery inverter* has only an AC side, so
//!   [COB-011] sets AC power; a *hybrid inverter* also has PV strings on its DC bus, so
//!   [COB-012] sets DC power and the AC side is whatever the sun leaves over.
//! * **`pcc`** — "hold the grid connection at zero". The setpoint is the power at the
//!   *point of common coupling* ([COB-021]) and the inverter works out its own number from
//!   what it measures there. This is what self-consumption optimisation actually wants,
//!   and it is why the inverter rather than the manager does the arithmetic: the inverter
//!   can react in milliseconds and a manager on a network cannot.
//! * **`auto`** — the inverter's own business logic. Not a mode a manager sets so much as
//!   one it is told about ([COB-910/1]).
//!
//! Everything here follows the **passive sign convention** ([COB-001/1]): positive is
//! consumption, negative is production. A setpoint of −3 000 W is discharging at three
//! kilowatts, and a caller that gets the sign wrong charges when it meant to discharge.
//!
//! # The safety story
//!
//! The same shape as LPC, for the same reason. The manager's heartbeat is due every sixty
//! seconds; two minutes without one and the inverter enters `failsafe`, where it is driven
//! by the failsafe setpoints and every control-mode setpoint is deactivated ([COB-913],
//! [COB-009/2]). It stays there for at least the Failsafe Duration Minimum — two to
//! twenty-four hours ([COB-044/1]) — and then gives up on the manager and runs on its own.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::cob::{
//!     BatteryControl, CobConfig, CobState, ControlMode, EffectiveControl, InverterKind,
//!     SetpointWrite,
//! };
//!
//! // A hybrid inverter that falls back to holding the grid connection at zero.
//! let mut inverter = BatteryControl::new(
//!     CobConfig::new(InverterKind::Hybrid, Duration::from_secs(2 * 3_600))
//!         .with_failsafe(0.0)
//!         .with_default(0.0),
//!     Duration::ZERO,
//! );
//! assert_eq!(inverter.state(), CobState::Init);
//!
//! // The manager establishes control: a heartbeat, then the mode.
//! let mut now = Duration::from_secs(10);
//! inverter.on_heartbeat(now);
//! now += Duration::from_secs(1);
//! assert!(inverter.on_control_mode(ControlMode::Power, true, now).is_accepted());
//! assert_eq!(inverter.state(), CobState::PowerControl);
//!
//! // …and then charges the battery at 3 kW. Positive is consumption.
//! now += Duration::from_secs(1);
//! assert!(inverter.on_setpoint(&SetpointWrite::active(3_000.0), true, now).is_accepted());
//! assert_eq!(inverter.effective(), EffectiveControl::Setpoint(3_000.0));
//!
//! // Two minutes of silence and the failsafe applies. [COB-913]
//! inverter.handle_timeout(now + Duration::from_secs(121));
//! assert_eq!(inverter.state(), CobState::Failsafe);
//! assert_eq!(inverter.effective(), EffectiveControl::Failsafe(0.0));
//! ```

use alloc::string::ToString;
use alloc::vec;
use core::time::Duration;

use crate::model::{
    CmdData, DeviceConfigurationKeyId, DeviceConfigurationKeyName, DeviceConfigurationKeyValueData,
    DeviceConfigurationKeyValueDescriptionData, DeviceConfigurationKeyValueDescriptionListData,
    DeviceConfigurationKeyValueListData, DeviceConfigurationKeyValueType,
    DeviceConfigurationKeyValueValue, EntityType, ErrorNumber, FeatureType, Function, Role,
    ScaledNumber, ScopeType, SetpointData, SetpointDescriptionData, SetpointDescriptionListData,
    SetpointId, SetpointListData, SetpointType, UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};
use crate::usecases::monitoring::functions::{
    CLIENT_CONFIGURATION_WRITES, SERVER_CONFIGURATION_WRITEABLE,
};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` on the wire.
pub const NAME: &str = "controlOfBattery";

/// The actor an inverter announces itself as.
pub const INVERTER_ACTOR: &str = "Inverter";

/// The actor an energy manager announces itself as.
pub const CEM_ACTOR: &str = "CEM";

/// How often the energy manager sends its heartbeat: "at least every 60 seconds"
/// ([COB-008]).
///
/// The cadence, not the tolerance — [`HEARTBEAT_TIMEOUT`] is twice it. Reported by the
/// descriptors as
/// [`Delivery::Periodic`](crate::usecases::descriptor::Delivery::Periodic).
pub const HEARTBEAT_PERIOD: Duration = Duration::from_secs(60);

/// No heartbeat for this long moves the inverter into the failsafe state ([COB-913]).
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(120);

/// A write on the Active Control Mode counts only if a heartbeat arrived within this
/// window before it ([COB-495]).
pub const WRITE_WINDOW: Duration = Duration::from_secs(60);

/// Without a heartbeat *and* a following control-mode write within this time, an inverter
/// that has just restarted stops waiting and runs on its own ([COB-905], [COB-916]).
pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The permitted range for the Failsafe Duration Minimum ([COB-044/1], [COB-044/3]).
pub const FAILSAFE_DURATION_RANGE: core::ops::RangeInclusive<Duration> =
    Duration::from_secs(2 * 3_600)..=Duration::from_secs(24 * 3_600);

/// The `setpointId` this implementation gives the control-mode setpoint.
pub const SETPOINT_ID: SetpointId = SetpointId(1);

/// The `keyId` of the Active Control Mode.
pub const CONTROL_MODE_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(1);

/// The `keyName` the Active Control Mode is published under (COB Table 20).
///
/// This is the fixed half — the element the specification pins — and what an energy
/// manager resolves an inverter's own `keyId` from.
pub const CONTROL_MODE_KEY_NAME: DeviceConfigurationKeyName =
    DeviceConfigurationKeyName::BatteryActiveControlMode;

/// The `keyId` of the default setpoint.
pub const DEFAULT_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(2);

/// The `keyId` of the failsafe setpoint.
pub const FAILSAFE_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(3);

/// The `keyId` of the Failsafe Duration Minimum.
pub const FAILSAFE_DURATION_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(4);

/// What kind of machine is being controlled ([COB-970]).
///
/// It decides which setpoint the `power` control mode uses, and the two are not
/// interchangeable: a hybrid inverter's AC output is its battery's DC power *plus*
/// whatever the strings are producing, so telling it an AC number would not say what the
/// battery should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InverterKind {
    /// A battery inverter: an AC side and a battery, and nothing else ([COB-970/3]).
    Battery,
    /// A hybrid inverter: a battery *and* PV strings on the same DC bus ([COB-970/2]).
    Hybrid,
}

/// How an energy manager is controlling the battery right now ([COB-031]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlMode {
    /// `power`: the setpoint is the battery's own power ([COB-011], [COB-012]).
    Power,
    /// `pcc`: the setpoint is the power at the grid connection point ([COB-021]).
    ///
    /// The inverter works out its own number from what it measures there, which is what
    /// self-consumption optimisation wants: the inverter reacts in milliseconds and an
    /// energy manager on a network cannot.
    Pcc,
    /// `auto`: the inverter's own business logic ([COB-031/3]).
    Auto,
}

impl ControlMode {
    /// The string that crosses the wire, as `value.string`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Power => "power",
            Self::Pcc => "pcc",
            Self::Auto => "auto",
        }
    }

    /// Reads one off the wire.
    pub fn read(value: &str) -> Option<Self> {
        Some(match value {
            "power" => Self::Power,
            "pcc" => Self::Pcc,
            "auto" => Self::Auto,
            _ => return None,
        })
    }
}

/// The states of COB §2.4.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobState {
    /// Just (re)started: driven by the failsafe setpoints until the manager is heard from
    /// ([COB-901]).
    Init,
    /// Under control, in the `power` mode.
    PowerControl,
    /// Under control, in the `pcc` mode.
    PccControl,
    /// Under control, but running its own logic — the manager is heard from and is not
    /// setting a number ([COB-910/1]).
    AutoControl,
    /// Running its own logic with no manager at all ([COB-905]).
    AutoUncontrolled,
    /// The manager has gone quiet; the failsafe setpoints apply ([COB-913]).
    Failsafe,
}

impl CobState {
    /// The state's name as COB §2.4.2 writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "Init",
            Self::PowerControl => "Power Control State",
            Self::PccControl => "PCC Control State",
            Self::AutoControl => "Auto Control State",
            Self::AutoUncontrolled => "Auto Uncontrolled State",
            Self::Failsafe => "Failsafe State",
        }
    }

    /// The control mode this state corresponds to, where it has one.
    pub const fn control_mode(self) -> Option<ControlMode> {
        match self {
            Self::PowerControl => Some(ControlMode::Power),
            Self::PccControl => Some(ControlMode::Pcc),
            Self::AutoControl | Self::AutoUncontrolled => Some(ControlMode::Auto),
            Self::Init | Self::Failsafe => None,
        }
    }
}

impl core::fmt::Display for CobState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What is actually driving the inverter right now.
///
/// Four different things, and an application that treated them alike would misreport why
/// a battery is doing what it is doing — which, in a household that is being billed for
/// it, is the whole question.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EffectiveControl {
    /// The manager's setpoint, in watts, passive sign convention.
    Setpoint(f64),
    /// The default setpoint, because the manager's is deactivated or expired ([COB-917]).
    Default(f64),
    /// The failsafe setpoint, because the manager has gone quiet ([COB-913]).
    Failsafe(f64),
    /// Nothing: the inverter is running its own logic.
    Autonomous,
}

impl EffectiveControl {
    /// The setpoint in watts, or [`None`] when the inverter is on its own.
    pub fn watts(self) -> Option<f64> {
        match self {
            Self::Setpoint(w) | Self::Default(w) | Self::Failsafe(w) => Some(w),
            Self::Autonomous => None,
        }
    }

    /// Whether the manager is the one deciding.
    pub fn is_controlled(self) -> bool {
        matches!(self, Self::Setpoint(_) | Self::Default(_))
    }
}

/// A write on a control-mode setpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SetpointWrite {
    /// Whether the setpoint is in force ([COB-004/1]).
    pub is_active: bool,
    /// The setpoint in watts, passive sign convention: positive charges, negative
    /// discharges.
    pub watts: f64,
    /// How long the setpoint is valid ([COB-005/1]).
    ///
    /// The duration starts running when the write arrives, **whether or not the setpoint
    /// is activated** — the specification is explicit about that — and the setpoint is
    /// deactivated when it reaches zero ([COB-006/1]).
    pub duration: Option<Duration>,
}

impl SetpointWrite {
    /// An activated setpoint without a duration.
    pub fn active(watts: f64) -> Self {
        Self {
            is_active: true,
            watts,
            duration: None,
        }
    }

    /// An activated setpoint valid for `duration`.
    pub fn active_for(watts: f64, duration: Duration) -> Self {
        Self {
            is_active: true,
            watts,
            duration: Some(duration),
        }
    }

    /// A deactivated setpoint, which hands the inverter back to its default.
    pub fn deactivated() -> Self {
        Self {
            is_active: false,
            watts: 0.0,
            duration: None,
        }
    }
}

/// What a write was answered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Accepted: `ACK` ([COB-002/1]).
    Accepted,
    /// Refused: `NACK` ([COB-003/1]). The old setpoint stands.
    Rejected(NackReason),
}

impl WriteOutcome {
    /// True for an acceptance.
    pub fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }

    /// The SPINE `errorNumber` this outcome is reported with.
    pub fn error_number(self) -> ErrorNumber {
        match self {
            Self::Accepted => ErrorNumber::None,
            Self::Rejected(_) => ErrorNumber::CommandRejected,
        }
    }
}

/// Why a write was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NackReason {
    /// The inverter cannot apply the value.
    #[error("the inverter cannot apply this setpoint")]
    CannotApply,
    /// The value is not a finite number.
    #[error("the setpoint is not a finite value")]
    NotFinite,
    /// A failsafe duration outside the two-to-twenty-four-hour range ([COB-044/3]).
    #[error("the failsafe duration minimum must be between 2 and 24 hours")]
    DurationOutOfRange,
    /// A write that arrived without a recent heartbeat ([COB-495]).
    #[error("no heartbeat within the last 60 seconds")]
    NoRecentHeartbeat,
    /// A setpoint written before the control mode was established.
    ///
    /// §2.3: the heartbeat comes first, then the Active Control Mode, and only then
    /// anything else. An inverter that took a setpoint before knowing which frame it was
    /// in would apply a grid-connection number to its battery, or the reverse.
    #[error("the active control mode has not been established")]
    ModeNotEstablished,
}

/// How this inverter is set up.
#[derive(Clone, Debug, PartialEq)]
pub struct CobConfig {
    /// Which setpoint the `power` mode uses.
    pub kind: InverterKind,
    /// The Failsafe Duration Minimum ([COB-044]), between two and twenty-four hours.
    pub failsafe_duration: Duration,
    /// The failsafe setpoint, in watts ([COB-041], [COB-042], [COB-043]).
    pub failsafe_watts: f64,
    /// The default setpoint, applied when the manager's is deactivated ([COB-032] to
    /// [COB-034]).
    pub default_watts: f64,
}

impl CobConfig {
    /// A configuration with the mandatory failsafe duration.
    ///
    /// Both power values start at nought, which is the safe answer for a battery: a
    /// machine that does not know what to do should do nothing, rather than charge or
    /// discharge on a guess.
    pub fn new(kind: InverterKind, failsafe_duration: Duration) -> Self {
        Self {
            kind,
            failsafe_duration,
            failsafe_watts: 0.0,
            default_watts: 0.0,
        }
    }

    /// Sets the failsafe setpoint, in watts.
    #[must_use]
    pub fn with_failsafe(mut self, watts: f64) -> Self {
        self.failsafe_watts = watts;
        self
    }

    /// Sets the default setpoint, in watts.
    #[must_use]
    pub fn with_default(mut self, watts: f64) -> Self {
        self.default_watts = watts;
        self
    }

    /// The `keyName` of the default setpoint, which depends on the control mode and on
    /// what kind of machine this is.
    pub fn default_key(&self, mode: ControlMode) -> DeviceConfigurationKeyName {
        match (mode, self.kind) {
            (ControlMode::Pcc, _) => DeviceConfigurationKeyName::DefaultPccPower,
            (_, InverterKind::Battery) => DeviceConfigurationKeyName::DefaultAcPower,
            (_, InverterKind::Hybrid) => DeviceConfigurationKeyName::DefaultDcPower,
        }
    }

    /// The `keyName` of the failsafe setpoint, likewise.
    pub fn failsafe_key(&self, mode: ControlMode) -> DeviceConfigurationKeyName {
        match (mode, self.kind) {
            (ControlMode::Pcc, _) => DeviceConfigurationKeyName::FailsafePccPowerSetpoint,
            (_, InverterKind::Battery) => DeviceConfigurationKeyName::FailsafeAcPowerSetpoint,
            (_, InverterKind::Hybrid) => DeviceConfigurationKeyName::FailsafeDcPowerSetpoint,
        }
    }

    /// The `scopeType` of the setpoint the `power` mode writes ([COB-011], [COB-012]).
    pub const fn power_scope(&self) -> ScopeType {
        match self.kind {
            InverterKind::Battery => ScopeType::BatteryAcPower,
            InverterKind::Hybrid => ScopeType::BatteryDcPower,
        }
    }
}

/// The Inverter actor of Control of Battery.
///
/// The state machine of §2.4, driven the same way as everything else in this crate: give
/// it a monotonic instant and tell it what happened.
#[derive(Clone, Debug)]
pub struct BatteryControl {
    config: CobConfig,
    state: CobState,
    /// The manager's setpoint, once one has been accepted.
    setpoint: Option<SetpointWrite>,
    /// When the current setpoint's duration runs out.
    setpoint_expiry: Option<Duration>,
    last_heartbeat: Option<Duration>,
    entered_state_at: Duration,
    /// When a heartbeat arrived in a state waiting for a control-mode write to follow.
    awaiting_mode_since: Option<Duration>,
}

impl BatteryControl {
    /// Starts in `Init`, driven by the failsafe setpoints ([COB-901]).
    pub fn new(config: CobConfig, now: Duration) -> Self {
        Self {
            config,
            state: CobState::Init,
            setpoint: None,
            setpoint_expiry: None,
            last_heartbeat: None,
            entered_state_at: now,
            awaiting_mode_since: None,
        }
    }

    /// The current state.
    pub fn state(&self) -> CobState {
        self.state
    }

    /// The configuration.
    pub fn config(&self) -> &CobConfig {
        &self.config
    }

    /// The manager's setpoint, if one has been accepted.
    pub fn setpoint(&self) -> Option<SetpointWrite> {
        self.setpoint
    }

    /// What is actually driving the inverter right now.
    pub fn effective(&self) -> EffectiveControl {
        match self.state {
            CobState::Init | CobState::Failsafe => {
                EffectiveControl::Failsafe(self.config.failsafe_watts)
            }
            CobState::AutoControl | CobState::AutoUncontrolled => EffectiveControl::Autonomous,
            CobState::PowerControl | CobState::PccControl => match self.setpoint {
                // [COB-917]: a deactivated setpoint hands the inverter to its default,
                // which is a number and not "do nothing".
                Some(setpoint) if setpoint.is_active => EffectiveControl::Setpoint(setpoint.watts),
                _ => EffectiveControl::Default(self.config.default_watts),
            },
        }
    }

    /// Whether the setpoint is currently activated, which is what `setpointListData`
    /// reports ([COB-009]).
    pub fn is_setpoint_active(&self) -> bool {
        matches!(self.state, CobState::PowerControl | CobState::PccControl)
            && self.setpoint.is_some_and(|s| s.is_active)
    }

    /// Records a heartbeat from the energy manager.
    ///
    /// **A heartbeat on its own never establishes control**, exactly as in LPC: it opens
    /// the sixty-second window in which a write on the Active Control Mode counts, and
    /// nothing more ([COB-495]).
    pub fn on_heartbeat(&mut self, now: Duration) {
        self.last_heartbeat = Some(now);
        if matches!(self.state, CobState::Failsafe | CobState::AutoUncontrolled)
            && self.awaiting_mode_since.is_none()
        {
            self.awaiting_mode_since = Some(now);
        }
    }

    /// Applies a write on the Active Control Mode ([COB-031]).
    ///
    /// `accepted` is the inverter's own answer to "can I run in this mode?", which only
    /// the inverter can give. A mode it refuses moves it to `Auto Control` rather than
    /// leaving it where it was ([COB-904/3], [COB-914/3]): the manager is present and the
    /// inverter is deciding for itself, which is a different thing from either side of it.
    pub fn on_control_mode(
        &mut self,
        mode: ControlMode,
        accepted: bool,
        now: Duration,
    ) -> WriteOutcome {
        self.handle_timeout(now);

        if self.needs_recent_heartbeat() && !self.heartbeat_is_recent(now) {
            return WriteOutcome::Rejected(NackReason::NoRecentHeartbeat);
        }
        self.awaiting_mode_since = None;

        if !accepted {
            self.enter(CobState::AutoControl, now);
            return WriteOutcome::Rejected(NackReason::CannotApply);
        }

        match mode {
            ControlMode::Power => self.enter(CobState::PowerControl, now),
            ControlMode::Pcc => self.enter(CobState::PccControl, now),
            ControlMode::Auto => self.enter(CobState::AutoControl, now),
        }
        WriteOutcome::Accepted
    }

    /// The inverter deciding for itself, while the manager is still there ([COB-910/1]).
    ///
    /// Transitions 6 and 9. Does nothing outside the two controlled states, where there is
    /// no control to hand back.
    pub fn go_autonomous(&mut self, now: Duration) -> bool {
        if !matches!(self.state, CobState::PowerControl | CobState::PccControl) {
            return false;
        }
        self.enter(CobState::AutoControl, now);
        true
    }

    /// Applies a write on the control-mode setpoint ([COB-011], [COB-012], [COB-021]).
    ///
    /// `accepted` is the inverter's own answer. A refusal leaves the old setpoint standing
    /// ([COB-003/1]) and does not move the state machine — unlike a refused *control mode*,
    /// which does.
    pub fn on_setpoint(
        &mut self,
        write: &SetpointWrite,
        accepted: bool,
        now: Duration,
    ) -> WriteOutcome {
        self.handle_timeout(now);

        if !matches!(self.state, CobState::PowerControl | CobState::PccControl) {
            return WriteOutcome::Rejected(NackReason::ModeNotEstablished);
        }
        if !write.watts.is_finite() {
            return WriteOutcome::Rejected(NackReason::NotFinite);
        }
        if !accepted {
            return WriteOutcome::Rejected(NackReason::CannotApply);
        }

        self.setpoint = Some(*write);
        // [COB-005/1]: the duration starts running on receipt, whether or not the setpoint
        // is activated.
        self.setpoint_expiry = write.duration.map(|d| now + d);
        if write.duration == Some(Duration::ZERO) {
            self.setpoint = Some(SetpointWrite {
                is_active: false,
                ..*write
            });
        }
        WriteOutcome::Accepted
    }

    /// Applies a write on the Failsafe Duration Minimum ([COB-044]).
    pub fn on_failsafe_duration(&mut self, duration: Duration, now: Duration) -> WriteOutcome {
        self.handle_timeout(now);
        if self.needs_recent_heartbeat() && !self.heartbeat_is_recent(now) {
            return WriteOutcome::Rejected(NackReason::NoRecentHeartbeat);
        }
        if !FAILSAFE_DURATION_RANGE.contains(&duration) {
            return WriteOutcome::Rejected(NackReason::DurationOutOfRange);
        }
        self.config.failsafe_duration = duration;
        WriteOutcome::Accepted
    }

    /// Applies a write on the failsafe setpoint ([COB-041] to [COB-043]).
    pub fn on_failsafe_setpoint(&mut self, watts: f64, now: Duration) -> WriteOutcome {
        self.handle_timeout(now);
        if self.needs_recent_heartbeat() && !self.heartbeat_is_recent(now) {
            return WriteOutcome::Rejected(NackReason::NoRecentHeartbeat);
        }
        if !watts.is_finite() {
            return WriteOutcome::Rejected(NackReason::NotFinite);
        }
        self.config.failsafe_watts = watts;
        WriteOutcome::Accepted
    }

    /// Advances the timers.
    pub fn handle_timeout(&mut self, now: Duration) {
        // [COB-913]: no heartbeat for 120 seconds, from any controlled state.
        if matches!(
            self.state,
            CobState::PowerControl | CobState::PccControl | CobState::AutoControl
        ) && now.saturating_sub(self.last_heartbeat.unwrap_or(self.entered_state_at))
            >= HEARTBEAT_TIMEOUT
        {
            // [COB-009/2]: every control-mode setpoint is deactivated on the way in.
            self.setpoint = None;
            self.setpoint_expiry = None;
            self.enter(CobState::Failsafe, now);
            return;
        }

        // [COB-006/1]: an expired duration deactivates the setpoint. The state does not
        // change — the manager is still in control, and the default applies.
        if let Some(expiry) = self.setpoint_expiry
            && now >= expiry
            && let Some(setpoint) = self.setpoint.as_mut()
        {
            setpoint.is_active = false;
        }

        match self.state {
            // [COB-905]: nothing heard in the settle window after a restart.
            CobState::Init if now.saturating_sub(self.entered_state_at) >= SETTLE_TIMEOUT => {
                self.enter(CobState::AutoUncontrolled, now);
            }
            CobState::Failsafe => {
                // [COB-915]: the Failsafe Duration Minimum ran out. [COB-916]: heartbeats
                // resumed but no control-mode write followed.
                let minimum_elapsed =
                    now.saturating_sub(self.entered_state_at) >= self.config.failsafe_duration;
                let heard_but_not_controlling = self
                    .awaiting_mode_since
                    .is_some_and(|since| now.saturating_sub(since) >= SETTLE_TIMEOUT);
                if minimum_elapsed || heard_but_not_controlling {
                    self.enter(CobState::AutoUncontrolled, now);
                }
            }
            _ => {}
        }
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called.
    ///
    /// # Invariant
    ///
    /// [`None`] is returned in exactly one state, [`CobState::AutoUncontrolled`], where
    /// nothing is pending. Everywhere else a deadline is due and the answer is [`Some`] —
    /// which matters, because a caller that waits on this value would otherwise never
    /// reach the failsafe of [COB-913].
    pub fn poll_timeout(&self) -> Option<Duration> {
        let mut next: Option<Duration> = None;
        let mut consider = |deadline: Duration| {
            next = Some(next.map_or(deadline, |current: Duration| current.min(deadline)));
        };

        match self.state {
            CobState::PowerControl | CobState::PccControl | CobState::AutoControl => {
                consider(self.last_heartbeat.unwrap_or(self.entered_state_at) + HEARTBEAT_TIMEOUT);
                if let Some(expiry) = self.setpoint_expiry
                    && self.setpoint.is_some_and(|s| s.is_active)
                {
                    consider(expiry);
                }
            }
            CobState::Init => consider(self.entered_state_at + SETTLE_TIMEOUT),
            CobState::Failsafe => {
                consider(self.entered_state_at + self.config.failsafe_duration);
                if let Some(since) = self.awaiting_mode_since {
                    consider(since + SETTLE_TIMEOUT);
                }
            }
            CobState::AutoUncontrolled => {}
        }
        next
    }

    /// True in the states where a control-mode write needs a fresh heartbeat behind it.
    fn needs_recent_heartbeat(&self) -> bool {
        matches!(
            self.state,
            CobState::Init | CobState::Failsafe | CobState::AutoUncontrolled
        )
    }

    fn heartbeat_is_recent(&self, now: Duration) -> bool {
        self.last_heartbeat
            .is_some_and(|hb| now.saturating_sub(hb) < WRITE_WINDOW)
    }

    fn enter(&mut self, state: CobState, now: Duration) {
        if self.state == state {
            return;
        }
        self.state = state;
        self.entered_state_at = now;
        self.awaiting_mode_since = None;
    }
}

impl crate::usecases::signals::Signals for BatteryControl {
    /// What a tester reads off an inverter.
    fn signals(&self, _: ()) -> crate::usecases::signals::SignalSet {
        use crate::usecases::signals::{Signal, SignalSet, SignalValue};
        use alloc::borrow::Cow;

        SignalSet::new()
            .with(Signal::new(
                "cob:state",
                SignalValue::Text(Cow::Borrowed(self.state.as_str())),
            ))
            .with(Signal::new(
                "cob:controlMode",
                match self.state.control_mode() {
                    Some(mode) => SignalValue::Text(Cow::Borrowed(mode.as_str())),
                    None => SignalValue::Absent,
                },
            ))
            .with(
                Signal::new(
                    "cob:setpoint",
                    SignalValue::number(self.setpoint.filter(|s| s.is_active).map(|s| s.watts)),
                )
                .in_unit("W"),
            )
            .with(Signal::new(
                "cob:isActive",
                SignalValue::Bool(self.is_setpoint_active()),
            ))
            .with(
                Signal::new(
                    "cob:effective",
                    SignalValue::number(self.effective().watts()),
                )
                .in_unit("W"),
            )
            .with(
                Signal::new(
                    "cob:failsafeSetpoint",
                    SignalValue::Number(self.config.failsafe_watts),
                )
                .in_unit("W"),
            )
            .with(Signal::new(
                "cob:failsafeDuration",
                SignalValue::Seconds(self.config.failsafe_duration.as_secs_f64()),
            ))
            .with(Signal::new(
                "cob:lastHeartbeat",
                SignalValue::seconds(self.last_heartbeat),
            ))
            .with(Signal::new(
                "cob:nextDeadline",
                SignalValue::seconds(self.poll_timeout()),
            ))
    }
}

// ---- what the inverter publishes ---------------------------------------------------

/// Builds the `Setpoint` feature scenarios 1 and 2 are served from.
///
/// Writes are deferred: whether a setpoint can be applied is the inverter's decision, and
/// that decision is the acknowledgement.
pub fn setpoint_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::Setpoint, Role::Server)
        .with_deferred_writes()
        .with_function(Function::SetpointDescriptionListData, Operations::read())
        .with_function(Function::SetpointConstraintsListData, Operations::read())
        .with_function(Function::SetpointListData, Operations::read_write())
}

/// Builds the `DeviceConfiguration` feature scenarios 3 and 4 are served from.
pub fn device_configuration_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceConfiguration, Role::Server)
        .with_deferred_writes()
        .with_function(
            Function::DeviceConfigurationKeyValueDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::DeviceConfigurationKeyValueListData,
            Operations::read_write(),
        )
}

/// Builds the `DeviceDiagnosis` feature that carries the heartbeat (scenario 5).
pub fn device_diagnosis_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceDiagnosis, Role::Server)
        .with_function(Function::DeviceDiagnosisHeartbeatData, Operations::read())
}

/// The setpoint description an inverter publishes for a control mode.
pub fn setpoint_description(config: &CobConfig, mode: ControlMode) -> CmdData {
    let scope = match mode {
        ControlMode::Pcc => ScopeType::PccPower,
        _ => config.power_scope(),
    };
    CmdData::SetpointDescriptionListData(SetpointDescriptionListData {
        setpoint_description_data: Some(vec![SetpointDescriptionData {
            setpoint_id: Some(SETPOINT_ID),
            setpoint_type: Some(SetpointType::ValueAbsolute),
            unit: Some(UnitOfMeasurement::W),
            scope_type: Some(scope),
            ..Default::default()
        }]),
    })
}

/// The current setpoint, as `setpointListData`.
pub fn setpoint_data(control: &BatteryControl) -> CmdData {
    CmdData::SetpointListData(SetpointListData {
        setpoint_data: Some(vec![SetpointData {
            setpoint_id: Some(SETPOINT_ID),
            is_setpoint_changeable: Some(true),
            is_setpoint_active: Some(control.is_setpoint_active()),
            value: Some(ScaledNumber::from_f64(
                control.setpoint().map(|s| s.watts).unwrap_or(0.0),
                0,
            )),
            ..Default::default()
        }]),
    })
}

/// Reads a `setpointListData` payload as a setpoint change.
///
/// **Give this the resolved state, not a partial update** — the same rule as everywhere
/// else in this crate: an absent `isSetpointActive` reads as *not active*, which is right
/// for a complete value and wrong for a fragment.
pub fn read_setpoint_write(data: &CmdData) -> Option<SetpointWrite> {
    let CmdData::SetpointListData(list) = data else {
        return None;
    };
    let entry = list
        .setpoint_data
        .as_ref()?
        .iter()
        .find(|e| e.setpoint_id == Some(SETPOINT_ID))?;

    let watts = match entry.value.as_ref() {
        Some(value) => value.to_f64()?,
        None => 0.0,
    };
    // §3.1.8.2, in the same words LPC uses: "Durations used within this Use Case SHALL be
    // presented as relative times. The same holds for the `endTime` Element used for the
    // validity duration ([COB-005/1])". A present `endTime` that is not a duration is a
    // broken write, and reading it as [`None`] would mean "no duration" — a setpoint meant
    // to expire applied until something else replaced it. Refusing produces a NACK.
    let duration = match entry.time_period.as_ref().and_then(|p| p.end_time.as_ref()) {
        Some(end_time) => Some(end_time.as_duration()?),
        None => None,
    };

    Some(SetpointWrite {
        is_active: entry.is_setpoint_active.unwrap_or(false),
        watts,
        duration,
    })
}

/// The configuration keys of scenarios 3 and 4.
pub fn configuration_descriptions(config: &CobConfig, mode: ControlMode) -> CmdData {
    CmdData::DeviceConfigurationKeyValueDescriptionListData(
        DeviceConfigurationKeyValueDescriptionListData {
            device_configuration_key_value_description_data: Some(vec![
                DeviceConfigurationKeyValueDescriptionData {
                    key_id: Some(CONTROL_MODE_KEY),
                    key_name: Some(CONTROL_MODE_KEY_NAME),
                    value_type: Some(DeviceConfigurationKeyValueType::String),
                    ..Default::default()
                },
                DeviceConfigurationKeyValueDescriptionData {
                    key_id: Some(DEFAULT_KEY),
                    key_name: Some(config.default_key(mode)),
                    value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
                    unit: Some(UnitOfMeasurement::W),
                    ..Default::default()
                },
                DeviceConfigurationKeyValueDescriptionData {
                    key_id: Some(FAILSAFE_KEY),
                    key_name: Some(config.failsafe_key(mode)),
                    value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
                    unit: Some(UnitOfMeasurement::W),
                    ..Default::default()
                },
                DeviceConfigurationKeyValueDescriptionData {
                    key_id: Some(FAILSAFE_DURATION_KEY),
                    key_name: Some(DeviceConfigurationKeyName::FailsafeDurationMinimum),
                    value_type: Some(DeviceConfigurationKeyValueType::Duration),
                    ..Default::default()
                },
            ]),
        },
    )
}

/// The current configuration values.
pub fn configuration_values(control: &BatteryControl) -> CmdData {
    let config = control.config();
    let mode = control.state().control_mode().unwrap_or(ControlMode::Auto);
    CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
        device_configuration_key_value_data: Some(vec![
            DeviceConfigurationKeyValueData {
                key_id: Some(CONTROL_MODE_KEY),
                value: Some(DeviceConfigurationKeyValueValue {
                    string: Some(mode.as_str().into()),
                    ..Default::default()
                }),
                is_value_changeable: Some(true),
            },
            DeviceConfigurationKeyValueData {
                key_id: Some(DEFAULT_KEY),
                value: Some(DeviceConfigurationKeyValueValue {
                    scaled_number: Some(ScaledNumber::from_f64(config.default_watts, 0)),
                    ..Default::default()
                }),
                is_value_changeable: Some(true),
            },
            DeviceConfigurationKeyValueData {
                key_id: Some(FAILSAFE_KEY),
                value: Some(DeviceConfigurationKeyValueValue {
                    scaled_number: Some(ScaledNumber::from_f64(config.failsafe_watts, 0)),
                    ..Default::default()
                }),
                is_value_changeable: Some(true),
            },
            DeviceConfigurationKeyValueData {
                key_id: Some(FAILSAFE_DURATION_KEY),
                value: Some(DeviceConfigurationKeyValueValue {
                    duration: Some(crate::model::format_iso8601_duration(
                        config.failsafe_duration,
                    )),
                    ..Default::default()
                }),
                is_value_changeable: Some(true),
            },
        ]),
    })
}

/// Reads the Active Control Mode out of a `deviceConfigurationKeyValueListData`.
///
/// `key` is the identifier **on the device the payload came from**: this inverter's own
/// [`CONTROL_MODE_KEY`] when reading back what it published, and the peer's — from
/// [`addressing::find_key_id`](crate::usecases::addressing::find_key_id) with
/// [`CONTROL_MODE_KEY_NAME`] — when an energy manager reads an inverter it did not build.
pub fn read_control_mode(data: &CmdData, key: DeviceConfigurationKeyId) -> Option<ControlMode> {
    let CmdData::DeviceConfigurationKeyValueListData(list) = data else {
        return None;
    };
    list.device_configuration_key_value_data
        .iter()
        .flatten()
        .find(|entry| entry.key_id == Some(key))
        .and_then(|entry| entry.value.as_ref())
        .and_then(|value| value.string.as_ref())
        .and_then(|value| ControlMode::read(value.as_str()))
}

/// A control-mode write an energy manager sends.
///
/// `key` is the **inverter's** `keyId` for the Active Control Mode, from its
/// `deviceConfigurationKeyValueDescriptionListData`. It is not [`CONTROL_MODE_KEY`]
/// unless the inverter happens to number its keys the way this crate numbers its own:
/// COB Table 20 spells it `<k1#(1..1)>`, and a `DeviceConfiguration` feature carries every
/// configuration key the inverter has. Writing a control mode into another key is a write
/// the inverter accepts and applies.
pub fn control_mode_payload(mode: ControlMode, key: DeviceConfigurationKeyId) -> CmdData {
    CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
        device_configuration_key_value_data: Some(vec![DeviceConfigurationKeyValueData {
            key_id: Some(key),
            value: Some(DeviceConfigurationKeyValueValue {
                string: Some(mode.as_str().to_string().into()),
                ..Default::default()
            }),
            ..Default::default()
        }]),
    })
}

// ---- descriptors -------------------------------------------------------------------

const INVERTER_ENTITIES: &[EntityType] = &[EntityType::Inverter];
const CEM_ENTITIES: &[EntityType] = &[EntityType::CEM];

const SERVER_SETPOINTS: &[FunctionUse] = &[
    FunctionUse::server(FeatureType::Setpoint, Function::SetpointDescriptionListData),
    FunctionUse::server(FeatureType::Setpoint, Function::SetpointConstraintsListData),
    FunctionUse::server_writeable(FeatureType::Setpoint, Function::SetpointListData),
];

const CLIENT_SETPOINTS: &[FunctionUse] = &[
    FunctionUse::client(FeatureType::Setpoint, Function::SetpointDescriptionListData),
    FunctionUse::client(FeatureType::Setpoint, Function::SetpointConstraintsListData),
    FunctionUse::client_writes(FeatureType::Setpoint, Function::SetpointListData),
];

// [COB-008]: the heartbeat is the one function here that arrives on a clock, so it is
// the one whose silence is a fact rather than a value that has not changed.
const SERVER_HEARTBEAT: &[FunctionUse] = &[FunctionUse::client(
    FeatureType::DeviceDiagnosis,
    Function::DeviceDiagnosisHeartbeatData,
)
.periodic(HEARTBEAT_PERIOD)];

const CLIENT_HEARTBEAT: &[FunctionUse] = &[FunctionUse::server(
    FeatureType::DeviceDiagnosis,
    Function::DeviceDiagnosisHeartbeatData,
)
.periodic(HEARTBEAT_PERIOD)];

const NAMES: [&str; 5] = [
    "Control mode \"Power\"",
    "Control mode \"PCC\"",
    "Configuration parameters",
    "Failsafe values",
    "Heartbeat",
];

/// The inverter: the actor that is controlled.
pub static INVERTER: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: INVERTER_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: INVERTER_ENTITIES,
    counterpart: CEM_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: SERVER_SETPOINTS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: SERVER_SETPOINTS,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Mandatory,
            functions: SERVER_CONFIGURATION_WRITEABLE,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Mandatory,
            functions: SERVER_CONFIGURATION_WRITEABLE,
        },
        Scenario {
            number: 5,
            name: NAMES[4],
            support: Support::Mandatory,
            functions: SERVER_HEARTBEAT,
        },
    ],
};

/// The energy manager: the actor that controls.
pub static CEM: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: CEM_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: CEM_ENTITIES,
    counterpart: INVERTER_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: CLIENT_SETPOINTS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: CLIENT_SETPOINTS,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Mandatory,
            functions: CLIENT_CONFIGURATION_WRITES,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Mandatory,
            functions: CLIENT_CONFIGURATION_WRITES,
        },
        Scenario {
            number: 5,
            name: NAMES[4],
            support: Support::Mandatory,
            functions: CLIENT_HEARTBEAT,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn hybrid() -> BatteryControl {
        BatteryControl::new(
            CobConfig::new(InverterKind::Hybrid, secs(2 * 3_600))
                .with_failsafe(0.0)
                .with_default(-500.0),
            Duration::ZERO,
        )
    }

    /// §3.1.8.2: a validity duration that is not a relative time makes the write unusable.
    ///
    /// Reading it as absent would be the dangerous half of the mistake — absent means "no
    /// expiry", so a setpoint meant to lapse would be held until something else replaced
    /// it. The schema's `AbsoluteOrRelativeTimeType` union permits the timestamp this
    /// refuses, which is exactly why a CEM sends one in good faith.
    #[test]
    fn cob_005_a_validity_duration_that_is_not_a_relative_time_is_refused() {
        let with_end_time = |end_time: &str| {
            CmdData::SetpointListData(SetpointListData {
                setpoint_data: Some(alloc::vec![SetpointData {
                    setpoint_id: Some(SETPOINT_ID),
                    is_setpoint_active: Some(true),
                    value: Some(ScaledNumber::from_f64(-2_000.0, 0)),
                    time_period: Some(crate::model::TimePeriod {
                        end_time: Some(end_time.into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
            })
        };

        for unreadable in ["2026-09-05T10:00:00Z", "", "P1M"] {
            assert_eq!(
                read_setpoint_write(&with_end_time(unreadable)),
                None,
                "{unreadable:?} is not a relative time"
            );
        }
        assert_eq!(
            read_setpoint_write(&with_end_time("PT2H"))
                .expect("a well-formed write")
                .duration,
            Some(secs(7_200)),
        );
    }

    /// Transition 0 and [COB-901]: a restart is driven by the failsafe setpoints, not by
    /// nothing and not by the last thing it was told.
    #[test]
    fn cob_901_a_restart_starts_on_the_failsafe_setpoint() {
        let inverter = hybrid();
        assert_eq!(inverter.state(), CobState::Init);
        assert_eq!(inverter.effective(), EffectiveControl::Failsafe(0.0));
        assert!(!inverter.is_setpoint_active());
    }

    /// Transitions 1, 2 and 3, and the rule underneath them: a heartbeat opens the window
    /// and the *control-mode write* establishes control.
    #[test]
    fn cob_904_the_control_mode_write_is_what_establishes_control() {
        let mut inverter = hybrid();

        // Heartbeats alone change nothing.
        for second in 1..=30 {
            inverter.on_heartbeat(secs(second));
        }
        assert_eq!(inverter.state(), CobState::Init);

        // A write with no recent heartbeat is not evaluated.
        let mut stale = hybrid();
        assert_eq!(
            stale.on_control_mode(ControlMode::Power, true, secs(5)),
            WriteOutcome::Rejected(NackReason::NoRecentHeartbeat)
        );
        assert_eq!(stale.state(), CobState::Init);

        // Transition 1.
        assert!(
            inverter
                .on_control_mode(ControlMode::Power, true, secs(31))
                .is_accepted()
        );
        assert_eq!(inverter.state(), CobState::PowerControl);

        // Transition 3: a mode the inverter refuses moves it to Auto Control, not back.
        let mut refuses = hybrid();
        refuses.on_heartbeat(secs(10));
        assert_eq!(
            refuses.on_control_mode(ControlMode::Pcc, false, secs(11)),
            WriteOutcome::Rejected(NackReason::CannotApply)
        );
        assert_eq!(refuses.state(), CobState::AutoControl);
    }

    /// [COB-917]: a deactivated setpoint hands the inverter to its *default*, which is a
    /// number. Not zero, and not the last thing the manager said.
    #[test]
    fn cob_917_a_deactivated_setpoint_falls_back_to_the_default() {
        let mut inverter = hybrid();
        inverter.on_heartbeat(secs(10));
        inverter.on_control_mode(ControlMode::Power, true, secs(11));
        assert_eq!(inverter.effective(), EffectiveControl::Default(-500.0));

        assert!(
            inverter
                .on_setpoint(&SetpointWrite::active(3_000.0), true, secs(12))
                .is_accepted()
        );
        assert_eq!(inverter.effective(), EffectiveControl::Setpoint(3_000.0));

        assert!(
            inverter
                .on_setpoint(&SetpointWrite::deactivated(), true, secs(13))
                .is_accepted()
        );
        assert_eq!(
            inverter.effective(),
            EffectiveControl::Default(-500.0),
            "the default, not nought and not the old setpoint"
        );
    }

    /// [COB-005/1] and [COB-006/1]: the duration runs from the write and deactivates the
    /// setpoint, leaving the state alone — the manager is still in control.
    #[test]
    fn cob_006_an_expired_duration_deactivates_without_losing_control() {
        let mut inverter = hybrid();
        inverter.on_heartbeat(secs(10));
        inverter.on_control_mode(ControlMode::Power, true, secs(11));
        inverter.on_setpoint(
            &SetpointWrite::active_for(3_000.0, secs(60)),
            true,
            secs(12),
        );
        assert_eq!(inverter.effective(), EffectiveControl::Setpoint(3_000.0));

        // Keep the heartbeat alive, or the failsafe would fire first.
        inverter.on_heartbeat(secs(70));
        inverter.handle_timeout(secs(72));
        assert_eq!(inverter.state(), CobState::PowerControl, "still controlled");
        assert_eq!(inverter.effective(), EffectiveControl::Default(-500.0));
        assert!(!inverter.is_setpoint_active());
    }

    /// Transitions 7, 10 and 13: two minutes of silence, from any controlled state.
    #[test]
    fn cob_913_silence_moves_every_controlled_state_to_failsafe() {
        for mode in [ControlMode::Power, ControlMode::Pcc, ControlMode::Auto] {
            let mut inverter = hybrid();
            inverter.on_heartbeat(secs(10));
            inverter.on_control_mode(mode, true, secs(11));

            inverter.handle_timeout(secs(10) + HEARTBEAT_TIMEOUT - secs(1));
            assert_ne!(inverter.state(), CobState::Failsafe, "not yet, {mode:?}");

            inverter.handle_timeout(secs(10) + HEARTBEAT_TIMEOUT);
            assert_eq!(inverter.state(), CobState::Failsafe, "{mode:?}");
            assert_eq!(inverter.effective(), EffectiveControl::Failsafe(0.0));
            assert!(
                inverter.setpoint().is_none(),
                "[COB-009/2]: every setpoint is deactivated on the way in"
            );
        }
    }

    /// Transitions 14 and 17, and the rule that a setpoint may not be written before the
    /// mode: an inverter that took one would apply a grid number to its battery.
    #[test]
    fn cob_914_the_mode_comes_before_the_setpoint() {
        let mut inverter = hybrid();
        inverter.on_heartbeat(secs(10));
        inverter.on_control_mode(ControlMode::Power, true, secs(11));
        inverter.handle_timeout(secs(10) + HEARTBEAT_TIMEOUT);
        assert_eq!(inverter.state(), CobState::Failsafe);

        // A setpoint on its own is refused, and the state does not move.
        let now = secs(10) + HEARTBEAT_TIMEOUT + secs(1);
        assert_eq!(
            inverter.on_setpoint(&SetpointWrite::active(1_000.0), true, now),
            WriteOutcome::Rejected(NackReason::ModeNotEstablished)
        );
        assert_eq!(inverter.state(), CobState::Failsafe);

        // Heartbeat, then the mode, then the setpoint.
        inverter.on_heartbeat(now);
        assert!(
            inverter
                .on_control_mode(ControlMode::Power, true, now + secs(1))
                .is_accepted()
        );
        assert_eq!(inverter.state(), CobState::PowerControl);
        assert!(
            inverter
                .on_setpoint(&SetpointWrite::active(1_000.0), true, now + secs(2))
                .is_accepted()
        );
    }

    /// [COB-915] and [COB-916]: two ways out of the failsafe state, both ending in
    /// `Auto Uncontrolled`.
    #[test]
    fn cob_915_and_916_the_failsafe_state_is_not_forever() {
        // The duration runs out.
        let mut inverter = hybrid();
        inverter.on_heartbeat(secs(10));
        inverter.on_control_mode(ControlMode::Power, true, secs(11));
        let entered = secs(10) + HEARTBEAT_TIMEOUT;
        inverter.handle_timeout(entered);
        inverter.handle_timeout(entered + secs(2 * 3_600) - secs(1));
        assert_eq!(inverter.state(), CobState::Failsafe);
        inverter.handle_timeout(entered + secs(2 * 3_600));
        assert_eq!(inverter.state(), CobState::AutoUncontrolled);

        // Or the manager beats but never writes a mode.
        let mut quiet = hybrid();
        quiet.on_heartbeat(secs(10));
        quiet.on_control_mode(ControlMode::Power, true, secs(11));
        quiet.handle_timeout(entered);
        quiet.on_heartbeat(entered + secs(1));
        quiet.handle_timeout(entered + secs(1) + SETTLE_TIMEOUT);
        assert_eq!(quiet.state(), CobState::AutoUncontrolled);
    }

    /// The invariant a caller's event loop rests on.
    #[test]
    fn the_deadline_is_none_only_when_nothing_is_pending() {
        let mut inverter = hybrid();
        let mut steps = 0;
        while let Some(deadline) = inverter.poll_timeout() {
            assert_ne!(inverter.state(), CobState::AutoUncontrolled);
            inverter.handle_timeout(deadline);
            steps += 1;
            assert!(
                steps <= 8,
                "the deadlines did not settle: {}",
                inverter.state()
            );
        }
        assert_eq!(inverter.state(), CobState::AutoUncontrolled);
    }

    /// [COB-001/1]: passive sign convention, and a caller that gets it wrong charges when
    /// it meant to discharge.
    #[test]
    fn cob_001_positive_is_consumption_and_negative_is_production() {
        let mut inverter = hybrid();
        inverter.on_heartbeat(secs(10));
        inverter.on_control_mode(ControlMode::Power, true, secs(11));

        inverter.on_setpoint(&SetpointWrite::active(3_000.0), true, secs(12));
        assert_eq!(inverter.effective().watts(), Some(3_000.0), "charging");

        inverter.on_setpoint(&SetpointWrite::active(-3_000.0), true, secs(13));
        assert_eq!(inverter.effective().watts(), Some(-3_000.0), "discharging");
    }

    /// The setpoint a `power` mode writes depends on the machine, and the two are not
    /// interchangeable ([COB-011] against [COB-012]).
    #[test]
    fn cob_011_and_012_the_machine_decides_which_setpoint_power_mode_uses() {
        let battery = CobConfig::new(InverterKind::Battery, secs(2 * 3_600));
        let hybrid = CobConfig::new(InverterKind::Hybrid, secs(2 * 3_600));

        assert_eq!(battery.power_scope(), ScopeType::BatteryAcPower);
        assert_eq!(hybrid.power_scope(), ScopeType::BatteryDcPower);
        assert_eq!(
            battery.default_key(ControlMode::Power).as_str(),
            "defaultAcPower"
        );
        assert_eq!(
            hybrid.default_key(ControlMode::Power).as_str(),
            "defaultDcPower"
        );
        // PCC is the same for both: it is a property of the grid connection, not of the
        // machine behind it.
        assert_eq!(
            battery.default_key(ControlMode::Pcc),
            hybrid.default_key(ControlMode::Pcc)
        );
        assert_eq!(
            battery.failsafe_key(ControlMode::Pcc).as_str(),
            "failsafePccPowerSetpoint"
        );
    }

    /// The setpoint round-trips through the wire form.
    #[test]
    fn a_setpoint_survives_the_round_trip() {
        let mut inverter = hybrid();
        inverter.on_heartbeat(secs(10));
        inverter.on_control_mode(ControlMode::Power, true, secs(11));
        inverter.on_setpoint(&SetpointWrite::active(-2_500.0), true, secs(12));

        let published = setpoint_data(&inverter);
        let read = read_setpoint_write(&published).expect("a setpoint");
        assert_eq!(read.watts, -2_500.0);
        assert!(read.is_active);

        // The inverter's own key, resolved from what it published rather than assumed.
        let key = crate::usecases::addressing::find_key_id(
            &configuration_descriptions(inverter.config(), ControlMode::Power),
            &CONTROL_MODE_KEY_NAME,
        )
        .expect("the inverter describes its control-mode key");
        let mode = control_mode_payload(ControlMode::Pcc, key);
        assert_eq!(read_control_mode(&mode, key), Some(ControlMode::Pcc));
    }

    /// [COB-044/3]: two to twenty-four hours, and nothing else.
    #[test]
    fn cob_044_the_failsafe_duration_has_a_range() {
        let mut inverter = hybrid();
        inverter.on_heartbeat(secs(10));
        inverter.on_control_mode(ControlMode::Power, true, secs(11));

        assert!(
            inverter
                .on_failsafe_duration(secs(4 * 3_600), secs(12))
                .is_accepted()
        );
        assert_eq!(inverter.config().failsafe_duration, secs(4 * 3_600));

        assert_eq!(
            inverter.on_failsafe_duration(secs(3_600), secs(13)),
            WriteOutcome::Rejected(NackReason::DurationOutOfRange)
        );
        assert_eq!(
            inverter.on_failsafe_duration(secs(25 * 3_600), secs(14)),
            WriteOutcome::Rejected(NackReason::DurationOutOfRange)
        );
        assert_eq!(inverter.config().failsafe_duration, secs(4 * 3_600));
    }

    #[test]
    fn the_descriptors_say_what_table_2_says() {
        assert_eq!(INVERTER.use_case_name().as_str(), NAME);
        assert_eq!(INVERTER.use_case_actor().as_str(), "Inverter");
        assert_eq!(CEM.use_case_actor().as_str(), "CEM");
        assert_eq!(
            INVERTER.required_scenarios().collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        assert_eq!(
            CEM.required_scenarios().collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );

        // The manager writes both the setpoint and the configuration, so it binds to both.
        let binding: Vec<_> = CEM.features_needing_binding().collect();
        assert_eq!(
            binding,
            [&FeatureType::Setpoint, &FeatureType::DeviceConfiguration]
        );
    }
}
