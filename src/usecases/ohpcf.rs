//! Optimization of Self-Consumption by Heat Pump Compressor Flexibility (OHPCF).
//!
//! The one use case in this crate that can ask a **heat pump** to consume more.
//!
//! Everything on the grid side asks an appliance to do less — [`limitation`] a ceiling,
//! [`emobility::opev`] a current not to exceed, [`cob`] a battery between limits — and a
//! ceiling an appliance is already under changes nothing. A controller that has worked out
//! the building will be cheaper if the compressor runs *now*, while the roof is exporting,
//! cannot say so with a limit.
//!
//! [`hvac::cdt`] is the other lever, and not the same thing: CDT raises a hot water
//! **setpoint** and leaves the circuit's controller to decide what to do about it, whereas
//! this **starts a process** — the compressor's optional power consumption — at a time the
//! CEM names, and stops, pauses or resumes it afterwards. A tank already at temperature
//! will not run for a higher setpoint; it will run for this.
//!
//! Two scenarios, both mandatory for both actors (Table 1):
//!
//! 1. **Monitor the compressor's power consumption flexibility** — the compressor
//!    announces that it *could* run, how much it would draw, and how the CEM may interrupt
//!    it; then reports the state of the process once one is scheduled.
//! 2. **Control it** — the CEM schedules the announced process ([OHPCF-021]), and stops,
//!    pauses or resumes a scheduled one ([OHPCF-022]).
//!
//! # One function, four phases
//!
//! It is all `smartEnergyManagementPsData` on one `SmartEnergyManagementPs` server
//! (Table 10), and what the payload *means* is which phase the compressor is in:
//!
//! | | |
//! |---|---|
//! | **A** — an offer | `state: inactive`, no `schedule`. "I could run; I will not start on my own" [OHPCF-011/3] |
//! | **B** — a process | `state: scheduled`/`running`/`paused`, with a `schedule.startTime` |
//! | **C** — it ended | `state: completed` or `invalid` [OHPCF-006] |
//! | **D** — nothing | no `alternatives` at all [OHPCF-003] |
//!
//! [`Flexibility`] is the compressor's side of that and [`CompressorOffer`] the CEM's.
//!
//! §2.3 puts the electrical backup heater **out of scope**: at a COP of 1 it is never the
//! right thing to run on low-cost energy, because the compressor makes the same heat from
//! grid energy more cheaply.
//!
//! # The two durations
//!
//! [OHPCF-008] and [OHPCF-009]: the minimum time the compressor has to run once started,
//! and the minimum time it needs off before it can start again. A plan that ignores them
//! is one the compressor refuses, or follows into short-cycling. [`Durations`] carries
//! both.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::ohpcf::{self, CompressorOffer, Durations, Flexibility, Interrupt};
//!
//! // The compressor could run at 2.4 kW; the CEM may stop it, and it needs 20 minutes
//! // once it starts.
//! let offer = Flexibility::offered(2_400.0)
//!     .interruptible(Interrupt::Stoppable)
//!     .lasting(Durations::new().at_least(Duration::from_secs(20 * 60)));
//!
//! // What the CEM makes of it.
//! let read = CompressorOffer::read(&offer.data()).expect("an offer");
//! assert!(read.is_available(), "inactive and unscheduled: it is on offer");
//! assert_eq!(read.power_watts, Some(2_400.0));
//! assert!(read.is_stoppable);
//!
//! // The roof is exporting. Run it now — `startTime` here is a span, not an instant:
//! // this feature restricts the element to `xs:duration`.
//! let write = ohpcf::activate(read.sequence, Duration::ZERO);
//! # let _ = write;
//! ```
//!
//! # Subscribe to be told, bind to write
//!
//! Both scenarios are served from the one `SmartEnergyManagementPs` feature, and they ask
//! for different pre-scenario communication. §3.4.1.1 says of scenario 1 that "Binding
//! SHOULD NOT be used for this Scenario", and §3.4.2 adds for scenario 2: "Actors that
//! write parts of a Feature within this Scenario need to create a binding […] Only one
//! binding partner is allowed to write the data specified in this Scenario."
//!
//! So a CEM needs both, and the one that is easy to leave out is the binding — every
//! monitoring use case in this crate needs none, this is the first here that *writes*, and
//! nothing in the payload of an offer mentions it. What a CEM without one gets is an offer
//! it can see, report, and never take up: [`activate`] is answered with `errorNumber` 9,
//! `bindingRequired`, before the payload is looked at. The SPINE test specification makes
//! that a conformance requirement of the *server* — [SPINE-TS-BIND-02], `TC_SPINE_BIND_002`
//! "Reject unbound write" — so a compressor that let the write through would be the one at
//! fault.
//!
//! [`CompressorPeer::follow`] sends the binding, the subscription and the initial read in
//! the order the two scenarios want them, and is what a CEM should call the moment
//! [`locate`] returns a peer.
//!
//! ```no_run
//! # use core::time::Duration;
//! use eebus::usecases::ohpcf;
//! # fn example(
//! #     engine: &mut eebus::spine::Engine,
//! #     remote: &eebus::spine::RemoteDevice,
//! #     client: &eebus::model::FeatureAddress,
//! #     now: Duration,
//! # ) -> Option<()> {
//! let compressor = ohpcf::locate(remote)?;
//! compressor.follow(engine, client, now);
//! # Some(())
//! # }
//! ```
//!
//! [`limitation`]: crate::usecases::limitation
//! [`emobility::opev`]: crate::usecases::emobility::opev
//! [`cob`]: crate::usecases::cob
//! [`hvac::cdt`]: crate::usecases::hvac::cdt

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use crate::model::{
    AlternativesId, CmdData, EntityType, FeatureType, Function, PowerSequenceId,
    PowerSequenceState, PowerTimeSlotNumber, PowerTimeSlotValueType, Role, ScaledNumber,
    SmartEnergyManagementPsAlternatives, SmartEnergyManagementPsAlternativesRelation,
    SmartEnergyManagementPsData, SmartEnergyManagementPsDataNodeScheduleInformation,
    SmartEnergyManagementPsPowerSequence, SmartEnergyManagementPsPowerSequenceDescription,
    SmartEnergyManagementPsPowerSequenceOperatingConstraintsDuration,
    SmartEnergyManagementPsPowerSequenceOperatingConstraintsInterrupt,
    SmartEnergyManagementPsPowerSequenceSchedule, SmartEnergyManagementPsPowerSequenceState,
    SmartEnergyManagementPsPowerTimeSlot, SmartEnergyManagementPsPowerTimeSlotSchedule,
    SmartEnergyManagementPsPowerTimeSlotValueList,
    SmartEnergyManagementPsPowerTimeSlotValueListValue, UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
///
/// The SMA Home Manager 2 in `tests/fixtures/devices` announces exactly this, which is
/// what says the use case is deployed rather than only specified.
pub const NAME: &str = "optimizationOfSelfConsumptionByHeatPumpCompressorFlexibility";

/// The actor that decides when the compressor should run.
pub const CEM_ACTOR: &str = "CEM";

/// The actor that runs: a component of a heat pump (§3.2.2.1).
pub const COMPRESSOR_ACTOR: &str = "Compressor";

/// The `alternativesId` **this** implementation publishes under.
///
/// A local choice, `<as1#(1..1)>`. [OHPCF-013] permits exactly one alternative at a time,
/// so a peer's is whichever one it sent; [`CompressorOffer::read`] takes it from the
/// payload rather than assuming this.
pub const ALTERNATIVES_ID: AlternativesId = AlternativesId(1);

/// The `sequenceId` **this** implementation publishes under.
///
/// `<s1#(1..1)>`, and the identifier every [OHPCF-021] and [OHPCF-022] write addresses. A
/// CEM takes a peer's from [`CompressorOffer::sequence`]: writing to the wrong sequence is
/// a request the compressor cannot attribute.
pub const SEQUENCE_ID: PowerSequenceId = PowerSequenceId(1);

/// The `slotNumber` of the one time slot Table 11 gives this use case.
pub const SLOT_NUMBER: PowerTimeSlotNumber = PowerTimeSlotNumber(1);

// ---- the feature a Compressor serves -------------------------------------------------

/// Builds the `SmartEnergyManagementPs` feature both scenarios are served from (Table 10).
///
/// One function, read **and write**: scenario 1 reads it and scenario 2 writes into it.
/// Table 10 marks the write's partial support `M` rather than `R`, and that is not a
/// detail — [OHPCF-021] sets `schedule.startTime` and [OHPCF-022] sets `state.state`, each
/// leaving the rest of a large document alone. A full write would have the CEM restating
/// the compressor's own announcement back at it, and getting it wrong.
///
/// Writes are **deferred**: the compressor decides. A pause on a process that is not
/// running, a stop on one that was never announced as stoppable — those are refusals, and
/// a refusal has to come before the acknowledgement rather than after it. Feed each
/// [`SpineEvent::WriteRequested`](crate::spine::SpineEvent::WriteRequested) to
/// [`Flexibility::apply`] and answer with
/// [`Engine::accept_write_with`](crate::spine::Engine::accept_write_with) — handing back
/// [`Flexibility::data`] — or [`reject_write`](crate::spine::Engine::reject_write).
///
/// **`accept_write_with`, not `accept_write`**, and the difference is on the wire.
/// `smartEnergyManagementPsData` is one value rather than a list SPINE can address entries
/// within, so storing the CEM's partial write replaces the compressor's whole
/// `alternatives` element with its two-field fragment — and §7.4.1 then notifies every
/// subscriber of a compressor that has just withdrawn its power value and both of its
/// interrupt options, the writer included.
pub fn flexibility_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::SmartEnergyManagementPs, Role::Server)
        .with_function(
            Function::SmartEnergyManagementPsData,
            Operations::read_write(),
        )
        .with_deferred_writes()
}

