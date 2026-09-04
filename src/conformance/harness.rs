//! The device-level half of the conformance suite, runnable against a real box.
//!
//! [`CATALOGUE`](super::CATALOGUE) holds all 203 abstract test cases and
//! `tests/conformance.rs` answers the ones that are about the protocol. Fourteen are not:
//! a factory reset, a power cut, a start-up duration, what the appliance actually draws.
//! [`DEVICE_LEVEL`](super::DEVICE_LEVEL) says which, and why each belongs to the device.
//! This module is what a consumer drives to answer them **against its own running
//! binary**, before a laboratory does it for money.
//!
//! Seven procedures cover the fourteen — LPC and LPP state the same cases under different
//! prefixes, so each procedure answers a pair. Each carries the High-Level Test
//! Specification's own steps and expected results ([`Procedure::steps`]), so the thing a
//! person or a script has to do is written down next to the identifier the laboratory will
//! report.
//!
//! # What it does, and what it cannot
//!
//! It **judges**: given what the device declared in its parameter sheet and what was
//! actually observed, it says pass, fail or inconclusive against the specification's own
//! deadlines — 60 seconds for a heartbeat and the limit that follows it, 120 seconds for a
//! resend after a NACK, the manufacturer's own `StartUpDur` for coming back from a power
//! cut. It does **not** press the button: cutting the power and triggering the factory
//! reset are the consumer's, which is exactly why these fourteen are not answerable here.
//!
//! ```
//! use core::time::Duration;
//! use eebus::conformance::harness::{DeviceObservation, DeviceParameters, DeviceRun, Verdict};
//!
//! // What the device's parameter sheet declares.
//! let declared = DeviceParameters::new(Duration::from_secs(45))
//!     .failsafe(4_200.0, Duration::from_secs(2 * 3_600));
//! let mut run = DeviceRun::new(declared);
//!
//! // The consumer resets its own binary and reads the values back out of it.
//! run.observe(DeviceObservation::FactoryReset {
//!     limit_active: false,
//!     failsafe_watts: 4_200.0,
//!     failsafe_duration: Duration::from_secs(2 * 3_600),
//! });
//!
//! let report = run.report();
//! assert_eq!(report.verdict("ATC_LPC_COM_PT_CSInit_002"), Some(&Verdict::Passed));
//! assert_eq!(report.passed(), 2, "the LPC case and its LPP twin");
//! assert!(!report.is_complete(), "six procedures have not been run");
//! ```

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

use crate::usecases::descriptor::{Support, actors};

use super::{AbstractTestCase, Coverage, device, find};

/// The 60 seconds a heartbeat and the limit that follows it have to arrive in.
///
/// `[E-DT60]` throughout the LPC High-Level Test Specification, and note 2 of its Table 19
/// calls it "best practice" rather than a hard requirement — a laboratory measures it, so
/// this does too.
pub const HEARTBEAT_THEN_LIMIT: Duration = Duration::from_secs(60);

/// The 120 seconds a resend after a NACK is expected within (Table 19, note 1).
pub const RESEND_AFTER_NACK: Duration = Duration::from_secs(120);

/// How long the power stays off in a black-start test (Tables 17 and 31, step 2).
pub const BLACK_START_OUTAGE: Duration = Duration::from_secs(90);

/// One step of a test case: what to do, and what should happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The action, as the specification's "Execution" column words it.
    pub action: &'static str,
    /// What the specification's "Expected result" column says should follow.
    pub expected: &'static str,
}

const fn step(action: &'static str, expected: &'static str) -> Step {
    Step { action, expected }
}

/// One device-level test case, as a procedure somebody can run.
///
/// Each answers two abstract test cases — the LPC one and its LPP twin — because the two
/// specifications state the same case under different prefixes and a device implementing
/// both is measured on both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Procedure {
    /// Reset the device to factory defaults and read its parameters back
    /// (`ATC_*_COM_PT_CSInit_002`).
    FactoryReset,
    /// Write failsafe values, power-cycle the device, and read them back
    /// (`ATC_*_COM_PT_CSInit_003`).
    Persistence,
    /// Cut the power to the whole installation and let the Controllable System come back
    /// (`ATC_*_COM_PT_CSConnection_009`).
    ControllableSystemBlackStart,
    /// The same for the Energy Guard (`ATC_*_COM_PT_EGConnection_003`).
    EnergyGuardBlackStart,
    /// Restart the Energy Guard's own process and wait for its opening exchange
    /// (`ATC_*_COM_PT_EGConnection_001`).
    EnergyGuardReboot,
    /// Reject a limit the rebooted Energy Guard writes, and wait for it to try again
    /// (`ATC_*_COM_PT_EGMessages_002`).
    EnergyGuardResendAfterNack,
    /// Write a limit above what the appliance can draw and read back what was applied
    /// (`ATC_*_COM_PT_CSConnection_006`).
    ApplianceCeiling,
}

/// Every procedure, in the order a report lists them.
pub static PROCEDURES: &[Procedure] = &[
    Procedure::FactoryReset,
    Procedure::Persistence,
    Procedure::ControllableSystemBlackStart,
    Procedure::EnergyGuardBlackStart,
    Procedure::EnergyGuardReboot,
    Procedure::EnergyGuardResendAfterNack,
    Procedure::ApplianceCeiling,
];