/// Whether the announced power is what the compressor will draw or a ceiling on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PowerKind {
    /// `power` [OHPCF-011/2/2]: "a good approximation".
    #[default]
    Approximate,
    /// `powerMax` [OHPCF-011/2/3]: the most it will draw.
    Maximum,
}

impl PowerKind {
    fn value_type(self) -> PowerTimeSlotValueType {
        match self {
            Self::Approximate => PowerTimeSlotValueType::Power,
            Self::Maximum => PowerTimeSlotValueType::PowerMax,
        }
    }

    fn of(value_type: &PowerTimeSlotValueType) -> Option<Self> {
        match value_type {
            PowerTimeSlotValueType::Power => Some(Self::Approximate),
            PowerTimeSlotValueType::PowerMax => Some(Self::Maximum),
            _ => None,
        }
    }
}

/// How the CEM may interrupt a running process.
///
/// [OHPCF-011/7] is why this is an enum with no empty case: "the Actor Compressor SHALL
/// offer at least one option for an interrupt by the Actor CEM". A compressor that
/// announces neither has announced a process the CEM can start and never get back out of,
/// and the specification does not allow it. Table 11 states the rule twice, once from each
/// side: if `isPausable` is not `true` then `isStoppable` SHALL be, and the other way
/// round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    /// The CEM may stop the process, and not pause it.
    Stoppable,
    /// The CEM may pause and resume it, and not stop it.
    Pausable,
    /// Both.
    Either,
}

impl Interrupt {
    /// Whether the CEM may abort the process.
    pub fn is_stoppable(self) -> bool {
        matches!(self, Self::Stoppable | Self::Either)
    }

    /// Whether the CEM may pause and resume it.
    pub fn is_pausable(self) -> bool {
        matches!(self, Self::Pausable | Self::Either)
    }

    /// What a peer's two flags amount to, or [`None`] where it announced neither.
    ///
    /// [`None`] is a compressor breaking [OHPCF-011/7], and it is worth seeing rather than
    /// defaulting: a CEM that read it as "stoppable" would schedule a process it cannot
    /// end, and one that read it as "pausable" would try to pause a compressor that will
    /// refuse.
    pub fn of(stoppable: bool, pausable: bool) -> Option<Self> {
        match (stoppable, pausable) {
            (true, true) => Some(Self::Either),
            (true, false) => Some(Self::Stoppable),
            (false, true) => Some(Self::Pausable),
            (false, false) => None,
        }
    }
}

/// What the compressor needs of any plan (`operatingConstraintsDuration`).
///
/// Both are optional in Table 11 and both are worth publishing: a compressor started and
/// stopped every two minutes is a compressor being destroyed, and the CEM is the only
/// thing in a position to avoid it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Durations {
    /// [OHPCF-008]: the least time it must run before it may be stopped.
    pub active_min: Option<Duration>,
    /// [OHPCF-009]: the least time it must stay off before it may start again.
    pub pause_min: Option<Duration>,
}

impl Durations {
    /// No constraints published.
    pub fn new() -> Self {
        Self::default()
    }

    /// [OHPCF-008]: the least time the compressor must run once started.
    #[must_use]
    pub fn at_least(mut self, active_min: Duration) -> Self {
        self.active_min = Some(active_min);
        self
    }

    /// [OHPCF-009]: the least time it must stay off before starting again.
    #[must_use]
    pub fn resting(mut self, pause_min: Duration) -> Self {
        self.pause_min = Some(pause_min);
        self
    }

    fn is_empty(&self) -> bool {
        self.active_min.is_none() && self.pause_min.is_none()
    }
}

// ---- the Compressor's side -----------------------------------------------------------

/// The compressor's optional power consumption process, as it stands.
///
/// Hold one, render it with [`data`](Self::data) whenever a peer reads or subscribes, and
/// feed it every write the CEM makes with [`apply`](Self::apply). It is a state machine of
/// six states, and it refuses what the specification does not allow rather than accepting
/// it and doing nothing — a CEM that pauses a process which is not running has made a
/// mistake, and being told so is more use than an acknowledgement that changes nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Flexibility {
    sequence: PowerSequenceId,
    alternatives: AlternativesId,
    power_watts: f64,
    kind: PowerKind,
    interrupt: Interrupt,
    durations: Durations,
    state: PowerSequenceState,
    start_time: Option<Duration>,
    /// Whether there is a process at all. `false` is phase D, [OHPCF-003].
    offered: bool,
}

impl Flexibility {
    /// An offer: the compressor could run at this many watts, and will not start by itself.
    ///
    /// Phase A. `state: inactive`, no `schedule` — which is exactly what [OHPCF-011/3]
    /// means by "will not be started autonomously by the server", since Table 11 says an
    /// absent `schedule` denotes it.
    ///
    /// The default interrupt is [`Interrupt::Stoppable`], the weaker of the two that
    /// satisfies [OHPCF-011/7]; say otherwise with
    /// [`interruptible`](Self::interruptible).
    pub fn offered(power_watts: f64) -> Self {
        Self {
            sequence: SEQUENCE_ID,
            alternatives: ALTERNATIVES_ID,
            power_watts,
            kind: PowerKind::Approximate,
            interrupt: Interrupt::Stoppable,
            durations: Durations::new(),
            state: PowerSequenceState::Inactive,
            start_time: None,
            offered: true,
        }
    }

    /// Whether the announced power is an approximation or a ceiling ([OHPCF-011/2]).
    #[must_use]
    pub fn as_maximum(mut self) -> Self {
        self.kind = PowerKind::Maximum;
        self
    }

    /// How the CEM may interrupt it ([OHPCF-011/5], [OHPCF-011/6]).
    #[must_use]
    pub fn interruptible(mut self, interrupt: Interrupt) -> Self {
        self.interrupt = interrupt;
        self
    }

    /// The minimum run and rest times ([OHPCF-008], [OHPCF-009]).
    #[must_use]
    pub fn lasting(mut self, durations: Durations) -> Self {
        self.durations = durations;
        self
    }

    /// Publishes it under a different `sequenceId` and `alternativesId`.
    ///
    /// For a compressor whose numbering is not this crate's, which is every device that
    /// serves something else from the same feature.
    #[must_use]
    pub fn numbered(mut self, alternatives: AlternativesId, sequence: PowerSequenceId) -> Self {
        self.alternatives = alternatives;
        self.sequence = sequence;
        self
    }

    /// The state the process is in.
    pub fn state(&self) -> PowerSequenceState {
        self.state.clone()
    }

    /// How long from now the process is due to start, once the CEM has said.
    ///
    /// A **span**, not an instant, and that is the schema's doing rather than a
    /// simplification: `SmartEnergyManagementPs` restricts `schedule.startTime` to
    /// `xs:duration` (SPINE 1.3.0 `EEBus_SPINE_TS_SmartEnergyManagementPs.xsd`), where the
    /// generic `PowerSequences` feature it derives from allows the
    /// `AbsoluteOrRelativeTimeType` union. So `PT0S` is "now" and `PT2H` is "in two hours",
    /// and a wall-clock instant is not expressible here at all — which is the right answer
    /// for a compressor that may have no clock to read one against.
    pub fn start_time(&self) -> Option<Duration> {
        self.start_time
    }

    /// How the CEM may interrupt it.
    pub fn interrupt(&self) -> Interrupt {
        self.interrupt
    }

    /// Whether there is an offer or a process at all.
    ///
    /// `false` is phase D — [OHPCF-003], "there is no process" — which is what the
    /// compressor announces before it can offer anything and after a process has ended.
    pub fn is_offered(&self) -> bool {
        self.offered
    }

    /// Withdraws the offer or the process (phases C and D).
    ///
    /// [OHPCF-006/2]: a compressor aborting an announced process may say so by announcing
    /// the *absence* of one, rather than explicitly. After this, [`data`](Self::data)
    /// renders the empty document.
    pub fn withdraw(&mut self) {
        self.offered = false;
        self.state = PowerSequenceState::Inactive;
        self.start_time = None;
    }

    /// Offers it again, from `inactive` with no schedule (phase A).
    pub fn offer_again(&mut self) {
        self.offered = true;
        self.state = PowerSequenceState::Inactive;
        self.start_time = None;
    }

    /// The compressor starts the process it was told to schedule ([OHPCF-012/4]).
    ///
    /// Autonomous, at the `startTime` the CEM named — which is why this is a method on the
    /// compressor and not something the CEM writes: `running` arrives at the CEM as a
    /// notification, not as an acknowledgement.
    pub fn start(&mut self) {
        self.state = PowerSequenceState::Running;
    }

    /// The process finished on its own ([OHPCF-006/3], [OHPCF-012/2/5]).
    ///
    /// The tank is full, the room is warm. Announced explicitly, which is what the
    /// specification requires of a *completion* — unlike an abortion, which may be
    /// announced as an absence instead.
    pub fn complete(&mut self) {
        self.state = PowerSequenceState::Completed;
        self.start_time = None;
    }

    /// The compressor aborted it itself ([OHPCF-006/1]).
    pub fn abort(&mut self) {
        self.state = PowerSequenceState::Invalid;
        self.start_time = None;
    }

    /// Renders the whole `smartEnergyManagementPsData`.
    ///
    /// `nodeScheduleInformation` is always here, even in phase D: Table 11's note says
    /// those elements "SHALL always be set properly even if currently no optional or
    /// scheduled power consumption process is available".
    pub fn data(&self) -> CmdData {
        CmdData::SmartEnergyManagementPsData(SmartEnergyManagementPsData {
            node_schedule_information: Some(SmartEnergyManagementPsDataNodeScheduleInformation {
                // Table 11 fixes all three of these values for this use case.
                node_remote_controllable: Some(true),
                supports_single_slot_scheduling_only: Some(true),
                alternatives_count: Some(if self.offered { 1 } else { 0 }),
                total_sequences_count_max: Some(1),
                supports_reselection: Some(false),
            }),
            // [OHPCF-003]: no alternatives at all is "there is no process".
            alternatives: Some(if self.offered {
                vec![self.alternative()]
            } else {
                Vec::new()
            }),
        })
    }

    /// Stores the current document on the feature, replacing what is there.
    ///
    /// For publishing a change the compressor made *by itself* — it started at the
    /// scheduled time, the tank filled, it aborted. A change a **CEM** asked for is stored
    /// by answering the write with
    /// [`Engine::accept_write_with`](crate::spine::Engine::accept_write_with) and this
    /// document, which stores and notifies in one step rather than leaving the peer's
    /// fragment on the feature for one notification.
    ///
    /// The state is the *device's*, not the peer's, which is why overwriting is the right
    /// operation here and merging is not.
    pub fn publish(
        &self,
        engine: &mut crate::spine::Engine,
        feature: &crate::model::FeatureAddress,
    ) {
        let address = feature.clone();
        if let Some(feature) = engine.device_mut().resolve_mut(&address) {
            let _ = feature.set_data(self.data());
        }
    }

    /// Publishes it and tells every subscriber.
    ///
    /// Scenario 1 is a subscription: §3.3.4 has the CEM subscribe to the compressor rather
    /// than poll it, so a state change the compressor made on its own — it started at the
    /// scheduled time, the tank filled, it aborted — reaches the CEM only through this.
    ///
    /// Not needed after an accepted write:
    /// [`Engine::accept_write_with`](crate::spine::Engine::accept_write_with) already
    /// notifies, and calling both sends the same document twice.
    pub fn notify(
        &self,
        engine: &mut crate::spine::Engine,
        feature: &crate::model::FeatureAddress,
        now: Duration,
    ) {
        self.publish(engine, feature);
        engine.notify(feature, &Function::SmartEnergyManagementPsData, now);
    }