impl Procedure {
    /// The part of the `ATC_…` identifier that follows the use case's abbreviation.
    const fn suffix(self) -> &'static str {
        match self {
            Self::FactoryReset => "COM_PT_CSInit_002",
            Self::Persistence => "COM_PT_CSInit_003",
            Self::ControllableSystemBlackStart => "COM_PT_CSConnection_009",
            Self::EnergyGuardBlackStart => "COM_PT_EGConnection_003",
            Self::EnergyGuardReboot => "COM_PT_EGConnection_001",
            Self::EnergyGuardResendAfterNack => "COM_PT_EGMessages_002",
            Self::ApplianceCeiling => "COM_PT_CSConnection_006",
        }
    }

    /// The two abstract test cases this procedure answers: the LPC one, then its LPP twin.
    pub fn cases(self) -> [&'static str; 2] {
        match self {
            Self::FactoryReset => ["ATC_LPC_COM_PT_CSInit_002", "ATC_LPP_COM_PT_CSInit_002"],
            Self::Persistence => ["ATC_LPC_COM_PT_CSInit_003", "ATC_LPP_COM_PT_CSInit_003"],
            Self::ControllableSystemBlackStart => [
                "ATC_LPC_COM_PT_CSConnection_009",
                "ATC_LPP_COM_PT_CSConnection_009",
            ],
            Self::EnergyGuardBlackStart => [
                "ATC_LPC_COM_PT_EGConnection_003",
                "ATC_LPP_COM_PT_EGConnection_003",
            ],
            Self::EnergyGuardReboot => [
                "ATC_LPC_COM_PT_EGConnection_001",
                "ATC_LPP_COM_PT_EGConnection_001",
            ],
            Self::EnergyGuardResendAfterNack => [
                "ATC_LPC_COM_PT_EGMessages_002",
                "ATC_LPP_COM_PT_EGMessages_002",
            ],
            Self::ApplianceCeiling => [
                "ATC_LPC_COM_PT_CSConnection_006",
                "ATC_LPP_COM_PT_CSConnection_006",
            ],
        }
    }

    /// The catalogue entries, for the description, the level and the requirements covered.
    pub fn catalogue(self) -> [&'static AbstractTestCase; 2] {
        let [lpc, lpp] = self.cases();
        [
            find(lpc).expect("every procedure names a catalogued test case"),
            find(lpp).expect("every procedure names a catalogued test case"),
        ]
    }

    /// Which actor is under test.
    pub fn actor(self) -> &'static str {
        match self {
            Self::EnergyGuardBlackStart
            | Self::EnergyGuardReboot
            | Self::EnergyGuardResendAfterNack => actors::ENERGY_GUARD,
            _ => actors::CONTROLLABLE_SYSTEM,
        }
    }

    /// Why the device owns this case rather than the library, as
    /// [`super::device`] words it.
    pub fn owner(self) -> &'static str {
        match self {
            Self::FactoryReset => device::FACTORY_RESET,
            Self::Persistence => device::PERSISTENCE,
            Self::ControllableSystemBlackStart | Self::EnergyGuardBlackStart => device::BLACK_START,
            Self::EnergyGuardReboot | Self::EnergyGuardResendAfterNack => device::REBOOT,
            Self::ApplianceCeiling => device::APPLIANCE,
        }
    }

    /// Whether a device has to pass it, and what raises an optional one to mandatory.
    ///
    /// Taken from the LPC entry; the LPP twin carries the same marking, which
    /// `the_two_specifications_mark_the_same_cases` checks.
    pub fn level(self) -> (Support, &'static [&'static str]) {
        let [lpc, _] = self.catalogue();
        (lpc.level, lpc.raised_when)
    }

    /// The specification's own steps, in order.
    ///
    /// Transcribed from the LPC High-Level Test Specification 1.0.2 tables 15, 17, 19, 28,
    /// 31, 33 and 34. The LPP twin's are the same with "consumption" read as "production".
    pub fn steps(self) -> &'static [Step] {
        match self {
            Self::FactoryReset => FACTORY_RESET_STEPS,
            Self::Persistence => PERSISTENCE_STEPS,
            Self::ControllableSystemBlackStart => CS_BLACK_START_STEPS,
            Self::EnergyGuardBlackStart => EG_BLACK_START_STEPS,
            Self::EnergyGuardReboot => EG_REBOOT_STEPS,
            Self::EnergyGuardResendAfterNack => EG_RESEND_STEPS,
            Self::ApplianceCeiling => APPLIANCE_STEPS,
        }
    }
}

const FACTORY_RESET_STEPS: &[Step] = &[
    step("Reset the CS.", "The CS reboots."),
    step(
        "Connect the CS to the EG.",
        "The CS is connected and able to exchange messages.",
    ),
    step("Send an EG heartbeat.", "The CS receives the heartbeat."),
    step(
        "Check whether the APCL of the CS is activated.",
        "The APCL of the CS is deactivated.",
    ),
    step(
        "Check the PFCAPL value of the CS.",
        "It equals the value in the parameter sheet.",
    ),
    step(
        "Check the PFSDM value of the CS.",
        "It equals the value in the parameter sheet.",
    ),
];

const PERSISTENCE_STEPS: &[Step] = &[
    step(
        "Connect the CS to the EG and send a heartbeat.",
        "The CS receives it.",
    ),
    step(
        "Send an APCL deactivation write, then an FCAPL write, then a \
                 Failsafe Duration Minimum write.",
        "The CS accepts each of them.",
    ),
    step(
        "Reboot the CS and wait until it can exchange messages again.",
        "The CS restarts in CF_CS_Init.",
    ),
    step(
        "Read the FCAPL and Failsafe Duration Minimum back.",
        "Both hold the values written before the reboot, not the defaults.",
    ),
];

const CS_BLACK_START_STEPS: &[Step] = &[
    step(
        "Switch off the power supply to both the tester and the DUT.",
        "Both devices turn off.",
    ),
    step("Wait 90 seconds.", ""),
    step("Switch the power supply on again.", "Both devices turn on."),
    step(
        "Wait for the CS to reach at least CF_CS_Init, up to the larger of \
                 StartUpDur_EG and StartUpDur_CS.",
        "The CS is reachable again inside that time.",
    ),
    step(
        "Send a heartbeat and an APCL deactivation write.",
        "The CS accepts it, or rejects it and accepts the next one.",
    ),
];

const EG_BLACK_START_STEPS: &[Step] = &[
    step(
        "Switch off the power supply to both the tester and the DUT.",
        "Both devices turn off.",
    ),
    step("Wait 90 seconds.", ""),
    step("Switch the power supply on again.", "Both devices turn on."),
    step(
        "Wait for the EG to re-establish the connection, up to the larger of \
                 StartUpDur_EG and StartUpDur_CS.",
        "The connection is up again inside that time.",
    ),
    step(
        "Wait for a heartbeat and the APCL write that follows it.",
        "Both arrive within 60 seconds.",
    ),
];

const EG_REBOOT_STEPS: &[Step] = &[
    step(
        "Reboot the EG and wait for the reboot to complete.",
        "Completed within StartUpDur_EG.",
    ),
    step(
        "Wait for the EG to send a heartbeat and a following APCL write.",
        "Both arrive within 60 seconds.",
    ),
];

const EG_RESEND_STEPS: &[Step] = &[
    step(
        "Reboot the EG and wait for the reboot to complete.",
        "Completed within StartUpDur_EG.",
    ),
    step(
        "Wait for the EG to send at least one heartbeat.",
        "The connection is maintained.",
    ),
    step(
        "Reject the next activated APCL write on the CS side.",
        "The EG receives the NACK.",
    ),
    step(
        "Wait for the EG to send a heartbeat and another APCL write.",
        "Both arrive within 60 seconds; the resend within 120.",
    ),
];

const APPLIANCE_STEPS: &[Step] = &[
    step(
        "Connect the CS to the EG and send a heartbeat.",
        "The CS receives it.",
    ),
    step(
        "Send an activated APCL write above the appliance's maximum \
                 consumption (data set APCL_05).",
        "The CS accepts it and moves to CF_CS_Limited_wo_dur.",
    ),
    step(
        "Read the APCL value back from the CS.",
        "It equals the value sent, rather than a clamped one.",
    ),
];

impl core::fmt::Display for Procedure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.suffix())
    }
}

/// What a device declares about itself, which is what its results are judged against.
///
/// §6.11.8 of the High-Level Test Specification: "a manufacturer needs to provide its
/// specific time in the parameter sheet up to which its device can establish
/// communication". A start-up duration is not a number the specification fixes — it is a
/// number the manufacturer commits to and is then held to.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceParameters {
    /// `StartUpDur`: from power on to being able to exchange messages.
    pub start_up: Duration,
    /// The tester's own, where a black start powers both down (Tables 17 and 31 wait for
    /// the larger of the two). Zero where the tester is instant or is a script.
    pub peer_start_up: Duration,
    /// The default Failsafe Consumption/Production Active Power Limit, in watts.
    pub failsafe_watts: Option<f64>,
    /// The default Failsafe Duration Minimum.
    pub failsafe_duration: Option<Duration>,
    /// What the device declares about itself in the parameter sheet, which raises some
    /// optional cases to mandatory — "the device is black-start capable" is the one that
    /// matters here.
    pub conditions: Vec<String>,
}

impl DeviceParameters {
    /// The declared start-up duration, and nothing else yet.
    pub fn new(start_up: Duration) -> Self {
        Self {
            start_up,
            peer_start_up: Duration::ZERO,
            failsafe_watts: None,
            failsafe_duration: None,
            conditions: Vec::new(),
        }
    }

    /// The failsafe defaults a factory reset must restore.
    #[must_use]
    pub fn failsafe(mut self, watts: f64, duration: Duration) -> Self {
        self.failsafe_watts = Some(watts);
        self.failsafe_duration = Some(duration);
        self
    }

    /// The tester's declared start-up duration, for the black-start cases.
    #[must_use]
    pub fn peer_start_up(mut self, start_up: Duration) -> Self {
        self.peer_start_up = start_up;
        self
    }

    /// Declares something about the device that raises an optional case to mandatory.
    #[must_use]
    pub fn declaring(mut self, condition: impl Into<String>) -> Self {
        self.conditions.push(condition.into());
        self
    }

    /// How long a black start may take: the larger of the two start-up durations.
    pub fn black_start_deadline(&self) -> Duration {
        self.start_up.max(self.peer_start_up)
    }
}