    fn alternative(&self) -> SmartEnergyManagementPsAlternatives {
        SmartEnergyManagementPsAlternatives {
            relation: Some(SmartEnergyManagementPsAlternativesRelation {
                alternatives_id: Some(self.alternatives),
            }),
            power_sequence: Some(vec![SmartEnergyManagementPsPowerSequence {
                description: Some(SmartEnergyManagementPsPowerSequenceDescription {
                    sequence_id: Some(self.sequence),
                    power_unit: Some(UnitOfMeasurement::W),
                    ..Default::default()
                }),
                state: Some(SmartEnergyManagementPsPowerSequenceState {
                    state: Some(self.state.clone()),
                    // Table 11: present only while the process is running or paused.
                    active_slot_number: matches!(
                        self.state,
                        PowerSequenceState::Running | PowerSequenceState::Paused
                    )
                    .then_some(SLOT_NUMBER),
                    sequence_remote_controllable: Some(true),
                    ..Default::default()
                }),
                // An absent `schedule` is [OHPCF-011/3]: not started autonomously.
                schedule: self.start_time.map(|start| {
                    SmartEnergyManagementPsPowerSequenceSchedule {
                        start_time: Some(crate::model::format_iso8601_duration(start)),
                        ..Default::default()
                    }
                }),
                operating_constraints_interrupt: Some(
                    SmartEnergyManagementPsPowerSequenceOperatingConstraintsInterrupt {
                        // Table 11 has each flag present only when it is true, and
                        // [OHPCF-011/7] guarantees at least one of them is.
                        is_pausable: self.interrupt.is_pausable().then_some(true),
                        is_stoppable: self.interrupt.is_stoppable().then_some(true),
                        ..Default::default()
                    },
                ),
                operating_constraints_duration: (!self.durations.is_empty()).then(|| {
                    SmartEnergyManagementPsPowerSequenceOperatingConstraintsDuration {
                        active_duration_min: self
                            .durations
                            .active_min
                            .map(crate::model::format_iso8601_duration),
                        pause_duration_min: self
                            .durations
                            .pause_min
                            .map(crate::model::format_iso8601_duration),
                        ..Default::default()
                    }
                }),
                power_time_slot: Some(vec![SmartEnergyManagementPsPowerTimeSlot {
                    schedule: Some(SmartEnergyManagementPsPowerTimeSlotSchedule {
                        slot_number: Some(SLOT_NUMBER),
                        // [OHPCF-011/4]: an absent `defaultDuration` is a duration a heat
                        // pump compressor cannot predict, which is the normal case here.
                        ..Default::default()
                    }),
                    value_list: Some(SmartEnergyManagementPsPowerTimeSlotValueList {
                        value: Some(vec![SmartEnergyManagementPsPowerTimeSlotValueListValue {
                            value_type: Some(self.kind.value_type()),
                            value: Some(ScaledNumber::from_f64(self.power_watts, 0)),
                        }]),
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }]),
        }
    }

    /// Takes what a CEM wrote, and says what it asked for or why it cannot have it.
    ///
    /// **Give it the resolved state, not the fragment.** Every write in this use case is
    /// partial by Table 10's own requirement, and a fragment read as a whole document is a
    /// process with no state and no schedule.
    ///
    /// What it refuses, and why each refusal is worth having rather than a silent success:
    ///
    /// * a schedule for a process that has already started — §2.5.2 permits a re-schedule
    ///   "as long as the scheduled process did not start", and after that only the state
    ///   commands apply;
    /// * a stop on a process the compressor did not announce as stoppable, and a pause on
    ///   one it did not announce as pausable ([OHPCF-011/5], [OHPCF-011/6]);
    /// * a pause on something that is not running, and a resume on something that is not
    ///   paused ([OHPCF-022/2], [OHPCF-022/3] both name the state they apply in);
    /// * anything at all addressed to a sequence this compressor does not publish.
    ///
    /// On success the state has already moved: an accepted write is a change, and the
    /// caller's next [`data`](Self::data) reflects it.
    pub fn apply(&mut self, resolved: &CmdData) -> Result<Request, Refused> {
        let CmdData::SmartEnergyManagementPsData(data) = resolved else {
            return Err(Refused::NotThisFunction);
        };
        if !self.offered {
            return Err(Refused::NothingOffered);
        }
        let sequence = data
            .alternatives
            .iter()
            .flatten()
            .flat_map(|alternative| alternative.power_sequence.iter().flatten())
            .find(|sequence| {
                sequence
                    .description
                    .as_ref()
                    .and_then(|d| d.sequence_id)
                    .is_none_or(|id| id == self.sequence)
            })
            .ok_or(Refused::UnknownSequence)?;

        // [OHPCF-022] first: a state change is what the CEM asks for once the process
        // exists, and a command carrying both is answered on the state.
        if let Some(wanted) = sequence.state.as_ref().and_then(|s| s.state.as_ref()) {
            return self.change_state(wanted);
        }
        // [OHPCF-021]: a start time schedules the announced process.
        if let Some(start) = sequence
            .schedule
            .as_ref()
            .and_then(|schedule| schedule.start_time.as_ref())
        {
            return self.schedule(start);
        }
        Err(Refused::NothingAsked)
    }

    fn schedule(&mut self, start: &str) -> Result<Request, Refused> {
        // The schema restricts this element to `xs:duration`, so a value that is not one
        // is refused rather than stored and echoed back. A compressor that accepted
        // `2026-09-04T13:00:00Z` here — the shape the *generic* PowerSequences feature
        // allows, and the one a CEM written against it would send — would report itself
        // `scheduled` and start at a time neither side agrees on.
        let start =
            crate::model::parse_iso8601_duration(start).ok_or(Refused::UnreadableStartTime)?;
        match self.state {
            // §2.5.2 A: a re-schedule is the same command, and only before it starts.
            PowerSequenceState::Inactive | PowerSequenceState::Scheduled => {
                self.start_time = Some(start);
                self.state = PowerSequenceState::Scheduled;
                Ok(Request::Schedule { start_time: start })
            }
            PowerSequenceState::Completed | PowerSequenceState::Invalid => {
                Err(Refused::AlreadyEnded)
            }
            _ => Err(Refused::AlreadyStarted),
        }
    }

    fn change_state(&mut self, wanted: &PowerSequenceState) -> Result<Request, Refused> {
        // §2.5.1.1 B: the CEM may interrupt "as long as the power consumption has not been
        // stopped/aborted or completed". After that there is nothing to interrupt, and a
        // second stop reported as success would tell the CEM it did something.
        if matches!(
            self.state,
            PowerSequenceState::Completed | PowerSequenceState::Invalid
        ) {
            return Err(Refused::AlreadyEnded);
        }
        match wanted {
            // [OHPCF-022/1]: stop, which the specification spells `invalid`.
            PowerSequenceState::Invalid => {
                if !self.interrupt.is_stoppable() {
                    return Err(Refused::NotStoppable);
                }
                self.state = PowerSequenceState::Invalid;
                self.start_time = None;
                Ok(Request::Stop)
            }
            // [OHPCF-022/2]: pause, "if it is currently in a running state".
            PowerSequenceState::Paused => {
                if !self.interrupt.is_pausable() {
                    return Err(Refused::NotPausable);
                }
                if self.state != PowerSequenceState::Running {
                    return Err(Refused::NotRunning);
                }
                self.state = PowerSequenceState::Paused;
                Ok(Request::Pause)
            }
            // [OHPCF-022/3]: resume, "if it is currently in a paused state".
            PowerSequenceState::Running => {
                if self.state != PowerSequenceState::Paused {
                    return Err(Refused::NotPaused);
                }
                self.state = PowerSequenceState::Running;
                Ok(Request::Resume)
            }
            _ => Err(Refused::NotACommand),
        }
    }
}

/// What a CEM's write asked the compressor to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// [OHPCF-021]: run the announced process, starting then.
    Schedule {
        /// How long from now the CEM asked it to start — `PT0S` for immediately.
        start_time: Duration,
    },
    /// [OHPCF-022/1]: abort it.
    Stop,
    /// [OHPCF-022/2]: pause it.
    Pause,
    /// [OHPCF-022/3]: carry on.
    Resume,
}

/// Why a write could not be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// The payload is not `smartEnergyManagementPsData`.
    #[error("that is not this use case's function")]
    NotThisFunction,
    /// The compressor is announcing no process ([OHPCF-003]).
    #[error("there is no power consumption process to control")]
    NothingOffered,
    /// The write addresses a `sequenceId` this compressor does not publish.
    #[error("no power sequence of that identifier is published here")]
    UnknownSequence,
    /// The write named neither a start time nor a state.
    #[error("the write asked for nothing this use case defines")]
    NothingAsked,
    /// The `startTime` is not an `xs:duration`, which is what this feature restricts it to.
    ///
    /// The likely cause is a CEM written against the generic `PowerSequences` feature,
    /// whose `startTime` is the `AbsoluteOrRelativeTimeType` union and so accepts a
    /// timestamp. `SmartEnergyManagementPs` narrows it to a span, and storing a value this
    /// side cannot read would leave the compressor `scheduled` for a time it could not act
    /// on.
    #[error("`startTime` here is an ISO 8601 duration, such as `PT0S` or `PT2H`")]
    UnreadableStartTime,
    /// A schedule arrived for a process that has already started (§2.5.2).
    #[error("the process has already started; only stop, pause and resume apply now")]
    AlreadyStarted,
    /// A request arrived for a process that has completed or been aborted (§2.5.1.1 C).
    ///
    /// There is nothing left to schedule or interrupt. The compressor announces a fresh
    /// offer — [`Flexibility::offer_again`] — or the absence of one, and the CEM works from
    /// that.
    #[error("the process has ended; the compressor must announce a new offer first")]
    AlreadyEnded,
    /// A stop arrived for a process announced as not stoppable ([OHPCF-011/5]).
    #[error("this process was not announced as stoppable")]
    NotStoppable,
    /// A pause arrived for a process announced as not pausable ([OHPCF-011/6]).
    #[error("this process was not announced as pausable")]
    NotPausable,
    /// A pause arrived for a process that is not running ([OHPCF-022/2]).
    #[error("only a running process can be paused")]
    NotRunning,
    /// A resume arrived for a process that is not paused ([OHPCF-022/3]).
    #[error("only a paused process can be resumed")]
    NotPaused,
    /// A state the CEM is not permitted to write.
    #[error("a CEM may only write `invalid`, `paused` or `running`")]
    NotACommand,
}

impl Refused {
    /// The `errorNumber` to answer the write with.
    ///
    /// Everything here is the CEM asking for something this compressor will not do, which
    /// is what `commandRejected` says. A refusal is not a fault: §2.5.2's note is explicit
    /// that the compressor may also stop the process by itself, so a CEM and a compressor
    /// disagreeing about the state is an ordinary race rather than a broken peer.
    pub fn error_number(self) -> crate::spine::ErrorNumber {
        crate::spine::ErrorNumber::CommandRejected
    }
}

impl crate::usecases::signals::Signals for Flexibility {
    /// What a laboratory reads off a compressor.
    ///
    /// The High-Level Test Specifications keep asking for a debug interface — "the
    /// manufacturer must specify conditions on how the test case can be tested" — and this
    /// use case needs one more than most: the compressor **starts by itself** at the time
    /// the CEM named, so the transition into `running` is a timer firing rather than a
    /// message, and there is nothing on the wire to observe it by.
    ///
    /// ```
    /// use eebus::usecases::ohpcf::{Flexibility, Interrupt};
    /// use eebus::usecases::signals::Signals;
    ///
    /// let offer = Flexibility::offered(2_400.0).interruptible(Interrupt::Either);
    /// let signals = offer.signals(());
    /// assert_eq!(signals.get("ohpcf:state").and_then(|v| v.as_str()), Some("inactive"));
    /// assert_eq!(signals.get("ohpcf:power").and_then(|v| v.as_f64()), Some(2_400.0));
    /// ```
    fn signals(&self, _: ()) -> crate::usecases::signals::SignalSet {
        use crate::usecases::signals::{Signal, SignalSet, SignalValue};
        use alloc::borrow::Cow;

        let text = |value: &str| SignalValue::Text(Cow::Owned(value.to_string()));
        SignalSet::new()
            .with(Signal::new(
                "ohpcf:offered",
                SignalValue::Bool(self.offered),
            ))
            .with(Signal::new(
                "ohpcf:state",
                if self.offered {
                    text(self.state.as_str())
                } else {
                    SignalValue::Absent
                },
            ))
            .with(Signal::new(
                "ohpcf:power",
                if self.offered {
                    SignalValue::Number(self.power_watts)
                } else {
                    SignalValue::Absent
                },
            ))
            .with(Signal::new(
                "ohpcf:startTime",
                match self.start_time {
                    // Seconds from now, as the element itself is a span. The other two
                    // durations in this set are reported the same way.
                    Some(start) => SignalValue::Number(start.as_secs_f64()),
                    None => SignalValue::Absent,
                },
            ))
            .with(Signal::new(
                "ohpcf:isStoppable",
                SignalValue::Bool(self.interrupt.is_stoppable()),
            ))
            .with(Signal::new(
                "ohpcf:isPausable",
                SignalValue::Bool(self.interrupt.is_pausable()),
            ))
            .with(Signal::new(
                "ohpcf:activeDurationMin",
                match self.durations.active_min {
                    Some(duration) => SignalValue::Number(duration.as_secs_f64()),
                    None => SignalValue::Absent,
                },
            ))
            .with(Signal::new(
                "ohpcf:pauseDurationMin",
                match self.durations.pause_min {
                    Some(duration) => SignalValue::Number(duration.as_secs_f64()),
                    None => SignalValue::Absent,
                },
            ))
    }
}

// ---- the CEM's side ------------------------------------------------------------------

/// What a CEM learned about one compressor's flexibility.
///
/// Read from a `smartEnergyManagementPsData` — the reply to the opening read, or any
/// notification after it. [`is_available`](Self::is_available) is the question scenario 2
/// turns on: an offer that is `inactive` with no schedule is one the CEM may take up.
#[derive(Clone, Debug, PartialEq)]
pub struct CompressorOffer {
    /// The alternative the offer belongs to.
    pub alternatives: AlternativesId,
    /// The sequence every write addresses.
    pub sequence: PowerSequenceId,
    /// What it would draw, in watts. [`None`] where the compressor published no value.
    pub power_watts: Option<f64>,
    /// Whether that is an approximation or a ceiling ([OHPCF-011/2]).
    pub kind: Option<PowerKind>,
    /// The state of the process ([OHPCF-012/2]).
    pub state: PowerSequenceState,
    /// How long from now it starts, once it is scheduled. Absent means it will not start
    /// on its own ([OHPCF-011/3]).
    ///
    /// A span rather than an instant, because this feature restricts the element to
    /// `xs:duration` — see [`Flexibility::start_time`]. A compressor that put a timestamp
    /// here has broken the schema, and this reads [`None`] for it rather than a number
    /// nobody sent.
    pub start_time: Option<Duration>,
    /// Whether the CEM may abort it.
    pub is_stoppable: bool,
    /// Whether the CEM may pause and resume it.
    pub is_pausable: bool,
    /// [OHPCF-008]: how long it must run once started.
    pub active_duration_min: Option<Duration>,
    /// [OHPCF-009]: how long it must rest before starting again.
    pub pause_duration_min: Option<Duration>,
    /// `sequenceRemoteControllable`. A sequence that says `false` is one this use case
    /// cannot drive, whatever else it announced.
    pub remote_controllable: bool,
}

impl CompressorOffer {
    /// Reads a compressor's payload.
    ///
    /// **Give it the resolved state, not a fragment**: a partial notification carrying
    /// only the new state has no power value and no interrupt flags in it, and reading
    /// that as a whole offer is a compressor that has withdrawn everything it said.
    ///
    /// Returns [`None`] for phase D — a payload with no `alternatives` — which is
    /// [OHPCF-003], "there is no process", and is a *fact* rather than a parse failure.
    /// Tell the two apart with [`is_absent`].
    pub fn read(data: &CmdData) -> Option<Self> {
        let CmdData::SmartEnergyManagementPsData(data) = data else {
            return None;
        };
        let alternative = data.alternatives.iter().flatten().next()?;
        let sequence = alternative.power_sequence.iter().flatten().next()?;
        let description = sequence.description.as_ref();
        let interrupt = sequence.operating_constraints_interrupt.as_ref();
        let durations = sequence.operating_constraints_duration.as_ref();
        let state = sequence.state.as_ref();

        let value = sequence
            .power_time_slot
            .iter()
            .flatten()
            .flat_map(|slot| slot.value_list.iter())
            .flat_map(|list| list.value.iter().flatten())
            .find(|value| value.value_type.as_ref().and_then(PowerKind::of).is_some());

        Some(Self {
            alternatives: alternative
                .relation
                .as_ref()
                .and_then(|relation| relation.alternatives_id)
                .unwrap_or(ALTERNATIVES_ID),
            sequence: description.and_then(|d| d.sequence_id)?,
            power_watts: value
                .and_then(|value| value.value.as_ref())
                .and_then(ScaledNumber::to_f64),
            kind: value
                .and_then(|value| value.value_type.as_ref())
                .and_then(PowerKind::of),
            state: state
                .and_then(|state| state.state.clone())
                .unwrap_or(PowerSequenceState::Inactive),
            start_time: sequence
                .schedule
                .as_ref()
                .and_then(|schedule| schedule.start_time.as_deref())
                .and_then(crate::model::parse_iso8601_duration),
            is_stoppable: interrupt.and_then(|i| i.is_stoppable) == Some(true),
            is_pausable: interrupt.and_then(|i| i.is_pausable) == Some(true),
            active_duration_min: durations
                .and_then(|d| d.active_duration_min.as_deref())
                .and_then(crate::model::parse_iso8601_duration),
            pause_duration_min: durations
                .and_then(|d| d.pause_duration_min.as_deref())
                .and_then(crate::model::parse_iso8601_duration),
            // Table 11 fixes it to "true"; a compressor that says otherwise is telling
            // the CEM not to write, and taking the absence as `true` would ignore that.
            remote_controllable: state.and_then(|s| s.sequence_remote_controllable) != Some(false),
        })
    }

    /// How the compressor says it may be interrupted, or [`None`] if it announced neither.
    ///
    /// [`None`] breaks [OHPCF-011/7] and is worth surfacing: it is a process the CEM could
    /// start and never end.
    pub fn interrupt(&self) -> Option<Interrupt> {
        Interrupt::of(self.is_stoppable, self.is_pausable)
    }

    /// Whether this is an offer the CEM may take up (phase A).
    ///
    /// `inactive`, with no start time, and remotely controllable. A process already
    /// scheduled or running is not on offer — [OHPCF-013] permits only one at a time —
    /// and one that says it is not remotely controllable is not on offer either.
    pub fn is_available(&self) -> bool {
        self.remote_controllable
            && self.state == PowerSequenceState::Inactive
            && self.start_time.is_none()
    }

    /// Whether a process is scheduled or under way (phase B).
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            PowerSequenceState::Scheduled
                | PowerSequenceState::Running
                | PowerSequenceState::Paused
        )
    }

    /// Whether the process has ended, either way ([OHPCF-006]).
    pub fn has_ended(&self) -> bool {
        matches!(
            self.state,
            PowerSequenceState::Completed | PowerSequenceState::Invalid
        )
    }
}

/// Whether a payload is the compressor saying it has no process at all ([OHPCF-003]).
///
/// Phase D, and the reason [`CompressorOffer::read`] returning [`None`] is not the same as
/// a payload this crate could not understand.
pub fn is_absent(data: &CmdData) -> bool {
    let CmdData::SmartEnergyManagementPsData(data) = data else {
        return false;
    };
    data.node_schedule_information.is_some()
        && data
            .alternatives
            .as_ref()
            .is_none_or(|alternatives| alternatives.is_empty())
}

/// [OHPCF-021]: schedules the announced process to start at `start_time`.
///
/// A **partial** write — Table 10 makes partial support mandatory for exactly this — so
/// everything the compressor announced stays as it announced it, and only the start time
/// arrives. Send it with
/// [`Engine::write`](crate::spine::Engine::write) and `partial: true`.
///
/// `start_time` is how long from now, not a wall-clock instant. That is the schema's
/// restriction rather than a simplification: `SmartEnergyManagementPs` narrows
/// `schedule.startTime` to `xs:duration`, where the generic `PowerSequences` feature it
/// derives from allows the `AbsoluteOrRelativeTimeType` union. So [`Duration::ZERO`] is
/// "now" and two hours is "in two hours" — and a compressor with no clock can act on
/// either, which is presumably why the specification narrowed it.
///
/// Re-scheduling is the same write again, and §2.5.2 allows it up until the process
/// starts.
///
/// **Needs a binding.** §3.4.2/1, and it is the one step a CEM built out of this crate's
/// monitoring use cases has never had to take — see [`CompressorPeer::follow`].
pub fn activate(sequence: PowerSequenceId, start_time: Duration) -> CmdData {
    write_sequence(SmartEnergyManagementPsPowerSequence {
        description: Some(SmartEnergyManagementPsPowerSequenceDescription {
            sequence_id: Some(sequence),
            ..Default::default()
        }),
        schedule: Some(SmartEnergyManagementPsPowerSequenceSchedule {
            start_time: Some(crate::model::format_iso8601_duration(start_time)),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// [OHPCF-022/1]: aborts the process. The specification spells it `invalid`.
///
/// A write, so it needs the binding [`CompressorPeer::follow`] asks for.
pub fn stop(sequence: PowerSequenceId) -> CmdData {
    state_write(sequence, PowerSequenceState::Invalid)
}

/// [OHPCF-022/2]: pauses a running process.
///
/// A write, so it needs the binding [`CompressorPeer::follow`] asks for.
pub fn pause(sequence: PowerSequenceId) -> CmdData {
    state_write(sequence, PowerSequenceState::Paused)
}

/// [OHPCF-022/3]: resumes a paused one.
///
/// A write, so it needs the binding [`CompressorPeer::follow`] asks for.
pub fn resume(sequence: PowerSequenceId) -> CmdData {
    state_write(sequence, PowerSequenceState::Running)
}

fn state_write(sequence: PowerSequenceId, state: PowerSequenceState) -> CmdData {
    write_sequence(SmartEnergyManagementPsPowerSequence {
        description: Some(SmartEnergyManagementPsPowerSequenceDescription {
            sequence_id: Some(sequence),
            ..Default::default()
        }),
        state: Some(SmartEnergyManagementPsPowerSequenceState {
            state: Some(state),
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn write_sequence(sequence: SmartEnergyManagementPsPowerSequence) -> CmdData {
    CmdData::SmartEnergyManagementPsData(SmartEnergyManagementPsData {
        alternatives: Some(vec![SmartEnergyManagementPsAlternatives {
            power_sequence: Some(vec![sequence]),
            ..Default::default()
        }]),
        ..Default::default()
    })
}

// ---- what a CEM finds ----------------------------------------------------------------

/// Where one compressor's flexibility lives.
///
/// **Subscribe to be told; bind to write.** Both scenarios are served from the one feature
/// and they ask for different pre-scenario communication (§3.4.1.1, §3.4.2):
///
/// | | |
/// |---|---|
/// | scenario 1, *monitor* | "Binding SHOULD NOT be used for this Scenario"; a **subscription** is what carries the compressor's own state changes |
/// | scenario 2, *control* | "Actors that write parts of a Feature within this Scenario need to create a binding" |
///
/// So a CEM that only subscribes gets every notification and cannot act on any of them:
/// [`activate`], [`stop`], [`pause`] and [`resume`] are writes, and a compressor answers a
/// write from a peer holding no binding with
/// [`ErrorNumber::BindingRequired`](crate::model::ErrorNumber::BindingRequired) — 9 — before
/// the payload is looked at (`TC_SPINE_BIND_002`). Nothing in the offer says so; it is an
/// acknowledgement that never comes back positive.
///
/// [`follow`](Self::follow) sends all three requests in the order the two scenarios want.
#[derive(Clone, Debug, PartialEq)]
pub struct CompressorPeer {
    /// The peer's device address.
    pub device: crate::model::AddressDevice,
    /// Its `SmartEnergyManagementPs` feature: read for the offer, written to take it up.
    ///
    /// One feature, two privileges. A subscription on it is what scenario 1 needs and a
    /// binding is what scenario 2 needs, and holding one is no evidence of the other.
    pub flexibility: crate::model::FeatureAddress,
}

/// The three requests [`CompressorPeer::follow`] sent, in the order they went out.
///
/// Worth keeping. The two calls are answered under their own counters as
/// [`SpineEvent::ResultReceived`](crate::spine::SpineEvent::ResultReceived), and that is
/// the **only** place a refused binding is visible — nothing else on the wire says a CEM
/// may not write. Told apart by counter, "the compressor will not let me write" and "the
/// compressor will not tell me anything" are two different commissioning faults with two
/// different fixes; told apart by nothing, they are one silence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Following {
    /// The binding request scenario 2 needs before any write ([OHPCF-021], [OHPCF-022]).
    ///
    /// `ErrorNumber::None` under this counter is what says [`activate`] will be looked at
    /// rather than answered `BindingRequired`.
    pub binding: crate::model::MsgCounter,
    /// The subscription request scenario 1 runs on.
    ///
    /// Refused, a CEM should poll instead: §3.3.4 permits exactly that, and a compressor
    /// with no subscriptions left is a compressor whose own state changes reach nobody.
    pub subscription: crate::model::MsgCounter,
    /// The initial read of `smartEnergyManagementPsData` (§3.4.1.2).
    ///
    /// A *successful* read arrives as
    /// [`ReplyReceived`](crate::spine::SpineEvent::ReplyReceived), which carries the
    /// feature rather than this counter; a failed one is a `ResultReceived` under it. So
    /// this is the counter that says the read went wrong, not the one that says it went
    /// right.
    pub read: crate::model::MsgCounter,
}

impl CompressorPeer {
    /// Starts following a compressor: binds, subscribes, and reads what it is offering.
    ///
    /// The pre-scenario communication both scenarios ask for, in one call and in the order
    /// the specification puts it:
    ///
    /// 1. **Bind** (§3.3.3, §3.4.2/1). A write needs it, and "only one binding partner is
    ///    allowed to write the data specified in this Scenario" — so a second CEM asking is
    ///    refused, and finding that out at commissioning is better than finding it out at
    ///    the moment the roof starts exporting.
    /// 2. **Subscribe** (§3.3.4, §3.4.1.1/3). Scenario 1's "Actors SHALL create a
    ///    subscription". Without it a compressor that starts at its scheduled time, or
    ///    completes, or withdraws the offer, changes state and tells nobody.
    /// 3. **Read** (§3.4.1.2). The initial scenario communication, which "SHALL be
    ///    exchanged each time a (re-)connection is established, even if the Pre-Scenario
    ///    communication phase is skipped" — the offer may have changed while the connection
    ///    was down.
    ///
    /// Calling it again restarts all three, which is what a reconnection needs: neither the
    /// binding nor the subscription survived it.
    ///
    /// `client` is the CEM's own client feature — the `Generic` one the use-case
    /// implementation guide §3.3 asks for, which
    /// [`limitation::client_feature`](crate::usecases::limitation::client_feature) builds.
    ///
    /// ```no_run
    /// # use core::time::Duration;
    /// use eebus::usecases::ohpcf;
    /// # fn example(
    /// #     engine: &mut eebus::spine::Engine,
    /// #     remote: &eebus::spine::RemoteDevice,
    /// #     client: &eebus::model::FeatureAddress,
    /// #     now: Duration,
    /// # ) -> Option<()> {
    /// let compressor = ohpcf::locate(remote)?;
    /// let pending = compressor.follow(engine, client, now);
    /// // `pending.binding` is the counter to watch: until it is acknowledged, every
    /// // `activate`/`stop`/`pause`/`resume` this CEM sends comes back refused.
    /// # let _ = pending;
    /// # Some(())
    /// # }
    /// ```
    pub fn follow(
        &self,
        engine: &mut crate::spine::Engine,
        client: &crate::model::FeatureAddress,
        now: Duration,
    ) -> Following {
        Following {
            binding: engine.request_binding(client, &self.flexibility, now),
            subscription: engine.request_subscription(client, &self.flexibility, now),
            read: engine.read(
                &self.flexibility,
                client,
                Function::SmartEnergyManagementPsData,
                now,
            ),
        }
    }
}

/// Finds a compressor's feature from its detailed discovery and use-case data.
///
/// The entity is a `Compressor` **inside** a `HeatPumpAppliance` (§3.2.2.1), so its address
/// nests — `[1, 1]` under the appliance's `[1]` — and a lookup that searched the appliance
/// would find the appliance's own features instead. That is the guide's §3.3 rule doing its
/// work: the feature is on the entity that announced the actor.
///
/// Returns [`None`] until the peer has announced both the use case and the feature.
///
/// What comes next is [`CompressorPeer::follow`], and it is not optional: a located
/// compressor is an address, not a conversation. Scenario 1 needs a subscription and
/// scenario 2 needs a **binding**, and a CEM that took only the subscription can watch an
/// offer it will never be allowed to take up.
pub fn locate(remote: &crate::spine::RemoteDevice) -> Option<CompressorPeer> {
    let found = remote.use_case(NAME, COMPRESSOR_ACTOR)?;
    Some(CompressorPeer {
        device: remote.address.clone()?,
        flexibility: remote.address_of(
            found,
            &FeatureType::SmartEnergyManagementPs,
            Role::Server,
        )?,
    })
}

// ---- descriptors ---------------------------------------------------------------------

/// The compressor is a sub-entity of a heat pump appliance (§3.2.2.1).
const COMPRESSOR_ENTITIES: &[EntityType] = &[EntityType::Compressor];
/// The CEM sits on a CEM entity.
const CEM_ENTITIES: &[EntityType] = &[EntityType::CEM];

const SERVER_FUNCTIONS: &[FunctionUse] = &[FunctionUse::server_writeable(
    FeatureType::SmartEnergyManagementPs,
    Function::SmartEnergyManagementPsData,
)];

const CLIENT_FUNCTIONS: &[FunctionUse] = &[FunctionUse::client_writes(
    FeatureType::SmartEnergyManagementPs,
    Function::SmartEnergyManagementPsData,
)];

const MONITOR: &str = "Monitor heat pump compressor's power consumption flexibility";
const CONTROL: &str = "Control heat pump compressor's power consumption flexibility";

/// The Compressor: the actor that runs.
pub static COMPRESSOR: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: COMPRESSOR_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: COMPRESSOR_ENTITIES,
    counterpart: CEM_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: MONITOR,
            support: Support::Mandatory,
            functions: SERVER_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: CONTROL,
            support: Support::Mandatory,
            functions: SERVER_FUNCTIONS,
        },
    ],
};

/// The CEM: the actor that decides when it should.
pub static CEM: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: CEM_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: CEM_ENTITIES,
    counterpart: COMPRESSOR_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: MONITOR,
            support: Support::Mandatory,
            functions: CLIENT_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: CONTROL,
            support: Support::Mandatory,
            functions: CLIENT_FUNCTIONS,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn an_offer() -> Flexibility {
        Flexibility::offered(2_400.0)
            .interruptible(Interrupt::Either)
            .lasting(
                Durations::new()
                    .at_least(Duration::from_secs(20 * 60))
                    .resting(Duration::from_secs(10 * 60)),
            )
    }

    /// Phase A: an offer is `inactive` with no schedule, and that absence is the message.
    #[test]
    fn an_offer_says_it_will_not_start_by_itself() {
        let offer = an_offer();
        let read = CompressorOffer::read(&offer.data()).expect("an offer");

        assert_eq!(read.state, PowerSequenceState::Inactive);
        assert_eq!(
            read.start_time, None,
            "[OHPCF-011/3]: an absent schedule is 'not started autonomously'"
        );
        assert!(read.is_available());
        assert!(!read.is_active() && !read.has_ended());
        assert_eq!(read.power_watts, Some(2_400.0));
        assert_eq!(read.kind, Some(PowerKind::Approximate));
        assert_eq!(read.interrupt(), Some(Interrupt::Either));
        assert_eq!(read.active_duration_min, Some(Duration::from_secs(1_200)));
        assert_eq!(read.pause_duration_min, Some(Duration::from_secs(600)));
        assert!(read.remote_controllable);
    }

    /// [OHPCF-021]: the CEM schedules it, and the compressor starts by itself at the time.
    #[test]
    fn the_cem_schedules_the_process_and_the_compressor_starts_it() {
        let mut compressor = an_offer();
        let write = activate(SEQUENCE_ID, Duration::from_secs(2 * 3_600));

        assert_eq!(
            compressor.apply(&write),
            Ok(Request::Schedule {
                start_time: Duration::from_secs(7_200)
            })
        );
        assert_eq!(compressor.state(), PowerSequenceState::Scheduled);

        let read = CompressorOffer::read(&compressor.data()).expect("a process");
        assert!(read.is_active() && !read.is_available());
        assert_eq!(read.start_time, Some(Duration::from_secs(7_200)));

        // [OHPCF-012/4]: the compressor begins on its own, and the CEM hears about it.
        compressor.start();
        let read = CompressorOffer::read(&compressor.data()).expect("a process");
        assert_eq!(read.state, PowerSequenceState::Running);
    }

    /// §2.5.2 A: a re-schedule is allowed until it starts, and not afterwards.
    #[test]
    fn a_reschedule_is_refused_once_the_process_has_started() {
        let mut compressor = an_offer();
        compressor
            .apply(&activate(SEQUENCE_ID, Duration::from_secs(3_600)))
            .expect("scheduled");
        assert!(
            compressor
                .apply(&activate(SEQUENCE_ID, Duration::from_secs(7_200)))
                .is_ok(),
            "a re-schedule before it starts is the same command again"
        );

        compressor.start();
        assert_eq!(
            compressor.apply(&activate(SEQUENCE_ID, Duration::from_secs(10_800))),
            Err(Refused::AlreadyStarted)
        );
    }

    /// The element is `xs:duration` here, so a timestamp is refused rather than stored.
    ///
    /// `SmartEnergyManagementPs` restricts `schedule.startTime` where the generic
    /// `PowerSequences` feature allows the `AbsoluteOrRelativeTimeType` union, so a CEM
    /// written against the wrong one of the two sends `2026-09-04T13:00:00Z` in perfectly
    /// good faith. Storing it would leave the compressor announcing itself `scheduled` for
    /// a time neither side could act on.
    #[test]
    fn a_start_time_that_is_not_a_duration_is_refused() {
        let mut compressor = an_offer();
        let timestamp = write_sequence(SmartEnergyManagementPsPowerSequence {
            description: Some(SmartEnergyManagementPsPowerSequenceDescription {
                sequence_id: Some(SEQUENCE_ID),
                ..Default::default()
            }),
            schedule: Some(SmartEnergyManagementPsPowerSequenceSchedule {
                start_time: Some("2026-09-04T13:00:00Z".into()),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(
            compressor.apply(&timestamp),
            Err(Refused::UnreadableStartTime)
        );
        assert_eq!(
            compressor.state(),
            PowerSequenceState::Inactive,
            "and the offer is left as it was, rather than scheduled for nothing"
        );
        assert_eq!(compressor.start_time(), None);

        // The same value read off a compressor that did store it: `None`, not a guess.
        let offer = CompressorOffer::read(&compressor.data()).expect("an offer");
        assert_eq!(offer.start_time, None);

        // `PT0S` — start now — is a duration and is accepted.
        assert_eq!(
            compressor.apply(&activate(SEQUENCE_ID, Duration::ZERO)),
            Ok(Request::Schedule {
                start_time: Duration::ZERO
            })
        );
    }

    /// [OHPCF-022]: stop, pause and resume, each only where the state allows it.
    #[test]
    fn the_state_commands_apply_only_in_the_states_they_are_defined_for() {
        let mut compressor = an_offer();
        compressor
            .apply(&activate(SEQUENCE_ID, Duration::ZERO))
            .expect("scheduled");

        assert_eq!(
            compressor.apply(&pause(SEQUENCE_ID)),
            Err(Refused::NotRunning),
            "[OHPCF-022/2] applies to a running process"
        );
        compressor.start();
        assert_eq!(compressor.apply(&pause(SEQUENCE_ID)), Ok(Request::Pause));
        assert_eq!(compressor.state(), PowerSequenceState::Paused);

        assert_eq!(
            compressor.apply(&pause(SEQUENCE_ID)),
            Err(Refused::NotRunning),
            "pausing a paused process asks for nothing"
        );
        assert_eq!(compressor.apply(&resume(SEQUENCE_ID)), Ok(Request::Resume));
        assert_eq!(compressor.state(), PowerSequenceState::Running);

        assert_eq!(compressor.apply(&stop(SEQUENCE_ID)), Ok(Request::Stop));
        assert_eq!(compressor.state(), PowerSequenceState::Invalid);
        let read = CompressorOffer::read(&compressor.data()).expect("a process");
        assert!(read.has_ended());
    }

    /// §2.5.1.1 C: once the process has ended there is nothing left to ask of it.
    #[test]
    fn a_process_that_has_ended_takes_no_further_commands() {
        for ending in [
            Flexibility::complete as fn(&mut Flexibility),
            Flexibility::abort as fn(&mut Flexibility),
        ] {
            let mut compressor = an_offer();
            compressor
                .apply(&activate(SEQUENCE_ID, Duration::ZERO))
                .unwrap();
            compressor.start();
            ending(&mut compressor);

            assert_eq!(
                compressor.apply(&stop(SEQUENCE_ID)),
                Err(Refused::AlreadyEnded),
                "a second stop reported as success would tell the CEM it did something"
            );
            assert_eq!(
                compressor.apply(&pause(SEQUENCE_ID)),
                Err(Refused::AlreadyEnded)
            );
            assert_eq!(
                compressor.apply(&activate(SEQUENCE_ID, Duration::from_secs(3_600))),
                Err(Refused::AlreadyEnded),
                "and a new run starts from a new offer, not from the old one"
            );

            // Which the compressor announces when it is ready to run again.
            compressor.offer_again();
            assert!(
                CompressorOffer::read(&compressor.data())
                    .expect("an offer")
                    .is_available()
            );
            assert!(
                compressor
                    .apply(&activate(SEQUENCE_ID, Duration::from_secs(3_600)))
                    .is_ok()
            );
        }
    }

    /// [OHPCF-011/5], [OHPCF-011/6]: an interrupt the compressor did not offer is refused.
    #[test]
    fn an_interrupt_the_compressor_never_offered_is_refused() {
        let mut only_stoppable = Flexibility::offered(2_400.0).interruptible(Interrupt::Stoppable);
        only_stoppable
            .apply(&activate(SEQUENCE_ID, Duration::ZERO))
            .unwrap();
        only_stoppable.start();
        assert_eq!(
            only_stoppable.apply(&pause(SEQUENCE_ID)),
            Err(Refused::NotPausable)
        );
        assert_eq!(only_stoppable.apply(&stop(SEQUENCE_ID)), Ok(Request::Stop));

        let mut only_pausable = Flexibility::offered(2_400.0).interruptible(Interrupt::Pausable);
        only_pausable
            .apply(&activate(SEQUENCE_ID, Duration::ZERO))
            .unwrap();
        only_pausable.start();
        assert_eq!(
            only_pausable.apply(&stop(SEQUENCE_ID)),
            Err(Refused::NotStoppable)
        );
        assert_eq!(only_pausable.apply(&pause(SEQUENCE_ID)), Ok(Request::Pause));
    }

    /// [OHPCF-011/7]: a compressor always offers at least one way out, and a peer that
    /// does not is reported rather than assumed.
    #[test]
    fn a_process_with_no_way_out_is_visible() {
        assert_eq!(Interrupt::of(false, false), None);
        assert_eq!(Interrupt::of(true, false), Some(Interrupt::Stoppable));
        assert_eq!(Interrupt::of(false, true), Some(Interrupt::Pausable));
        assert_eq!(Interrupt::of(true, true), Some(Interrupt::Either));

        // Every `Flexibility` this crate builds satisfies the rule by construction.
        for interrupt in [Interrupt::Stoppable, Interrupt::Pausable, Interrupt::Either] {
            let offer = Flexibility::offered(1.0).interruptible(interrupt);
            let read = CompressorOffer::read(&offer.data()).expect("an offer");
            assert_eq!(read.interrupt(), Some(interrupt));
        }
    }

    /// Phase D: no alternatives is "there is no process", and it is a fact, not a failure.
    #[test]
    fn an_absent_process_is_told_apart_from_an_unreadable_one() {
        let mut compressor = an_offer();
        compressor.withdraw();
        let data = compressor.data();

        assert!(is_absent(&data), "[OHPCF-003]");
        assert_eq!(CompressorOffer::read(&data), None);
        assert_eq!(
            compressor.apply(&activate(SEQUENCE_ID, Duration::ZERO)),
            Err(Refused::NothingOffered),
            "there is nothing to schedule"
        );

        // Table 11's note: the node information is there even in phase D.
        let CmdData::SmartEnergyManagementPsData(rendered) = &data else {
            panic!("expected the function");
        };
        let node = rendered
            .node_schedule_information
            .as_ref()
            .expect("always set");
        assert_eq!(node.node_remote_controllable, Some(true));
        assert_eq!(node.supports_reselection, Some(false));
        assert_eq!(node.alternatives_count, Some(0));

        compressor.offer_again();
        assert!(!is_absent(&compressor.data()));
    }

    /// A write addressed to somebody else's sequence changes nothing.
    #[test]
    fn a_write_to_an_unknown_sequence_is_refused() {
        let mut compressor = an_offer();
        assert_eq!(
            compressor.apply(&stop(PowerSequenceId(9))),
            Err(Refused::UnknownSequence)
        );
        assert_eq!(compressor.state(), PowerSequenceState::Inactive);
    }

    /// Table 11 fixes what the wire carries; a client filters on these.
    #[test]
    fn the_wire_says_watts_and_one_slot() {
        let CmdData::SmartEnergyManagementPsData(data) = an_offer().data() else {
            panic!("expected the function");
        };
        let alternatives = data.alternatives.as_ref().expect("one alternative");
        assert_eq!(alternatives.len(), 1, "[OHPCF-013]");
        let sequence = &alternatives[0].power_sequence.as_ref().unwrap()[0];
        assert_eq!(
            sequence.description.as_ref().unwrap().power_unit,
            Some(UnitOfMeasurement::W)
        );
        assert_eq!(
            sequence
                .state
                .as_ref()
                .unwrap()
                .sequence_remote_controllable,
            Some(true)
        );
        let slots = sequence.power_time_slot.as_ref().unwrap();
        assert_eq!(slots.len(), 1, "single-slot scheduling only");
        assert!(
            slots[0]
                .schedule
                .as_ref()
                .unwrap()
                .default_duration
                .is_none(),
            "[OHPCF-011/4]: the duration is unknown"
        );
    }

    /// `activeSlotNumber` is present only while the process runs or is paused (Table 11).
    #[test]
    fn the_active_slot_appears_only_while_the_process_is_under_way() {
        let slot = |flexibility: &Flexibility| {
            let CmdData::SmartEnergyManagementPsData(data) = flexibility.data() else {
                panic!("expected the function");
            };
            data.alternatives.as_ref().unwrap()[0]
                .power_sequence
                .as_ref()
                .unwrap()[0]
                .state
                .as_ref()
                .unwrap()
                .active_slot_number
        };

        let mut compressor = an_offer();
        assert_eq!(slot(&compressor), None, "inactive");
        compressor
            .apply(&activate(SEQUENCE_ID, Duration::ZERO))
            .unwrap();
        assert_eq!(slot(&compressor), None, "scheduled but not started");
        compressor.start();
        assert_eq!(slot(&compressor), Some(SLOT_NUMBER));
        compressor.apply(&pause(SEQUENCE_ID)).unwrap();
        assert_eq!(slot(&compressor), Some(SLOT_NUMBER));
        compressor.complete();
        assert_eq!(slot(&compressor), None);
    }

    /// Both actors implement both scenarios (Table 1).
    #[test]
    fn both_actors_implement_both_scenarios() {
        for descriptor in [&COMPRESSOR, &CEM] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.version, "1.0.0");
            assert_eq!(descriptor.required_scenarios().collect::<Vec<_>>(), [1, 2]);
        }
        assert_eq!(COMPRESSOR.role, ActorRole::Server);
        assert_eq!(CEM.role, ActorRole::Client);
        assert!(COMPRESSOR.permits_entity(&EntityType::Compressor));
        assert!(
            !COMPRESSOR.permits_entity(&EntityType::HeatPumpAppliance),
            "§3.2.2.1 puts the actor on the compressor, not on the appliance around it"
        );
        assert!(CEM.permits_entity(&EntityType::CEM));

        // The CEM writes, so it binds first.
        let binding: Vec<_> = CEM.features_needing_binding().collect();
        assert_eq!(binding, [&FeatureType::SmartEnergyManagementPs]);
    }
}