/// What actually happened when a procedure was run against the device.
///
/// One variant per [`Procedure`]. Every field is something a consumer's own harness can
/// read off its own binary — a value out of its configuration store, a timestamp, whether
/// a write was accepted — and nothing here needs a laboratory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeviceObservation {
    /// [`Procedure::FactoryReset`]: what the device held after being reset.
    FactoryReset {
        /// Whether the limit came back activated. It must not: Table 33 step 4.
        limit_active: bool,
        /// The failsafe limit the device came back with.
        failsafe_watts: f64,
        /// The failsafe duration minimum it came back with.
        failsafe_duration: Duration,
    },
    /// [`Procedure::Persistence`]: what was written, and what survived the power cut.
    Persistence {
        /// The failsafe limit that was written before the reboot.
        written_watts: f64,
        /// The failsafe duration that was written before the reboot.
        written_duration: Duration,
        /// The failsafe limit read back afterwards.
        stored_watts: f64,
        /// The failsafe duration read back afterwards.
        stored_duration: Duration,
    },
    /// [`Procedure::ControllableSystemBlackStart`] and
    /// [`Procedure::EnergyGuardBlackStart`]: whether the device came back on its own.
    BlackStart {
        /// Which actor was powered down.
        actor: BlackStartActor,
        /// How long from power on to being reachable again. [`None`] if it never was.
        reachable_after: Option<Duration>,
        /// Whether the limit exchange resumed afterwards. The Energy Guard's own
        /// heartbeat-and-limit is what says the use case is running again.
        exchange_resumed: bool,
    },
    /// [`Procedure::EnergyGuardReboot`]: the start-up, and the opening exchange after it.
    EnergyGuardReboot {
        /// How long the reboot took, from power on to being able to exchange messages.
        ready_after: Option<Duration>,
        /// How long after that the heartbeat and the limit that follows it arrived.
        heartbeat_then_limit_after: Option<Duration>,
    },
    /// [`Procedure::EnergyGuardResendAfterNack`]: whether a refused limit came again.
    EnergyGuardResendAfterNack {
        /// Whether the Controllable System actually refused the write. A test in which
        /// nothing was refused proves nothing about resending.
        limit_was_refused: bool,
        /// How long after the refusal the next limit arrived.
        resent_after: Option<Duration>,
        /// How long after that the heartbeat and the limit that follows it arrived.
        heartbeat_then_limit_after: Option<Duration>,
    },
    /// [`Procedure::ApplianceCeiling`]: a limit above what the appliance can draw.
    ApplianceCeiling {
        /// The limit that was written, in watts.
        written_watts: f64,
        /// Whether the device accepted it.
        accepted: bool,
        /// The limit the device holds afterwards. [`None`] if it could not be read.
        applied_watts: Option<f64>,
    },
}

impl DeviceObservation {
    /// The procedure this observation answers.
    pub fn procedure(&self) -> Procedure {
        match self {
            Self::FactoryReset { .. } => Procedure::FactoryReset,
            Self::Persistence { .. } => Procedure::Persistence,
            Self::BlackStart {
                actor: BlackStartActor::ControllableSystem,
                ..
            } => Procedure::ControllableSystemBlackStart,
            Self::BlackStart {
                actor: BlackStartActor::EnergyGuard,
                ..
            } => Procedure::EnergyGuardBlackStart,
            Self::EnergyGuardReboot { .. } => Procedure::EnergyGuardReboot,
            Self::EnergyGuardResendAfterNack { .. } => Procedure::EnergyGuardResendAfterNack,
            Self::ApplianceCeiling { .. } => Procedure::ApplianceCeiling,
        }
    }
}

/// Which side a black start was run against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlackStartActor {
    /// The Controllable System (`ATC_*_COM_PT_CSConnection_009`).
    ControllableSystem,
    /// The Energy Guard (`ATC_*_COM_PT_EGConnection_003`).
    EnergyGuard,
}

/// What a run concluded about one procedure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The device did what the specification asks.
    Passed,
    /// It did not, and this is what went wrong.
    Failed(String),
    /// The observation cannot decide the case, and this is why.
    ///
    /// Distinct from a failure on purpose: a persistence test that writes the value the
    /// device already held proves nothing, and counting that as either a pass or a failure
    /// would be a lie in one direction or the other.
    Inconclusive(String),
    /// The procedure was deliberately not run, and this is why.
    ///
    /// A device that is not black-start capable is entitled to skip the two black-start
    /// cases — they are `r`, raised to `m` only for a device that declares it — and the
    /// reason belongs in the report beside the identifier.
    Skipped(String),
}

impl Verdict {
    /// Whether this counts as covering the abstract test case.
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Passed)
    }
}

impl core::fmt::Display for Verdict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Passed => f.write_str("pass"),
            Self::Failed(why) => write!(f, "FAIL — {why}"),
            Self::Inconclusive(why) => write!(f, "inconclusive — {why}"),
            Self::Skipped(why) => write!(f, "skipped — {why}"),
        }
    }
}

/// One pass over the seven device-level procedures.
///
/// Built with the device's declared parameters, fed observations as its harness makes
/// them, and turned into a [`DeviceReport`] at the end. Observing the same procedure twice
/// replaces the earlier verdict, which is what re-running a step after a fix should do.
#[derive(Clone, Debug)]
pub struct DeviceRun {
    declared: DeviceParameters,
    results: Vec<(Procedure, Verdict)>,
}

impl DeviceRun {
    /// A run against a device that declares these parameters.
    pub fn new(declared: DeviceParameters) -> Self {
        Self {
            declared,
            results: Vec::new(),
        }
    }

    /// What the device declared.
    pub fn declared(&self) -> &DeviceParameters {
        &self.declared
    }

    /// Judges one observation and records the verdict, returning it.
    pub fn observe(&mut self, observation: DeviceObservation) -> &Verdict {
        let procedure = observation.procedure();
        let verdict = self.judge(&observation);
        self.record(procedure, verdict)
    }

    /// Records a procedure as deliberately not run.
    pub fn skip(&mut self, procedure: Procedure, why: impl Into<String>) -> &Verdict {
        self.record(procedure, Verdict::Skipped(why.into()))
    }

    /// Records a verdict a consumer reached some other way.
    ///
    /// For a procedure whose evidence does not fit an observation — a device whose
    /// factory reset is a physical switch and whose result was read off a screen.
    pub fn record(&mut self, procedure: Procedure, verdict: Verdict) -> &Verdict {
        match self.results.iter().position(|(held, _)| *held == procedure) {
            Some(index) => self.results[index].1 = verdict,
            None => self.results.push((procedure, verdict)),
        }
        &self
            .results
            .iter()
            .find(|(held, _)| *held == procedure)
            .expect("just recorded")
            .1
    }

    /// The verdict for one procedure, if it has been run.
    pub fn verdict(&self, procedure: Procedure) -> Option<&Verdict> {
        self.results
            .iter()
            .find(|(held, _)| *held == procedure)
            .map(|(_, verdict)| verdict)
    }

    /// Everything concluded so far, in the order [`PROCEDURES`] lists it.
    pub fn report(&self) -> DeviceReport {
        let rows = PROCEDURES
            .iter()
            .map(|procedure| {
                let verdict = self.verdict(*procedure).cloned().unwrap_or_else(|| {
                    Verdict::Inconclusive("the procedure has not been run".to_string())
                });
                (*procedure, verdict)
            })
            .collect();
        DeviceReport {
            rows,
            conditions: self.declared.conditions.clone(),
        }
    }

    fn judge(&self, observation: &DeviceObservation) -> Verdict {
        match observation {
            DeviceObservation::FactoryReset {
                limit_active,
                failsafe_watts,
                failsafe_duration,
            } => {
                if *limit_active {
                    return Verdict::Failed(
                        "the limit came back activated; Table 33 step 4 requires it deactivated"
                            .to_string(),
                    );
                }
                let (Some(watts), Some(duration)) = (
                    self.declared.failsafe_watts,
                    self.declared.failsafe_duration,
                ) else {
                    return Verdict::Inconclusive(
                        "the parameter sheet's PFCAPL and PFSDM were not declared, so there \
                         is nothing to compare the defaults against"
                            .to_string(),
                    );
                };
                if !close(*failsafe_watts, watts) {
                    return Verdict::Failed(alloc::format!(
                        "the failsafe limit came back as {failsafe_watts} W, and the \
                         parameter sheet declares {watts} W"
                    ));
                }
                if !about(*failsafe_duration, duration) {
                    return Verdict::Failed(alloc::format!(
                        "the failsafe duration came back as {failsafe_duration:?}, and the \
                         parameter sheet declares {duration:?}"
                    ));
                }
                Verdict::Passed
            }
            DeviceObservation::Persistence {
                written_watts,
                written_duration,
                stored_watts,
                stored_duration,
            } => {
                // Writing back what the device already held would pass without testing
                // anything, which is why Table 34 picks data sets away from the defaults.
                let same_as_default = self
                    .declared
                    .failsafe_watts
                    .is_some_and(|watts| close(*written_watts, watts))
                    && self
                        .declared
                        .failsafe_duration
                        .is_some_and(|duration| about(*written_duration, duration));
                if same_as_default {
                    return Verdict::Inconclusive(
                        "the values written are the device's own defaults, so reading them \
                         back after a reboot says nothing about storage"
                            .to_string(),
                    );
                }
                if !close(*stored_watts, *written_watts) {
                    return Verdict::Failed(alloc::format!(
                        "{written_watts} W was written and {stored_watts} W survived the reboot"
                    ));
                }
                if !about(*stored_duration, *written_duration) {
                    return Verdict::Failed(alloc::format!(
                        "{written_duration:?} was written and {stored_duration:?} survived \
                         the reboot"
                    ));
                }
                Verdict::Passed
            }
            DeviceObservation::BlackStart {
                reachable_after,
                exchange_resumed,
                ..
            } => {
                let deadline = self.declared.black_start_deadline();
                let Some(after) = reachable_after else {
                    return Verdict::Failed(
                        "the device never came back on its own after the power was restored"
                            .to_string(),
                    );
                };
                if *after > deadline {
                    return Verdict::Failed(alloc::format!(
                        "it came back after {after:?}, and the declared start-up duration \
                         is {deadline:?}"
                    ));
                }
                if !*exchange_resumed {
                    return Verdict::Failed(
                        "it came back and the limit exchange did not resume".to_string(),
                    );
                }
                Verdict::Passed
            }
            DeviceObservation::EnergyGuardReboot {
                ready_after,
                heartbeat_then_limit_after,
            } => {
                if let Some(failure) = self.judge_start_up(*ready_after) {
                    return failure;
                }
                self.judge_opening_exchange(*heartbeat_then_limit_after)
            }
            DeviceObservation::EnergyGuardResendAfterNack {
                limit_was_refused,
                resent_after,
                heartbeat_then_limit_after,
            } => {
                if !*limit_was_refused {
                    return Verdict::Inconclusive(
                        "no limit was refused, so nothing was resent and the case is \
                         untested"
                            .to_string(),
                    );
                }
                let Some(resent) = resent_after else {
                    return Verdict::Failed(
                        "the refused limit was never sent again; §2.5 has a refusal \
                         retried rather than the peer dropped"
                            .to_string(),
                    );
                };
                if *resent > RESEND_AFTER_NACK {
                    return Verdict::Failed(alloc::format!(
                        "the resend came after {resent:?}, and {RESEND_AFTER_NACK:?} is \
                         what Table 19 calls best practice"
                    ));
                }
                self.judge_opening_exchange(*heartbeat_then_limit_after)
            }
            DeviceObservation::ApplianceCeiling {
                written_watts,
                accepted,
                applied_watts,
            } => {
                if !*accepted {
                    return Verdict::Failed(alloc::format!(
                        "the device refused {written_watts} W; a limit above what the \
                         appliance can draw is still a valid limit and is to be accepted"
                    ));
                }
                let Some(applied) = applied_watts else {
                    return Verdict::Inconclusive(
                        "the limit the device holds could not be read back".to_string(),
                    );
                };
                if !close(*applied, *written_watts) {
                    return Verdict::Failed(alloc::format!(
                        "{written_watts} W was written and the device holds {applied} W; \
                         Table 28 step 4 wants the value sent, not a clamped one"
                    ));
                }
                Verdict::Passed
            }
        }
    }

    fn judge_start_up(&self, ready_after: Option<Duration>) -> Option<Verdict> {
        match ready_after {
            None => Some(Verdict::Failed(
                "the device never finished starting up".to_string(),
            )),
            Some(after) if after > self.declared.start_up => Some(Verdict::Failed(alloc::format!(
                "start-up took {after:?}, and the parameter sheet declares {:?}",
                self.declared.start_up
            ))),
            Some(_) => None,
        }
    }

    fn judge_opening_exchange(&self, after: Option<Duration>) -> Verdict {
        match after {
            None => Verdict::Failed(
                "no heartbeat and limit followed; §2.11 has the guard open with one as \
                 soon as the bindings settle"
                    .to_string(),
            ),
            Some(after) if after > HEARTBEAT_THEN_LIMIT => Verdict::Failed(alloc::format!(
                "the heartbeat and the limit that follows it took {after:?}, and \
                 {HEARTBEAT_THEN_LIMIT:?} is the deadline the test specification measures"
            )),
            Some(_) => Verdict::Passed,
        }
    }
}

/// What a [`DeviceRun`] concluded, one row per procedure.
///
/// [`Display`](core::fmt::Display) prints it as a table, which is what belongs in a
/// commissioning log next to the protocol-level coverage number.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceReport {
    rows: Vec<(Procedure, Verdict)>,
    conditions: Vec<String>,
}

impl DeviceReport {
    /// Every procedure and what it concluded.
    pub fn rows(&self) -> impl Iterator<Item = (Procedure, &Verdict)> {
        self.rows
            .iter()
            .map(|(procedure, verdict)| (*procedure, verdict))
    }

    /// The verdict for one `ATC_…` identifier, LPC or LPP.
    pub fn verdict(&self, case: &str) -> Option<&Verdict> {
        self.rows
            .iter()
            .find(|(procedure, _)| procedure.cases().contains(&case))
            .map(|(_, verdict)| verdict)
    }

    /// How many abstract test cases passed — two per procedure that did.
    pub fn passed(&self) -> usize {
        self.rows
            .iter()
            .filter(|(_, verdict)| verdict.is_pass())
            .count()
            * 2
    }

    /// The procedures that failed.
    pub fn failures(&self) -> impl Iterator<Item = (Procedure, &Verdict)> {
        self.rows
            .iter()
            .filter(|(_, verdict)| matches!(verdict, Verdict::Failed(_)))
            .map(|(procedure, verdict)| (*procedure, verdict))
    }

    /// Whether every procedure this device has to answer has been answered.
    ///
    /// A procedure the device is entitled to skip does not count against it: the two
    /// black-start cases are `r`, and mandatory only for a device that declares itself
    /// black-start capable. Everything else has to pass.
    pub fn is_complete(&self) -> bool {
        self.rows.iter().all(|(procedure, verdict)| {
            let (level, raised) = procedure.level();
            let required = level == Support::Mandatory
                || raised
                    .iter()
                    .any(|raise| self.conditions.iter().any(|held| held == raise));
            match verdict {
                Verdict::Passed => true,
                Verdict::Skipped(_) => !required,
                _ => false,
            }
        })
    }

    /// The coverage this run claims over the fourteen device-level test cases.
    ///
    /// Merge it with the protocol-level number to get the whole picture; a laboratory
    /// counts them together, so a report that leaves these out is measuring 189 of 203.
    pub fn coverage(&self) -> Coverage {
        let scope: Vec<&'static AbstractTestCase> =
            super::device_level().map(|(case, _)| case).collect();
        let claimed: Vec<&str> = self
            .rows
            .iter()
            .filter(|(_, verdict)| verdict.is_pass())
            .flat_map(|(procedure, _)| procedure.cases())
            .collect();
        Coverage::of(scope, &claimed)
    }
}

impl core::fmt::Display for DeviceReport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(
            f,
            "device-level conformance ({} of 14 passed)",
            self.passed()
        )?;
        for (procedure, verdict) in &self.rows {
            let [lpc, lpp] = procedure.cases();
            writeln!(f, "  {lpc}\n  {lpp}\n    {verdict}")?;
        }
        Ok(())
    }
}

/// Two power values that are the same value.
///
/// Half a watt: the wire carries a `ScaledNumber`, so a value that made the round trip
/// comes back rounded to the scale the device chose rather than bit-identical.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 0.5
}

/// Two durations that are the same duration, to the second ISO 8601 can express.
fn about(a: Duration, b: Duration) -> bool {
    a.as_secs() == b.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared() -> DeviceParameters {
        DeviceParameters::new(Duration::from_secs(45)).failsafe(4_200.0, Duration::from_secs(7_200))
    }

    /// Every procedure names two catalogued test cases, and between them they are the
    /// fourteen `DEVICE_LEVEL` lists — no more, no fewer, none twice.
    #[test]
    fn the_procedures_cover_exactly_the_device_level_cases() {
        let mut named: Vec<&str> = PROCEDURES
            .iter()
            .flat_map(|procedure| procedure.cases())
            .collect();
        named.sort_unstable();
        let mut listed: Vec<&str> = super::super::DEVICE_LEVEL
            .iter()
            .map(|(id, _)| *id)
            .collect();
        listed.sort_unstable();
        assert_eq!(named, listed);
        assert_eq!(named.len(), 14);

        for procedure in PROCEDURES {
            let [lpc, lpp] = procedure.catalogue();
            assert_eq!(lpc.dut, procedure.actor());
            assert_eq!(lpp.dut, procedure.actor());
            assert!(!procedure.steps().is_empty(), "{procedure} has no steps");
        }
    }

    /// The two specifications mark the same case the same way, which is what lets one
    /// procedure answer both.
    #[test]
    fn the_two_specifications_mark_the_same_cases() {
        for procedure in PROCEDURES {
            let [lpc, lpp] = procedure.catalogue();
            assert_eq!(lpc.level, lpp.level, "{procedure}");
            assert_eq!(lpc.raised_when, lpp.raised_when, "{procedure}");
        }
    }

    #[test]
    fn a_factory_reset_that_restores_the_declared_defaults_passes() {
        let mut run = DeviceRun::new(declared());
        assert_eq!(
            run.observe(DeviceObservation::FactoryReset {
                limit_active: false,
                failsafe_watts: 4_200.0,
                failsafe_duration: Duration::from_secs(7_200),
            }),
            &Verdict::Passed
        );
        assert_eq!(run.report().passed(), 2, "the LPC case and its LPP twin");
    }

    #[test]
    fn a_factory_reset_that_leaves_the_limit_active_fails() {
        let mut run = DeviceRun::new(declared());
        let verdict = run
            .observe(DeviceObservation::FactoryReset {
                limit_active: true,
                failsafe_watts: 4_200.0,
                failsafe_duration: Duration::from_secs(7_200),
            })
            .clone();
        assert!(matches!(verdict, Verdict::Failed(_)), "{verdict}");
        assert_eq!(run.report().passed(), 0);
    }

    /// Writing back the default and reading it again proves nothing, and says so.
    #[test]
    fn a_persistence_test_that_writes_the_default_is_inconclusive() {
        let mut run = DeviceRun::new(declared());
        let verdict = run
            .observe(DeviceObservation::Persistence {
                written_watts: 4_200.0,
                written_duration: Duration::from_secs(7_200),
                stored_watts: 4_200.0,
                stored_duration: Duration::from_secs(7_200),
            })
            .clone();
        assert!(matches!(verdict, Verdict::Inconclusive(_)), "{verdict}");

        // A value away from the default does test something.
        assert_eq!(
            run.observe(DeviceObservation::Persistence {
                written_watts: 3_000.0,
                written_duration: Duration::from_secs(10_800),
                stored_watts: 3_000.0,
                stored_duration: Duration::from_secs(10_800),
            }),
            &Verdict::Passed
        );
    }

    #[test]
    fn a_device_that_comes_back_too_slowly_fails_its_own_declared_time() {
        let declared = DeviceParameters::new(Duration::from_secs(45))
            .peer_start_up(Duration::from_secs(60))
            .failsafe(4_200.0, Duration::from_secs(7_200));
        let mut run = DeviceRun::new(declared);

        // Inside the larger of the two declared durations.
        assert_eq!(
            run.observe(DeviceObservation::BlackStart {
                actor: BlackStartActor::ControllableSystem,
                reachable_after: Some(Duration::from_secs(55)),
                exchange_resumed: true,
            }),
            &Verdict::Passed
        );

        let verdict = run
            .observe(DeviceObservation::BlackStart {
                actor: BlackStartActor::ControllableSystem,
                reachable_after: Some(Duration::from_secs(75)),
                exchange_resumed: true,
            })
            .clone();
        assert!(matches!(verdict, Verdict::Failed(_)), "{verdict}");
    }

    #[test]
    fn the_opening_exchange_after_a_reboot_is_measured_against_sixty_seconds() {
        let mut run = DeviceRun::new(declared());
        assert_eq!(
            run.observe(DeviceObservation::EnergyGuardReboot {
                ready_after: Some(Duration::from_secs(20)),
                heartbeat_then_limit_after: Some(Duration::from_secs(59)),
            }),
            &Verdict::Passed
        );
        let verdict = run
            .observe(DeviceObservation::EnergyGuardReboot {
                ready_after: Some(Duration::from_secs(20)),
                heartbeat_then_limit_after: Some(Duration::from_secs(61)),
            })
            .clone();
        assert!(matches!(verdict, Verdict::Failed(_)), "{verdict}");
    }

    /// A resend test in which nothing was refused is untested, not passed.
    #[test]
    fn a_resend_test_with_no_refusal_is_inconclusive() {
        let mut run = DeviceRun::new(declared());
        let verdict = run
            .observe(DeviceObservation::EnergyGuardResendAfterNack {
                limit_was_refused: false,
                resent_after: Some(Duration::from_secs(30)),
                heartbeat_then_limit_after: Some(Duration::from_secs(30)),
            })
            .clone();
        assert!(matches!(verdict, Verdict::Inconclusive(_)), "{verdict}");
    }

    /// [LPC-TS-035/4]: a limit above the appliance's maximum is accepted as sent.
    #[test]
    fn a_limit_above_the_appliances_maximum_is_applied_as_sent() {
        let mut run = DeviceRun::new(declared());
        assert_eq!(
            run.observe(DeviceObservation::ApplianceCeiling {
                written_watts: 11_550.0,
                accepted: true,
                applied_watts: Some(11_550.0),
            }),
            &Verdict::Passed
        );

        let verdict = run
            .observe(DeviceObservation::ApplianceCeiling {
                written_watts: 11_550.0,
                accepted: true,
                applied_watts: Some(11_000.0),
            })
            .clone();
        assert!(
            matches!(verdict, Verdict::Failed(_)),
            "clamping it is the failure this case exists to find: {verdict}"
        );
    }

    /// A device that is not black-start capable may skip the two black-start cases; one
    /// that declares it may not.
    #[test]
    fn a_skip_counts_only_where_the_case_is_not_required() {
        let mut run = DeviceRun::new(declared());
        for procedure in PROCEDURES {
            run.skip(*procedure, "not run in this pass");
        }
        let report = run.report();
        assert!(!report.is_complete(), "the mandatory cases were skipped");

        // Everything mandatory passes, the recommended black starts are skipped.
        let mut run = DeviceRun::new(declared());
        run.observe(DeviceObservation::FactoryReset {
            limit_active: false,
            failsafe_watts: 4_200.0,
            failsafe_duration: Duration::from_secs(7_200),
        });
        run.observe(DeviceObservation::Persistence {
            written_watts: 3_000.0,
            written_duration: Duration::from_secs(10_800),
            stored_watts: 3_000.0,
            stored_duration: Duration::from_secs(10_800),
        });
        run.observe(DeviceObservation::EnergyGuardReboot {
            ready_after: Some(Duration::from_secs(20)),
            heartbeat_then_limit_after: Some(Duration::from_secs(30)),
        });
        run.observe(DeviceObservation::EnergyGuardResendAfterNack {
            limit_was_refused: true,
            resent_after: Some(Duration::from_secs(90)),
            heartbeat_then_limit_after: Some(Duration::from_secs(30)),
        });
        run.observe(DeviceObservation::ApplianceCeiling {
            written_watts: 11_550.0,
            accepted: true,
            applied_watts: Some(11_550.0),
        });
        for procedure in [
            Procedure::ControllableSystemBlackStart,
            Procedure::EnergyGuardBlackStart,
        ] {
            run.skip(procedure, "the device has no supply to lose");
        }
        assert!(run.report().is_complete(), "{}", run.report());
        assert_eq!(run.report().passed(), 10);
        assert_eq!(run.report().coverage().covered(), 10);
        assert_eq!(run.report().coverage().missing(), 4);
    }

    /// A device that declares itself black-start capable cannot skip the black starts.
    #[test]
    fn declaring_black_start_capability_makes_those_cases_required() {
        let declared = declared().declaring("the device is black-start capable");
        let mut run = DeviceRun::new(declared);
        for procedure in PROCEDURES {
            run.skip(*procedure, "not run");
        }
        assert!(!run.report().is_complete());

        let raised: Vec<Procedure> = PROCEDURES
            .iter()
            .copied()
            .filter(|p| !p.level().1.is_empty())
            .collect();
        assert_eq!(
            raised,
            [
                Procedure::ControllableSystemBlackStart,
                Procedure::EnergyGuardBlackStart
            ],
            "the black starts are the two the parameter sheet can raise"
        );
    }

    /// The report prints something a person can read.
    #[test]
    fn the_report_is_printable() {
        let mut run = DeviceRun::new(declared());
        run.observe(DeviceObservation::FactoryReset {
            limit_active: false,
            failsafe_watts: 4_200.0,
            failsafe_duration: Duration::from_secs(7_200),
        });
        let printed = alloc::format!("{}", run.report());
        assert!(printed.contains("ATC_LPC_COM_PT_CSInit_002"));
        assert!(printed.contains("ATC_LPP_COM_PT_CSInit_002"));
        assert!(printed.contains("2 of 14 passed"));
    }
}
