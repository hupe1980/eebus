//! The machinery shared by the two use cases that curtail a car's charging current.
//!
//! Overload Protection ([`opev`](crate::usecases::emobility::opev)) and Optimization of
//! Self-Consumption ([`oscev`](crate::usecases::emobility::oscev)) are the same exchange
//! for opposite reasons. Both give a car a per-phase current ceiling in amperes, both
//! watch a four-second heartbeat, both fall back to a safe current the moment the manager
//! goes quiet, and both carry an error state that means what a missing heartbeat means.
//! Their technical specifications are structurally identical, down to the scenario
//! numbers and the tables.
//!
//! Two things differ, and both are what the car is *told about the number* rather than
//! what it does with it — see [`Purpose`]:
//!
//! | | OPEV | OSCEV |
//! |---|---|---|
//! | `limitCategory` | `obligation` — a fuse | `recommendation` — an opportunity |
//! | `scopeType` | `overloadProtection` | `selfConsumption` |
//!
//! That difference is not cosmetic to the car: an obligation must be obeyed, and a
//! recommendation is the manager saying "there is free solar power right now, take it if
//! you like". Everything else is one implementation, so a fix to one is a fix to both.
//!
//! Both are the EV counterpart of [LPC](crate::usecases::lpc), and the differences from
//! it are what make them separate use cases rather than a parameter:
//!
//! | | LPC | OPEV / OSCEV |
//! |---|---|---|
//! | What is limited | total active power, in watts | charging current, **per phase**, in amperes |
//! | Heartbeat timeout | 120 s | **4 s** ([OPEV-005], [OSCEV-005]) |
//! | On losing the peer | the failsafe power limit, for hours | a safe current, immediately |
//! | Who decides | a state machine with five states | the car, from three inputs |
//!
//! Four seconds because a car follows its pilot signal at once and the fuse does not wait,
//! where a heat pump's compressor needs minutes. Per phase because a car charging
//! asymmetrically ([OPEV-002]) takes 16 A where there is room and 6 A where there is not.
//!
//! ```
//! use core::time::Duration;
//! use eebus::model::ElectricalConnectionPhaseName as Phase;
//! use eebus::usecases::emobility::charging::{ChargingCurrents, EvCharging};
//!
//! // A car that will fall back to 6 A per phase if left alone.
//! let mut ev = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
//! assert_eq!(ev.effective(Phase::A), Some(6.0), "nothing heard from yet");
//!
//! // The manager is alive and allows 16 A on one phase, 10 A on the others.
//! let now = Duration::from_secs(1);
//! ev.on_heartbeat(now);
//! ev.on_limit(ChargingCurrents::new(16.0, 10.0, 10.0), now);
//! assert_eq!(ev.effective(Phase::A), Some(16.0));
//!
//! // Four seconds of silence and the safe current is back. [OPEV-005]
//! ev.handle_timeout(now + Duration::from_secs(5));
//! assert_eq!(ev.effective(Phase::A), Some(6.0));
//! assert!(ev.source().is_safe_fallback());
//! ```

use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use crate::model::{
    CmdData, ElectricalConnectionId, ElectricalConnectionParameterDescriptionData,
    ElectricalConnectionParameterDescriptionListData, ElectricalConnectionParameterId,
    ElectricalConnectionPermittedValueSetData, ElectricalConnectionPermittedValueSetListData,
    ElectricalConnectionPhaseName as Phase, EntityType, FeatureType, Function, LoadControlCategory,
    LoadControlLimitData, LoadControlLimitDescriptionData, LoadControlLimitDescriptionListData,
    LoadControlLimitId, LoadControlLimitListData, LoadControlLimitType, MeasurementId, Role,
    ScaledNumber, ScaledNumberRange, ScaledNumberSet, ScopeType, UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::addressing::ParameterIds;
use crate::usecases::descriptor::FunctionUse;

use super::{actors, names};

/// Which of the two current-curtailment use cases an actor is playing.
///
/// The state machine does not depend on this — a ceiling is a number of amperes either
/// way — but what the car is *told* about the number does, and that changes what a
/// well-behaved car does with it: an obligation is a fuse it must not blow, and a
/// recommendation is free solar power it may decline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// Overload Protection: the supply cannot carry more ([OPEV-001]).
    OverloadProtection,
    /// Optimization of Self-Consumption: there is self-produced power going spare
    /// ([OSCEV-001]).
    SelfConsumption,
}

impl Purpose {
    /// The `limitCategory` of the limit description (OPEV/OSCEV Table 6).
    ///
    /// The one element a car must not misread. `obligation` is a ceiling it may not
    /// exceed; `recommendation` is an offer, and a car that treats the second as the first
    /// will stop charging when the sun goes in.
    pub const fn limit_category(self) -> LoadControlCategory {
        match self {
            Self::OverloadProtection => LoadControlCategory::Obligation,
            Self::SelfConsumption => LoadControlCategory::Recommendation,
        }
    }

    /// The `scopeType` of the limit description.
    pub const fn scope_type(self) -> ScopeType {
        match self {
            Self::OverloadProtection => ScopeType::OverloadProtection,
            Self::SelfConsumption => ScopeType::SelfConsumption,
        }
    }

    /// The use-case name a device announces in `nodeManagementUseCaseData`.
    pub const fn use_case_name(self) -> &'static str {
        match self {
            Self::OverloadProtection => names::OPEV,
            Self::SelfConsumption => names::OSCEV,
        }
    }

    /// The prefix runtime signals are named with: `opev` or `oscev`.
    pub const fn signal_prefix(self) -> &'static str {
        match self {
            Self::OverloadProtection => "opev",
            Self::SelfConsumption => "oscev",
        }
    }

    /// The name of scenario 1, as the specification writes it.
    pub const fn scenario_one_name(self) -> &'static str {
        match self {
            Self::OverloadProtection => "Energy Guard curtails charging current of EV",
            Self::SelfConsumption => "CEM informs EV about self-produced current",
        }
    }
}

/// The version this implementation speaks.
///
/// OPEV 1.0.1b and OSCEV 1.0.1b both carry `1.0.1` as their `useCaseVersion`; the `b` is a
/// document sub-revision, which is why the descriptors state it separately.
pub const VERSION: &str = "1.0.1";

/// How long the car waits before falling back to its safe current ([OPEV-005]).
///
/// Four seconds, where LPC allows a hundred and twenty. A charging current is followed
/// immediately, so the protection can be that tight — and the fuse it protects does not
/// wait either.
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(4);

/// The actor names an energy manager may use for this use case (§3.2.2.1).
///
/// `CEM` for something that manages energy, `EnergyGuard` for something that only guards.
/// A car looking for a counterpart accepts either, because the specification lets the
/// manager choose.
pub const ENERGY_GUARD_ACTORS: &[&str] = &[actors::CEM, actors::ENERGY_GUARD];

/// How many decimal places a charging current keeps on the wire.
///
/// One: a pilot signal resolves to about a tenth of an ampere, and 6.0 A is not 6 A by
/// accident — the minimum a car may draw is exact.
const DECIMALS: u8 = 1;

/// The three phases, in the order the limits are numbered.
pub const PHASES: [Phase; 3] = [Phase::A, Phase::B, Phase::C];

/// The `limitId` **this** implementation gives each phase's current limit.
///
/// `<x1>`, `<x2>`, `<x3>` of Table 6, which is to say a placeholder: the number is the
/// car's to choose. This is what [`EvActor`] publishes for itself and **no peer's
/// numbering**. A manager finds a car's with [`PhaseLimits`], because curtailing phase A
/// by writing to `limitId` 1 on a car that numbers them differently limits a phase the
/// supply was not worried about and leaves the one that was at full current — which is
/// the fuse this use case exists to protect.
pub fn limit_id(phase: Phase) -> Option<LoadControlLimitId> {
    Some(LoadControlLimitId(match phase {
        Phase::A => 1,
        Phase::B => 2,
        Phase::C => 3,
        _ => return None,
    }))
}

/// Which `limitId` a car keeps each phase's current limit under.
///
/// Two functions say it between them and neither says it alone. The limit descriptions
/// (Table 6) give `limitId` and the `measurementId` it points at; the parameter
/// descriptions (Table 8) give that `measurementId` a phase. A manager that skipped the
/// correlation and used its own numbering would be writing to real limits of the car's —
/// acknowledged, applied, and on the wrong phase.
///
/// Feed it both description payloads, in either order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhaseLimits {
    /// What each parameter describes, which is where a `measurementId` gets its phase.
    parameters: ParameterIds,
    /// `limitId` to `measurementId`, from the limit descriptions of this purpose.
    limits: Vec<(LoadControlLimitId, MeasurementId)>,
}

impl PhaseLimits {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The numbering **this** implementation publishes, for a device describing itself.
    ///
    /// [`EvActor`] uses it to write its own `loadControlLimitListData`. A manager must
    /// not: the car on the other end chose its own, and [`learn`](Self::learn) is how it
    /// is found.
    pub fn own(phases: &[Phase]) -> Self {
        let mut limits = Self::new();
        limits.learn(
            &parameter_descriptions(ELECTRICAL_CONNECTION, phases),
            Purpose::OverloadProtection,
        );
        for phase in phases {
            let Some(id) = limit_id(phase.clone()) else {
                continue;
            };
            limits.limits.push((id, MeasurementId(id.get())));
        }
        limits
    }

    /// Learns what one description payload says, and reports whether it was one.
    ///
    /// `purpose` is what keeps the two use cases apart: a car curtailed for overload
    /// protection *and* offered surplus publishes two limits per phase, an `obligation`
    /// and a `recommendation`, and writing a recommendation where an obligation was meant
    /// tells the car it may ignore the fuse.
    pub fn learn(&mut self, data: &CmdData, purpose: Purpose) -> bool {
        match data {
            CmdData::LoadControlLimitDescriptionListData(list) => {
                for entry in list.load_control_limit_description_data.iter().flatten() {
                    let (Some(limit), Some(measurement)) = (entry.limit_id, entry.measurement_id)
                    else {
                        continue;
                    };
                    if entry.limit_type != Some(LoadControlLimitType::MaxValueLimit)
                        || entry.limit_category != Some(purpose.limit_category())
                        || entry.scope_type != Some(purpose.scope_type())
                    {
                        continue;
                    }
                    match self.limits.iter_mut().find(|(id, _)| *id == limit) {
                        Some((_, stored)) => *stored = measurement,
                        None => self.limits.push((limit, measurement)),
                    }
                }
                true
            }
            _ => self.parameters.learn(data),
        }
    }

    /// What this car said each of its electrical-connection parameters describes.
    pub fn parameters(&self) -> &ParameterIds {
        &self.parameters
    }

    /// The car's `limitId` for one phase, once both descriptions have arrived.
    pub fn limit_for(&self, phase: &Phase) -> Option<LoadControlLimitId> {
        self.limits
            .iter()
            .find(|(_, measurement)| {
                self.parameters.phase_of_measurement(*measurement) == Some(phase)
            })
            .map(|(limit, _)| *limit)
    }

    /// The phases this car publishes a limit for, in the order the limits are numbered.
    pub fn phases(&self) -> Vec<Phase> {
        let mut found: Vec<(LoadControlLimitId, Phase)> = PHASES
            .iter()
            .filter_map(|phase| Some((self.limit_for(phase)?, phase.clone())))
            .collect();
        found.sort_by_key(|(id, _)| id.get());
        found.into_iter().map(|(_, phase)| phase).collect()
    }

    /// Whether any phase can be addressed at all.
    pub fn is_known(&self) -> bool {
        PHASES.iter().any(|phase| self.limit_for(phase).is_some())
    }
}

/// The phase a `limitId` belongs to.
pub fn phase_of(id: LoadControlLimitId) -> Option<Phase> {
    match id.get() {
        1 => Some(Phase::A),
        2 => Some(Phase::B),
        3 => Some(Phase::C),
        _ => None,
    }
}

/// A charging current per phase, in amperes.
///
/// A car that cannot charge asymmetrically uses [`same`](Self::same) and the Energy Guard
/// writes the same value to all three; a car that can takes three different ones
/// ([OPEV-002]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChargingCurrents {
    /// Phase A, in amperes.
    pub a: Option<f64>,
    /// Phase B.
    pub b: Option<f64>,
    /// Phase C.
    pub c: Option<f64>,
}

impl ChargingCurrents {
    /// A current for each phase.
    pub fn new(a: f64, b: f64, c: f64) -> Self {
        Self {
            a: Some(a),
            b: Some(b),
            c: Some(c),
        }
    }

    /// The same current on every phase, which is what a symmetric charger takes.
    pub fn same(amperes: f64) -> Self {
        Self::new(amperes, amperes, amperes)
    }

    /// A current on one phase only, for a single-phase connection.
    pub fn single(phase: Phase, amperes: f64) -> Self {
        let mut currents = Self::default();
        currents.set(phase, Some(amperes));
        currents
    }

    /// The current on one phase.
    pub fn get(&self, phase: Phase) -> Option<f64> {
        self.get_ref(&phase)
    }

    /// The current on one phase, without taking ownership of the name.
    pub fn get_ref(&self, phase: &Phase) -> Option<f64> {
        match phase {
            Phase::A => self.a,
            Phase::B => self.b,
            Phase::C => self.c,
            _ => None,
        }
    }

    /// Sets the current on one phase.
    pub fn set(&mut self, phase: Phase, amperes: Option<f64>) {
        match &phase {
            Phase::A => self.a = amperes,
            Phase::B => self.b = amperes,
            Phase::C => self.c = amperes,
            _ => {}
        }
    }

    /// The smallest current set, which is what a symmetric charger is held to.
    pub fn smallest(&self) -> Option<f64> {
        [self.a, self.b, self.c]
            .into_iter()
            .flatten()
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| a.min(v)))
            })
    }

    /// The phases with a current set.
    pub fn phases(&self) -> impl Iterator<Item = (Phase, f64)> + '_ {
        PHASES
            .iter()
            .filter_map(|phase| self.get_ref(phase).map(|amps| (phase.clone(), amps)))
    }

    /// Every current, clamped into `[min, max]`.
    ///
    /// A car has an electrical minimum below which it cannot charge at all and a maximum
    /// the cable and the wallbox impose; a limit outside that band is one it cannot
    /// follow, and clamping is what it does instead of stopping.
    #[must_use]
    /// Holds every current inside what the car said it can take.
    ///
    /// Each phase is clamped to **its own** band. A phase the car published no band for
    /// falls back to [`ChargingBand::narrowest`] — the band permitted everywhere — because
    /// a value that is safe on every described phase is the most that can be justified
    /// about an undescribed one. With nothing described at all the currents are left as
    /// they are, and the car applies its own limits.
    pub fn clamped(mut self, band: &ChargingBand) -> Self {
        for phase in PHASES {
            let Some(value) = self.get_ref(&phase) else {
                continue;
            };
            let Some((min, max)) = band.for_phase(&phase).or_else(|| band.narrowest()) else {
                continue;
            };
            self.set(phase, Some(value.clamp(min, max)));
        }
        self
    }
}

// ---- the feature tables both use cases share ------------------------------------
//
// OPEV Table 5 and OSCEV Table 5 are the same table. What differs is the use-case name
// the descriptors carry, which is why the descriptors live in the two modules and the
// tables live here.

pub(super) const EV_ENTITIES: &[EntityType] = &[EntityType::EV];
pub(super) const GUARD_ENTITIES: &[EntityType] = &[EntityType::CEM];

pub(super) const EV_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::LoadControl,
        Function::LoadControlLimitDescriptionListData,
    ),
    FunctionUse::server_writeable(FeatureType::LoadControl, Function::LoadControlLimitListData),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionPermittedValueSetListData,
    ),
];

pub(super) const EV_WATCHES: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::DeviceDiagnosis,
        Function::DeviceDiagnosisHeartbeatData,
    ),
    FunctionUse::client(
        FeatureType::DeviceDiagnosis,
        Function::DeviceDiagnosisStateData,
    ),
];

pub(super) const GUARD_WRITES: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::LoadControl,
        Function::LoadControlLimitDescriptionListData,
    ),
    FunctionUse::client_writes(FeatureType::LoadControl, Function::LoadControlLimitListData),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionPermittedValueSetListData,
    ),
];

pub(super) const GUARD_SERVES: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::DeviceDiagnosis,
        Function::DeviceDiagnosisHeartbeatData,
    ),
    FunctionUse::server(
        FeatureType::DeviceDiagnosis,
        Function::DeviceDiagnosisStateData,
    ),
];

// ---- what the car publishes ----------------------------------------------------

/// Builds the `LoadControl` feature a car serves (Table 5).
///
/// Writes are deferred: whether a current can be followed is the car's decision, and
/// nothing else can make it.
pub fn load_control_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::LoadControl, Role::Server)
        .with_deferred_writes()
        .with_function(
            Function::LoadControlLimitDescriptionListData,
            Operations::read(),
        )
        .with_function(Function::LoadControlLimitListData, Operations::read_write())
}

/// Builds the `ElectricalConnection` feature that says what the car can take (Table 5).
pub fn electrical_connection_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::ElectricalConnection, Role::Server)
        .with_function(
            Function::ElectricalConnectionParameterDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::ElectricalConnectionPermittedValueSetListData,
            Operations::read(),
        )
}

/// The limit descriptions a car publishes, one per phase (Table 6).
///
/// `maxValueLimit` in amperes either way: the number written is a ceiling on current, not
/// a power and not a target. What [`Purpose`] decides is the pair a car reads to know
/// whether it *must* stay below it — `obligation` and `overloadProtection` — or *may* —
/// `recommendation` and `selfConsumption`.
pub fn limit_descriptions(purpose: Purpose, phases: &[Phase]) -> CmdData {
    CmdData::LoadControlLimitDescriptionListData(LoadControlLimitDescriptionListData {
        load_control_limit_description_data: Some(
            phases
                .iter()
                .filter_map(|phase| {
                    let id = limit_id(phase.clone())?;
                    Some(LoadControlLimitDescriptionData {
                        limit_id: Some(id),
                        limit_type: Some(LoadControlLimitType::MaxValueLimit),
                        limit_category: Some(purpose.limit_category()),
                        // The foreign identifier that ties this limit to the measurement
                        // of the same phase, where the car publishes one.
                        measurement_id: Some(MeasurementId(id.get())),
                        unit: Some(UnitOfMeasurement::A),
                        scope_type: Some(purpose.scope_type()),
                        ..Default::default()
                    })
                })
                .collect(),
        ),
    })
}

/// The current limits, as `loadControlLimitListData` (Table 7).
///
/// `limits` says which `limitId` the car keeps each phase under. A phase the car
/// published no limit for is left out rather than guessed at.
pub fn limit_data(currents: &ChargingCurrents, active: bool, limits: &PhaseLimits) -> CmdData {
    CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(
            currents
                .phases()
                .filter_map(|(phase, amperes)| {
                    Some(LoadControlLimitData {
                        limit_id: Some(limits.limit_for(&phase)?),
                        is_limit_changeable: Some(true),
                        is_limit_active: Some(active),
                        value: Some(ScaledNumber::from_f64(amperes, DECIMALS)),
                        ..Default::default()
                    })
                })
                .collect(),
        ),
    })
}

/// The limits deactivated on every named phase — "no curtailment is needed"
/// ([OPEV-004]).
///
/// Not an empty list: an empty `loadControlLimitListData` says nothing at all, and the
/// specification asks a manager to *tell* the car that no limit applies rather than leave
/// it to infer that from silence — which is what a lost heartbeat already means. Table 7
/// makes the value of a deactivated limit ignorable, so the phases are named and the
/// numbers are not.
pub fn deactivated(phases: &[Phase], limits: &PhaseLimits) -> CmdData {
    CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(
            phases
                .iter()
                .filter_map(|phase| {
                    Some(LoadControlLimitData {
                        limit_id: Some(limits.limit_for(phase)?),
                        is_limit_changeable: Some(true),
                        is_limit_active: Some(false),
                        ..Default::default()
                    })
                })
                .collect(),
        ),
    })
}

/// The phases a car said it charges on, from its parameter descriptions (Table 8).
pub fn read_phases(data: &CmdData) -> Option<Vec<Phase>> {
    let CmdData::ElectricalConnectionParameterDescriptionListData(list) = data else {
        return None;
    };
    let phases: Vec<Phase> = list
        .electrical_connection_parameter_description_data
        .iter()
        .flatten()
        .filter_map(|entry| entry.ac_measured_phases.clone())
        .collect();
    (!phases.is_empty()).then_some(phases)
}

/// The parameter descriptions binding each phase to its limit (Table 8).
pub fn parameter_descriptions(connection: u32, phases: &[Phase]) -> CmdData {
    CmdData::ElectricalConnectionParameterDescriptionListData(
        ElectricalConnectionParameterDescriptionListData {
            electrical_connection_parameter_description_data: Some(
                phases
                    .iter()
                    .filter_map(|phase| {
                        let id = limit_id(phase.clone())?;
                        Some(ElectricalConnectionParameterDescriptionData {
                            electrical_connection_id: Some(ElectricalConnectionId(connection)),
                            parameter_id: Some(ElectricalConnectionParameterId(id.get())),
                            measurement_id: Some(MeasurementId(id.get())),
                            ac_measured_phases: Some(phase.clone()),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
        },
    )
}

/// The band a car can charge in, per phase, in amperes (Table 9).
///
/// Per phase, and not one band for the car, because Table 9's permitted value sets are
/// addressed by `parameterId` and a car publishes one per phase. Reading the *first* set in
/// the list takes whichever parameter the car happened to describe first, so a car that also
/// publishes a power parameter has its charging currents clamped to a range in watts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChargingBand {
    per_phase: Vec<(Phase, (f64, f64))>,
}

impl ChargingBand {
    /// An empty band: the car has said nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same band on all three phases, for a car that charges symmetrically.
    pub fn uniform(min: f64, max: f64) -> Self {
        PHASES
            .into_iter()
            .fold(Self::new(), |band, phase| band.with(phase, min, max))
    }

    /// Records one phase's band.
    #[must_use]
    pub fn with(mut self, phase: Phase, min: f64, max: f64) -> Self {
        let band = (min.min(max), max.max(min));
        match self.per_phase.iter_mut().find(|(known, _)| known == &phase) {
            Some((_, stored)) => *stored = band,
            None => self.per_phase.push((phase, band)),
        }
        self
    }

    /// The band on one phase, where the car published one.
    pub fn for_phase(&self, phase: &Phase) -> Option<(f64, f64)> {
        self.per_phase
            .iter()
            .find(|(known, _)| known == phase)
            .map(|(_, band)| *band)
    }

    /// The band that is permitted on **every** phase the car published one for.
    ///
    /// The highest minimum and the lowest maximum. What a caller wants for a single
    /// number to show a user, and what a phase with no band of its own falls back to.
    pub fn narrowest(&self) -> Option<(f64, f64)> {
        let mut bands = self.per_phase.iter().map(|(_, band)| *band);
        let first = bands.next()?;
        Some(bands.fold(first, |(lo, hi), (min, max)| (lo.max(min), hi.min(max))))
    }

    /// The phases the car published a band for.
    pub fn phases(&self) -> impl Iterator<Item = &Phase> {
        self.per_phase.iter().map(|(phase, _)| phase)
    }

    /// Whether the car has published any band at all.
    pub fn is_empty(&self) -> bool {
        self.per_phase.is_empty()
    }
}

/// What the car can actually take, per phase (Table 9).
///
/// The Energy Guard needs this before it curtails anything: a limit below the minimum is
/// one the car cannot follow, and it will stop charging rather than draw less.
pub fn permitted_value_sets(connection: u32, min: f64, max: f64, phases: &[Phase]) -> CmdData {
    CmdData::ElectricalConnectionPermittedValueSetListData(
        ElectricalConnectionPermittedValueSetListData {
            electrical_connection_permitted_value_set_data: Some(
                phases
                    .iter()
                    .filter_map(|phase| {
                        let id = limit_id(phase.clone())?;
                        Some(ElectricalConnectionPermittedValueSetData {
                            electrical_connection_id: Some(ElectricalConnectionId(connection)),
                            parameter_id: Some(ElectricalConnectionParameterId(id.get())),
                            permitted_value_set: Some(vec![ScaledNumberSet {
                                range: Some(vec![ScaledNumberRange {
                                    min: Some(ScaledNumber::from_f64(min, DECIMALS)),
                                    max: Some(ScaledNumber::from_f64(max, DECIMALS)),
                                }]),
                                ..Default::default()
                            }]),
                        })
                    })
                    .collect(),
            ),
        },
    )
}

/// Reads a `loadControlLimitListData` payload as a set of per-phase currents.
///
/// A limit whose `isLimitActive` is `false` contributes nothing, which is Table 7's rule:
/// the value of a deactivated limit is to be ignored.
///
/// **Give this the resolved state, not a partial update.** The result describes every
/// phase, so a fragment naming one phase would read as "the other two are unlimited".
/// Pass [`WriteRequest::resolved`](crate::spine::WriteRequest::resolved); use
/// [`data`](crate::spine::WriteRequest::data) only to see which phases were addressed.
pub fn read_limit_write(data: &CmdData) -> Option<ChargingCurrents> {
    let CmdData::LoadControlLimitListData(list) = data else {
        return None;
    };
    let mut currents = ChargingCurrents::default();
    let mut saw_one = false;
    for entry in list.load_control_limit_data.iter().flatten() {
        let Some(phase) = entry.limit_id.and_then(phase_of) else {
            continue;
        };
        saw_one = true;
        if entry.is_limit_active == Some(false) {
            currents.set(phase, None);
            continue;
        }
        // A `value` that is present but unreadable makes the write unusable: reading it
        // as "no limit on this phase" would lift a curtailment rather than apply one.
        let amps = match entry.value.as_ref() {
            Some(value) => Some(value.to_f64()?),
            None => None,
        };
        currents.set(phase, amps);
    }
    saw_one.then_some(currents)
}

/// Reads the range a car said it can charge in, per phase (Table 9).
pub fn read_permitted_range(data: &CmdData, parameters: &ParameterIds) -> Option<ChargingBand> {
    let CmdData::ElectricalConnectionPermittedValueSetListData(list) = data else {
        return None;
    };
    let mut band = ChargingBand::new();
    for entry in list
        .electrical_connection_permitted_value_set_data
        .iter()
        .flatten()
    {
        // Which phase this set is for is in the *parameter* description, never here.
        let Some(id) = entry.parameter_id else {
            continue;
        };
        let Some(phase) = parameters
            .all()
            .find(|known| known.parameter == id)
            .and_then(|known| known.phases.clone())
        else {
            continue;
        };
        let Some(range) = entry
            .permitted_value_set
            .iter()
            .flatten()
            .filter_map(|set| set.range.as_ref())
            .flatten()
            .next()
        else {
            continue;
        };
        let (Some(min), Some(max)) = (
            range.min.as_ref().and_then(ScaledNumber::to_f64),
            range.max.as_ref().and_then(ScaledNumber::to_f64),
        ) else {
            continue;
        };
        band = band.with(phase, min, max);
    }
    (!band.is_empty()).then_some(band)
}

// ---- the car's decision ---------------------------------------------------------

/// Why the car is charging at the current it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurrentSource {
    /// Nothing has been heard from the Energy Guard yet.
    NotYetCurtailed,
    /// The Energy Guard's limit applies.
    Curtailed,
    /// The Energy Guard has gone quiet for longer than four seconds ([OPEV-005]).
    GuardSilent,
    /// The Energy Guard has announced a failure ([OPEV-007]).
    GuardFailed,
}

impl CurrentSource {
    /// Whether the car is on its own safe current rather than the manager's.
    pub fn is_safe_fallback(self) -> bool {
        !matches!(self, CurrentSource::Curtailed)
    }

    /// The name a debug interface reports this under.
    pub const fn as_str(self) -> &'static str {
        match self {
            CurrentSource::NotYetCurtailed => "notYetCurtailed",
            CurrentSource::Curtailed => "curtailed",
            CurrentSource::GuardSilent => "guardSilent",
            CurrentSource::GuardFailed => "guardFailed",
        }
    }
}

impl core::fmt::Display for CurrentSource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The car's side of the use case: what current to charge at, and why.
///
/// Three inputs and one output. The inputs are the Energy Guard's limit, its heartbeat,
/// and its operating state; the output is a current per phase. Two of the three inputs
/// are about whether to believe the first, and both of them fail *safe*: the car goes to
/// a current it knows cannot overload anything, not to the last one it was told.
#[derive(Clone, Debug)]
pub struct EvCharging {
    safe: ChargingCurrents,
    limit: Option<ChargingCurrents>,
    /// The electrical band the car can actually charge in, if it has said.
    range: Option<(f64, f64)>,
    last_heartbeat: Option<Duration>,
    guard_failed: bool,
    timeout: Duration,
    /// The clock, advanced by every input and by `handle_timeout`.
    ///
    /// Held rather than passed to each query, so that asking what the car is charging at
    /// cannot give an answer from a different moment than the one the caller means. The
    /// same shape as [`ControllableSystem`](crate::usecases::limitation::ControllableSystem).
    now: Duration,
}

impl EvCharging {
    /// A car that falls back to `safe` when the Energy Guard is not to be relied on.
    ///
    /// The safe current is the one that cannot overload the supply no matter what else is
    /// drawing — typically the smallest a car may charge at, which is 6 A for type 2.
    pub fn new(safe: ChargingCurrents, now: Duration) -> Self {
        Self {
            safe,
            limit: None,
            range: None,
            last_heartbeat: None,
            guard_failed: false,
            timeout: HEARTBEAT_TIMEOUT,
            now,
        }
    }

    /// Uses a heartbeat timeout other than four seconds.
    ///
    /// [OPEV-005] makes four seconds the *maximum*; a car on a supply with less headroom
    /// may watch more closely.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.min(HEARTBEAT_TIMEOUT);
        self
    }

    /// Declares the band this car can charge in, which also clamps what it accepts.
    #[must_use]
    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.range = Some((min, max));
        self
    }

    /// The safe current this car falls back to.
    pub fn safe_current(&self) -> ChargingCurrents {
        self.safe
    }

    /// The last curtailment the Energy Guard wrote, whether or not it is in force.
    ///
    /// What the car *publishes* on `LoadControl`, because that function is a record of
    /// what the manager set — not of what the car happens to be charging at. The two
    /// differ exactly when the manager is not to be relied on, and `isLimitActive` is
    /// what says so.
    pub fn limit(&self) -> Option<ChargingCurrents> {
        self.limit
    }

    /// When the last heartbeat from the manager arrived, if one has.
    pub fn last_heartbeat(&self) -> Option<Duration> {
        self.last_heartbeat
    }

    /// Records a heartbeat from the Energy Guard ([OPEV-006]).
    pub fn on_heartbeat(&mut self, now: Duration) {
        self.now = self.now.max(now);
        self.last_heartbeat = Some(now);
    }

    /// Records the Energy Guard's operating state ([OPEV-007]).
    ///
    /// A manager in failure is one whose curtailment cannot be relied on, and the car
    /// treats that exactly as it treats silence.
    pub fn on_guard_state(&mut self, failed: bool) {
        self.guard_failed = failed;
    }

    /// Applies a curtailment ([OPEV-001], [OPEV-003]).
    ///
    /// Returns whether it was accepted. A limit is refused only when it names no phase
    /// this car charges on; one outside the car's electrical band is *clamped* rather
    /// than refused, because a car that stops charging is worse for the fuse than a car
    /// charging at its minimum, and the Energy Guard learns the band from the permitted
    /// value sets anyway.
    pub fn on_limit(&mut self, currents: ChargingCurrents, now: Duration) -> bool {
        self.now = self.now.max(now);
        if currents.phases().next().is_none() {
            // A write that deactivates every phase is the Energy Guard saying no
            // curtailment is needed ([OPEV-004]), which is not a refusal.
            self.limit = Some(currents);
            self.last_heartbeat.get_or_insert(now);
            return true;
        }
        let currents = match self.range {
            // This car's own band, which it knows without being told.
            Some((min, max)) => currents.clamped(&ChargingBand::uniform(min, max)),
            None => currents,
        };
        self.limit = Some(currents);
        true
    }

    /// Advances the four-second watchdog.
    ///
    /// Call it at or after the instant [`poll_timeout`](Self::poll_timeout) reported; it
    /// is what turns silence into a fallback.
    pub fn handle_timeout(&mut self, now: Duration) {
        self.now = self.now.max(now);
    }

    /// The clock this car is working from.
    pub fn now(&self) -> Duration {
        self.now
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.last_heartbeat.map(|last| last + self.timeout)
    }

    /// Why the car is charging at the current it is.
    pub fn source(&self) -> CurrentSource {
        if self.guard_failed {
            return CurrentSource::GuardFailed;
        }
        match self.last_heartbeat {
            None => CurrentSource::NotYetCurtailed,
            // `>=`: the deadline `poll_timeout` publishes is `last + timeout`, and
            // waiting until exactly that instant has to be enough.
            Some(last) if self.now.saturating_sub(last) >= self.timeout => {
                CurrentSource::GuardSilent
            }
            Some(_) if self.limit.is_some() => CurrentSource::Curtailed,
            Some(_) => CurrentSource::NotYetCurtailed,
        }
    }

    /// The current to charge at on one phase, in amperes.
    ///
    /// This is the number the pilot signal is set from; everything else in this type
    /// exists to compute it. [`None`] means this phase is not curtailed at all — the car
    /// may draw whatever its own electrics allow.
    pub fn effective(&self, phase: Phase) -> Option<f64> {
        match self.source() {
            CurrentSource::Curtailed => self.limit.and_then(|limit| limit.get(phase)),
            _ => self.safe.get(phase),
        }
    }

    /// Every phase's current.
    pub fn effective_currents(&self) -> ChargingCurrents {
        let mut currents = ChargingCurrents::default();
        for phase in PHASES {
            let value = self.effective(phase.clone());
            currents.set(phase, value);
        }
        currents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// Table 6: a ceiling on current for the sake of a fuse, in amperes.
    /// A car whose phases are not numbered the way this crate numbers its own.
    ///
    /// Table 6's `<x1>`…`<x3>` are placeholders, so this is ordinary. The manager has to
    /// read the car's two description functions and compose them; using its own numbering
    /// would curtail phase B while leaving phase A — the one the fuse was worried
    /// about — at full current, and the car would acknowledge it.
    #[test]
    fn a_cars_phases_are_found_from_its_own_descriptions() {
        // This car: phase B on limit 1, phase A on 2, phase C on 3.
        let mut limits = PhaseLimits::new();
        limits.learn(
            &CmdData::LoadControlLimitDescriptionListData(LoadControlLimitDescriptionListData {
                load_control_limit_description_data: Some(vec![
                    described(1, 10, Purpose::OverloadProtection),
                    described(2, 20, Purpose::OverloadProtection),
                    described(3, 30, Purpose::OverloadProtection),
                ]),
            }),
            Purpose::OverloadProtection,
        );
        limits.learn(
            &CmdData::ElectricalConnectionParameterDescriptionListData(
                ElectricalConnectionParameterDescriptionListData {
                    electrical_connection_parameter_description_data: Some(vec![
                        parameter(10, Phase::B),
                        parameter(20, Phase::A),
                        parameter(30, Phase::C),
                    ]),
                },
            ),
            Purpose::OverloadProtection,
        );

        assert_eq!(limits.limit_for(&Phase::A), Some(LoadControlLimitId(2)));
        assert_eq!(limits.limit_for(&Phase::B), Some(LoadControlLimitId(1)));
        assert_eq!(limits.limit_for(&Phase::C), Some(LoadControlLimitId(3)));

        // And a write lands where the car keeps each phase.
        let currents = ChargingCurrents::single(Phase::A, 6.0);
        let CmdData::LoadControlLimitListData(list) = limit_data(&currents, true, &limits) else {
            panic!("expected the limits");
        };
        let entries = list.load_control_limit_data.as_ref().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].limit_id,
            Some(LoadControlLimitId(2)),
            "phase A is limit 2 on this car, not limit 1"
        );
    }

    /// A car offered surplus *and* held under the fuse publishes two limits per phase.
    ///
    /// Writing the obligation into the recommendation tells the car it may ignore the
    /// number, which is the difference between a protected supply and a tripped one.
    #[test]
    fn an_obligation_is_not_written_into_a_recommendation() {
        let descriptions =
            CmdData::LoadControlLimitDescriptionListData(LoadControlLimitDescriptionListData {
                load_control_limit_description_data: Some(vec![
                    described(1, 10, Purpose::SelfConsumption),
                    described(4, 10, Purpose::OverloadProtection),
                ]),
            });
        let parameters = CmdData::ElectricalConnectionParameterDescriptionListData(
            ElectricalConnectionParameterDescriptionListData {
                electrical_connection_parameter_description_data: Some(vec![parameter(
                    10,
                    Phase::A,
                )]),
            },
        );

        let mut obligation = PhaseLimits::new();
        obligation.learn(&descriptions, Purpose::OverloadProtection);
        obligation.learn(&parameters, Purpose::OverloadProtection);
        assert_eq!(
            obligation.limit_for(&Phase::A),
            Some(LoadControlLimitId(4)),
            "overload protection is the obligation"
        );

        let mut recommendation = PhaseLimits::new();
        recommendation.learn(&descriptions, Purpose::SelfConsumption);
        recommendation.learn(&parameters, Purpose::SelfConsumption);
        assert_eq!(
            recommendation.limit_for(&Phase::A),
            Some(LoadControlLimitId(1)),
            "self-consumption is the recommendation"
        );
    }

    /// Neither description is usable alone.
    #[test]
    fn a_phase_with_only_half_its_description_is_not_addressed() {
        let mut limits = PhaseLimits::new();
        limits.learn(
            &CmdData::LoadControlLimitDescriptionListData(LoadControlLimitDescriptionListData {
                load_control_limit_description_data: Some(vec![described(
                    1,
                    10,
                    Purpose::OverloadProtection,
                )]),
            }),
            Purpose::OverloadProtection,
        );
        assert!(!limits.is_known(), "no phase has been named yet");
        assert_eq!(limits.limit_for(&Phase::A), None);
    }

    fn described(
        limit: u32,
        measurement: u32,
        purpose: Purpose,
    ) -> LoadControlLimitDescriptionData {
        LoadControlLimitDescriptionData {
            limit_id: Some(LoadControlLimitId(limit)),
            limit_type: Some(LoadControlLimitType::MaxValueLimit),
            limit_category: Some(purpose.limit_category()),
            measurement_id: Some(MeasurementId(measurement)),
            unit: Some(UnitOfMeasurement::A),
            scope_type: Some(purpose.scope_type()),
            ..Default::default()
        }
    }

    fn parameter(measurement: u32, phase: Phase) -> ElectricalConnectionParameterDescriptionData {
        ElectricalConnectionParameterDescriptionData {
            electrical_connection_id: Some(ElectricalConnectionId(1)),
            parameter_id: Some(ElectricalConnectionParameterId(measurement)),
            measurement_id: Some(MeasurementId(measurement)),
            ac_measured_phases: Some(phase),
            ..Default::default()
        }
    }

    #[test]
    fn the_limit_description_says_amperes_and_overload_protection() {
        let CmdData::LoadControlLimitDescriptionListData(list) =
            limit_descriptions(Purpose::OverloadProtection, &PHASES)
        else {
            panic!("expected the descriptions");
        };
        let entries = list.load_control_limit_description_data.as_ref().unwrap();
        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert_eq!(entry.unit.as_ref(), Some(&UnitOfMeasurement::A));
            assert_eq!(
                entry.scope_type.as_ref(),
                Some(&ScopeType::OverloadProtection)
            );
            assert_eq!(
                entry.limit_type.as_ref(),
                Some(&LoadControlLimitType::MaxValueLimit)
            );
            assert_eq!(
                entry.limit_category.as_ref(),
                Some(&LoadControlCategory::Obligation)
            );
        }
        assert_eq!(entries[0].limit_id, limit_id(Phase::A));
        assert_eq!(entries[2].limit_id, limit_id(Phase::C));
    }

    /// [OPEV-002]: each phase carries its own limit, so a car can charge asymmetrically.
    #[test]
    fn opev_002_each_phase_is_limited_on_its_own() {
        let currents = ChargingCurrents::new(16.0, 6.0, 10.0);
        let written = limit_data(&currents, true, &PhaseLimits::own(&PHASES));
        let back = read_limit_write(&written).expect("the same function");
        assert_eq!(back, currents);
        assert_eq!(back.smallest(), Some(6.0), "what a symmetric charger takes");
    }

    /// Table 7: a deactivated limit's value is ignored.
    #[test]
    fn a_deactivated_limit_carries_no_current() {
        let written = limit_data(
            &ChargingCurrents::same(16.0),
            false,
            &PhaseLimits::own(&PHASES),
        );
        let back = read_limit_write(&written).expect("still the function");
        assert_eq!(back, ChargingCurrents::default(), "nothing is curtailed");
    }

    /// [OPEV-003]: nothing heard from the manager means the safe current.
    #[test]
    fn opev_003_a_car_starts_on_its_safe_current() {
        let ev = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
        assert_eq!(ev.source(), CurrentSource::NotYetCurtailed);
        assert_eq!(ev.effective(Phase::A), Some(6.0));
    }

    /// [OPEV-005]: four seconds of silence and the car is back on its safe current.
    #[test]
    fn opev_005_silence_for_four_seconds_falls_back() {
        let mut ev = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
        ev.on_heartbeat(secs(1));
        ev.on_limit(ChargingCurrents::same(16.0), secs(1));
        ev.handle_timeout(secs(2));
        assert_eq!(ev.source(), CurrentSource::Curtailed);
        assert_eq!(ev.effective(Phase::A), Some(16.0));

        // A moment before four seconds is still in time; four seconds exactly is not.
        //
        // The boundary belongs to silence, which is the convention everywhere in this
        // crate that a deadline is published — and here it is load-bearing rather than
        // tidy: `poll_timeout` answers `last + timeout`, so a caller that sleeps until
        // that instant is the shape sans-IO invites, and under the other rule it saw no
        // change, asked again, got the same instant back, and never fell back at all
        ev.handle_timeout(secs(4) + Duration::from_millis(999));
        assert_eq!(ev.source(), CurrentSource::Curtailed);
        ev.handle_timeout(ev.poll_timeout().expect("a published deadline"));
        assert_eq!(ev.source(), CurrentSource::GuardSilent);
        assert_eq!(ev.effective(Phase::A), Some(6.0));

        // And it comes back the moment the manager does.
        ev.on_heartbeat(secs(7));
        assert_eq!(ev.source(), CurrentSource::Curtailed);
        assert_eq!(ev.effective(Phase::A), Some(16.0));
    }

    /// [OPEV-007]: a manager in failure is treated exactly as a silent one.
    #[test]
    fn opev_007_a_failed_manager_is_not_trusted_even_while_it_beats() {
        let mut ev = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
        ev.on_heartbeat(secs(1));
        ev.on_limit(ChargingCurrents::same(16.0), secs(1));
        ev.handle_timeout(secs(2));
        assert_eq!(ev.effective(Phase::A), Some(16.0));

        ev.on_guard_state(true);
        assert_eq!(ev.source(), CurrentSource::GuardFailed);
        assert_eq!(
            ev.effective(Phase::A),
            Some(6.0),
            "the heartbeat is still arriving and is still not enough"
        );

        ev.on_guard_state(false);
        assert_eq!(ev.effective(Phase::A), Some(16.0));
    }

    /// [OPEV-004]: "no curtailment needed" is a message, not a silence.
    #[test]
    fn opev_004_deactivating_the_limit_is_not_the_same_as_going_quiet() {
        let mut ev = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
        ev.on_heartbeat(secs(1));
        assert!(ev.on_limit(ChargingCurrents::default(), secs(1)));
        ev.handle_timeout(secs(2));
        assert_eq!(ev.source(), CurrentSource::Curtailed);
        assert_eq!(
            ev.effective(Phase::A),
            None,
            "not curtailed at all, which is what the manager said"
        );
    }

    /// A limit outside the band the car declared is clamped, not refused.
    #[test]
    fn a_limit_below_the_minimum_becomes_the_minimum() {
        let mut ev =
            EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO).with_range(6.0, 16.0);
        ev.on_heartbeat(secs(1));
        ev.on_limit(ChargingCurrents::new(2.0, 32.0, 10.0), secs(1));
        assert_eq!(ev.effective(Phase::A), Some(6.0), "below the minimum");
        assert_eq!(ev.effective(Phase::B), Some(16.0), "above the maximum");
        assert_eq!(ev.effective(Phase::C), Some(10.0));
    }

    /// Table 9 round-trips, which is how the manager learns the band.
    #[test]
    fn the_permitted_range_round_trips() {
        let mut parameters = ParameterIds::new();
        parameters.learn(&parameter_descriptions(1, &PHASES));

        let published = permitted_value_sets(1, 6.0, 16.0, &PHASES);
        let band = read_permitted_range(&published, &parameters).expect("a band");
        for phase in PHASES {
            assert_eq!(band.for_phase(&phase), Some((6.0, 16.0)));
        }
        assert_eq!(band.narrowest(), Some((6.0, 16.0)));
    }

    /// The set that belongs to another quantity is not read as a charging band.
    ///
    /// A car that publishes a power parameter alongside its per-phase currents used to
    /// have whichever set came first taken as the band, so a manager could clamp amperes
    /// into a range in watts — and then write a "curtailment" of 11 000 A.
    #[test]
    fn a_permitted_set_for_another_parameter_is_not_the_charging_band() {
        let mut parameters = ParameterIds::new();
        // Parameter 1 is total AC power; the per-phase currents are 2, 3 and 4.
        parameters.learn(&CmdData::ElectricalConnectionParameterDescriptionListData(
            ElectricalConnectionParameterDescriptionListData {
                electrical_connection_parameter_description_data: Some(vec![
                    ElectricalConnectionParameterDescriptionData {
                        electrical_connection_id: Some(ElectricalConnectionId(1)),
                        parameter_id: Some(ElectricalConnectionParameterId(1)),
                        scope_type: Some(ScopeType::AcPowerTotal),
                        ..Default::default()
                    },
                    ElectricalConnectionParameterDescriptionData {
                        electrical_connection_id: Some(ElectricalConnectionId(1)),
                        parameter_id: Some(ElectricalConnectionParameterId(2)),
                        measurement_id: Some(MeasurementId(2)),
                        ac_measured_phases: Some(Phase::A),
                        scope_type: Some(ScopeType::AcCurrent),
                        ..Default::default()
                    },
                ]),
            },
        ));

        let published = CmdData::ElectricalConnectionPermittedValueSetListData(
            ElectricalConnectionPermittedValueSetListData {
                electrical_connection_permitted_value_set_data: Some(vec![
                    // The power band, first in the list.
                    ElectricalConnectionPermittedValueSetData {
                        electrical_connection_id: Some(ElectricalConnectionId(1)),
                        parameter_id: Some(ElectricalConnectionParameterId(1)),
                        permitted_value_set: Some(vec![ScaledNumberSet {
                            range: Some(vec![ScaledNumberRange {
                                min: Some(ScaledNumber::from_f64(1_400.0, 0)),
                                max: Some(ScaledNumber::from_f64(11_000.0, 0)),
                            }]),
                            ..Default::default()
                        }]),
                    },
                    ElectricalConnectionPermittedValueSetData {
                        electrical_connection_id: Some(ElectricalConnectionId(1)),
                        parameter_id: Some(ElectricalConnectionParameterId(2)),
                        permitted_value_set: Some(vec![ScaledNumberSet {
                            range: Some(vec![ScaledNumberRange {
                                min: Some(ScaledNumber::from_f64(6.0, 0)),
                                max: Some(ScaledNumber::from_f64(16.0, 0)),
                            }]),
                            ..Default::default()
                        }]),
                    },
                ]),
            },
        );

        let band = read_permitted_range(&published, &parameters).expect("a band");
        assert_eq!(
            band.for_phase(&Phase::A),
            Some((6.0, 16.0)),
            "amperes, not the watts on parameter 1"
        );
        assert_eq!(
            band.phases().count(),
            1,
            "the power parameter covers no phase, so it contributes no band"
        );
    }

    /// Table 8 binds each phase to its own parameter.
    #[test]
    fn each_phase_gets_its_own_parameter() {
        let CmdData::ElectricalConnectionParameterDescriptionListData(list) =
            parameter_descriptions(1, &PHASES)
        else {
            panic!("expected the parameter descriptions");
        };
        let entries = list
            .electrical_connection_parameter_description_data
            .as_ref()
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].ac_measured_phases.as_ref(), Some(&Phase::A));
        assert_eq!(
            entries[0].parameter_id,
            Some(ElectricalConnectionParameterId(1))
        );
        assert_eq!(entries[2].ac_measured_phases.as_ref(), Some(&Phase::C));
    }

    /// The watchdog says when it next needs attention.
    #[test]
    fn the_timeout_is_reported_so_a_caller_can_schedule_it() {
        let mut ev = EvCharging::new(ChargingCurrents::same(6.0), Duration::ZERO);
        assert_eq!(ev.poll_timeout(), None, "nothing to watch yet");
        ev.on_heartbeat(secs(10));
        assert_eq!(ev.poll_timeout(), Some(secs(14)));
    }
}

// ---- the two actors over an engine ----------------------------------------------

use crate::model::{AddressDevice, DeviceDiagnosisOperatingState, FeatureAddress, MsgCounter};
use crate::spine::{Engine, ErrorNumber, HeartbeatProducer, RemoteDevice, SpineEvent, WriteToken};

/// The connection this car's limits belong to.
///
/// One, because OPEV is about the supply a single car is plugged into.
pub const ELECTRICAL_CONNECTION: u32 = 1;

/// What the car's actor did.
#[derive(Clone, Debug, PartialEq)]
pub enum EvEvent {
    /// A curtailment was applied.
    Curtailed {
        /// The `msgCounter` of the write, which the acknowledgement references.
        request: MsgCounter,
        /// What the car will now charge at, per phase.
        currents: ChargingCurrents,
    },
    /// A curtailment was refused.
    Refused {
        /// The `msgCounter` of the write.
        request: MsgCounter,
    },
    /// The reason the car is charging at what it is has changed.
    SourceChanged {
        /// What it was.
        from: CurrentSource,
        /// What it is now.
        to: CurrentSource,
    },
}

/// The car, wired to a SPINE engine.
///
/// Holds an [`EvCharging`] and the two features OPEV asks a car to serve, and turns
/// engine events into its input. Writes are deferred, so the acknowledgement the Energy
/// Guard receives says what the car actually did with the limit.
#[derive(Debug)]
pub struct EvActor {
    purpose: Purpose,
    charging: EvCharging,
    load_control: FeatureAddress,
    electrical_connection: FeatureAddress,
    phases: Vec<Phase>,
    range: (f64, f64),
    /// The Energy Guard's `DeviceDiagnosis`, once discovery has found it.
    guard_diagnosis: Option<FeatureAddress>,
    last_source: CurrentSource,
}

impl EvActor {
    /// Wires a car's charging state to the features it serves.
    ///
    /// `range` is the band this car can charge in, in amperes — the minimum below which
    /// it stops rather than draws less, and the maximum its electrics allow. The Energy
    /// Guard reads it from the permitted value sets before it curtails anything.
    pub fn new(
        purpose: Purpose,
        charging: EvCharging,
        load_control: FeatureAddress,
        electrical_connection: FeatureAddress,
        phases: impl Into<Vec<Phase>>,
        range: (f64, f64),
    ) -> Self {
        let last_source = charging.source();
        Self {
            purpose,
            charging,
            load_control,
            electrical_connection,
            phases: phases.into(),
            range,
            guard_diagnosis: None,
            last_source,
        }
    }

    /// Which of the two use cases this actor plays.
    pub fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// The charging state.
    pub fn charging(&self) -> &EvCharging {
        &self.charging
    }

    /// The charging state, for the application to read the current off.
    pub fn charging_mut(&mut self) -> &mut EvCharging {
        &mut self.charging
    }

    /// Publishes what this car serves: the limit descriptions, the current limits, the
    /// parameter descriptions, and the band it can charge in.
    pub fn publish(&self, engine: &mut Engine, now: Duration) {
        let load_control = self.load_control.clone();
        let connection = self.electrical_connection.clone();

        // Table 7: `LoadControl` records what the Energy Guard set, and `isLimitActive`
        // says whether it is in force. Publishing the safe current here instead would
        // report a number the manager never wrote as though it had.
        let in_force = self.charging.source() == CurrentSource::Curtailed;
        let payload = match self.charging.limit() {
            Some(limit) if limit.phases().next().is_some() => {
                limit_data(&limit, in_force, &PhaseLimits::own(&self.phases))
            }
            _ => deactivated(&self.phases, &PhaseLimits::own(&self.phases)),
        };

        let mut changed = false;
        if let Some(feature) = engine.device_mut().resolve_mut(&load_control) {
            let _ = feature.set_data(limit_descriptions(self.purpose, &self.phases));
            changed = feature.set_data(payload).unwrap_or(false);
        }
        if let Some(feature) = engine.device_mut().resolve_mut(&connection) {
            let _ = feature.set_data(parameter_descriptions(ELECTRICAL_CONNECTION, &self.phases));
            let _ = feature.set_data(permitted_value_sets(
                ELECTRICAL_CONNECTION,
                self.range.0,
                self.range.1,
                &self.phases,
            ));
        }
        if changed {
            engine.notify(&load_control, &Function::LoadControlLimitListData, now);
        }
    }

    /// Subscribes to the Energy Guard's heartbeat and state (scenarios 2 and 3).
    ///
    /// Both live on the manager's `DeviceDiagnosis`, and both mean the same thing to the
    /// car: whether the curtailment can be relied on. Find the address with
    /// [`locate_guard`] first — resolving it from the engine's own view of the peer and
    /// then borrowing the engine mutably are two steps, and the borrow checker is right
    /// to insist they stay that way.
    pub fn watch(&mut self, engine: &mut Engine, guard_diagnosis: FeatureAddress, now: Duration) {
        let local = self.load_control.clone();
        engine.request_subscription(&local, &guard_diagnosis, now);
        self.guard_diagnosis = Some(guard_diagnosis);
    }

    /// The Energy Guard's `DeviceDiagnosis` this car is watching, once it is.
    pub fn watching(&self) -> Option<&FeatureAddress> {
        self.guard_diagnosis.as_ref()
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.charging.poll_timeout()
    }

    /// Advances the four-second watchdog, and publishes if the answer changed.
    pub fn handle_timeout(&mut self, engine: &mut Engine, now: Duration) -> Option<EvEvent> {
        self.charging.handle_timeout(now);
        self.settle(engine, now)
    }

    /// Handles one engine event.
    pub fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: &SpineEvent,
        now: Duration,
    ) -> Option<EvEvent> {
        match event {
            SpineEvent::DataNotified {
                feature,
                resolved: data,
                ..
            }
            | SpineEvent::ReplyReceived {
                feature,
                resolved: data,
                ..
            } => {
                // Only the Energy Guard this car subscribed to counts. Taking a heartbeat
                // from whoever sends one would let any device on the connection keep the
                // car curtailed — which is the opposite of what scenarios 2 and 3 are for.
                if self.guard_diagnosis.as_ref() != Some(feature) {
                    return None;
                }
                let mut touched = false;
                if let CmdData::DeviceDiagnosisHeartbeatData(_) = data {
                    self.charging.on_heartbeat(now);
                    touched = true;
                }
                if let Some(state) = super::evsecc::read_operating_state(data) {
                    self.charging
                        .on_guard_state(super::evsecc::is_failure(&state));
                    touched = true;
                }
                touched.then(|| self.settle(engine, now)).flatten()
            }
            // `data` names the phases this write addresses, `resolved` gives the whole
            // set of currents that results — a partial write naming one phase leaves the
            // limits standing on the other two, and reading the fragment alone drops them.
            SpineEvent::WriteRequested(write) if write.feature == self.load_control => {
                Some(self.decide(
                    engine,
                    write.token,
                    write.request,
                    &write.data,
                    &write.resolved,
                    now,
                ))
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decide(
        &mut self,
        engine: &mut Engine,
        token: WriteToken,
        request: MsgCounter,
        data: &CmdData,
        resolved: &CmdData,
        now: Duration,
    ) -> EvEvent {
        // The phases this write addresses come from what arrived; the currents they end
        // up at come from `resolved`, so a partial write naming one phase does not drop
        // the limits standing on the other two (SPINE IG §3.3).
        let Some(written) = read_limit_write(data) else {
            engine.reject_write(token, ErrorNumber::CommandRejected, now);
            return EvEvent::Refused { request };
        };
        let Some(mut currents) = read_limit_write(resolved) else {
            engine.reject_write(token, ErrorNumber::CommandRejected, now);
            return EvEvent::Refused { request };
        };
        // A limit on a phase this car does not charge on is one it cannot follow. Drop
        // those, and refuse outright if that leaves a curtailment naming nothing — the
        // Energy Guard read the phases from the parameter descriptions, so asking for a
        // phase that is not there is a mistake worth reporting rather than ignoring.
        let named = written.phases().count();
        for phase in PHASES {
            if !self.phases.contains(&phase) {
                currents.set(phase, None);
            }
        }
        if named > 0 && currents.phases().next().is_none() {
            engine.reject_write(token, ErrorNumber::CommandRejected, now);
            return EvEvent::Refused { request };
        }
        if !self.charging.on_limit(currents, now) {
            engine.reject_write(token, ErrorNumber::CommandRejected, now);
            return EvEvent::Refused { request };
        }
        if engine.accept_write(token, now).is_err() {
            // The car decided to follow the limit and the engine could not store it, so
            // the manager was answered with an error. What the manager was told is what
            // this reports.
            self.publish(engine, now);
            return EvEvent::Refused { request };
        }
        // What the Energy Guard reads back is what the car will charge at, which after a
        // clamp is not always what it asked for.
        self.publish(engine, now);
        EvEvent::Curtailed {
            request,
            currents: self.charging.effective_currents(),
        }
    }

    /// Reports a change in why the car is charging at what it is, and republishes.
    fn settle(&mut self, engine: &mut Engine, now: Duration) -> Option<EvEvent> {
        let source = self.charging.source();
        if source == self.last_source {
            return None;
        }
        let from = core::mem::replace(&mut self.last_source, source);
        self.publish(engine, now);
        Some(EvEvent::SourceChanged { from, to: source })
    }
}

/// Where one car's OPEV features live.
#[derive(Clone, Debug, PartialEq)]
pub struct EvPeer {
    /// The car's device address.
    pub device: AddressDevice,
    /// Its `LoadControl` feature, which carries the per-phase current limits.
    pub load_control: FeatureAddress,
    /// Its `ElectricalConnection` feature, which says what it can take.
    pub electrical_connection: FeatureAddress,
    /// Which of the two use cases this peer was found under.
    ///
    /// A car may serve both, and the same `LoadControl` feature then carries two limits
    /// per phase — an `obligation` and a `recommendation`. Everything addressed on this
    /// peer has to name which, or the fuse's ceiling is written as advice.
    pub purpose: Purpose,
}

/// Finds the Energy Guard's `DeviceDiagnosis` from its discovery data.
///
/// §3.2.2.1 lets a manager call itself `CEM` or `EnergyGuard`, so both are looked for.
pub fn locate_guard(remote: &RemoteDevice, purpose: Purpose) -> Option<FeatureAddress> {
    let use_case = ENERGY_GUARD_ACTORS
        .iter()
        .find_map(|actor| remote.use_case(purpose.use_case_name(), actor))?;
    remote.address_of(use_case, &FeatureType::DeviceDiagnosis, Role::Server)
}

/// Finds a car's features for one of the two use cases from its discovery data.
pub fn locate(remote: &RemoteDevice, purpose: Purpose) -> Option<EvPeer> {
    let use_case = remote.use_case(purpose.use_case_name(), actors::EV)?;
    Some(EvPeer {
        device: remote.address.clone()?,
        load_control: remote.address_of(use_case, &FeatureType::LoadControl, Role::Server)?,
        electrical_connection: remote.address_of(
            use_case,
            &FeatureType::ElectricalConnection,
            Role::Server,
        )?,
        purpose,
    })
}

/// What the Energy Guard learned.
#[derive(Clone, Debug, PartialEq)]
pub enum GuardEvent {
    /// The binding is held and the car's band is known, so it can be curtailed.
    Ready {
        /// The car.
        device: AddressDevice,
        /// The band it can charge in, per phase, in amperes.
        band: ChargingBand,
    },
    /// The car accepted a curtailment.
    Accepted {
        /// The car.
        device: AddressDevice,
        /// What it accepted.
        currents: ChargingCurrents,
    },
    /// The car refused one.
    Refused {
        /// The car.
        device: AddressDevice,
        /// What it reported.
        error: ErrorNumber,
    },
    /// A request to this car went unanswered through the whole of the SPINE
    /// implementation guide §2.6.2 escalation path.
    ///
    /// Silence, not a refusal. Whatever the request was holding has been released and an
    /// outstanding curtailment goes out afresh, so the guard carries on.
    ///
    /// Worth surfacing (§2.6.4): a car that has stopped answering is one whose current the
    /// supply can no longer bound, which is a question about the fuse.
    Unresponsive {
        /// The car.
        device: AddressDevice,
    },
}

/// One car, and where the Energy Guard has got to with it.
#[derive(Debug)]
struct TrackedEv {
    peer: EvPeer,
    bound: bool,
    binding_request: Option<MsgCounter>,
    /// The band this car said it can charge in, per phase.
    band: ChargingBand,
    /// Table 9 exactly as it arrived.
    ///
    /// Kept because it cannot be read until the parameter descriptions say which set
    /// covers which phase, and the two replies may come back in either order. Re-resolved
    /// whenever either moves, so neither has to be the one that arrives second.
    permitted: Option<CmdData>,
    /// The phases this car charges on, from its parameter descriptions.
    phases: Vec<Phase>,
    /// Which `limitId` this car keeps each of those phases under.
    ///
    /// Nothing is written until at least one phase is in here: the numbers are the car's.
    limits: PhaseLimits,
    required: Option<ChargingCurrents>,
    outstanding: Option<(MsgCounter, ChargingCurrents)>,
    applied: Option<ChargingCurrents>,
    ready_reported: bool,
}

/// The Energy Guard of OPEV, wired to a SPINE engine.
///
/// It serves the heartbeat and the operating state the car watches, and writes the
/// per-phase currents. The heartbeat is what makes this actor different from LPC's: at
/// four seconds, missing one is not a hiccup, so it goes out on every tick rather than on
/// a schedule of its own.
#[derive(Debug)]
pub struct OverloadGuardActor {
    client: FeatureAddress,
    diagnosis: FeatureAddress,
    heartbeat: HeartbeatProducer,
    failed: bool,
    peers: Vec<TrackedEv>,
}

impl OverloadGuardActor {
    /// A guard writing from `client` and beating from `diagnosis`.
    pub fn new(client: FeatureAddress, diagnosis: FeatureAddress, now: Duration) -> Self {
        Self {
            client,
            diagnosis,
            // Half the car's watchdog, so one lost message is not a fallback.
            heartbeat: HeartbeatProducer::new(now)
                .with_period(HEARTBEAT_TIMEOUT / 2, now)
                .due_at(now),
            failed: false,
            peers: Vec::new(),
        }
    }

    /// Builds the `DeviceDiagnosis` feature the guard serves (Table 12, Table 13).
    pub fn device_diagnosis_feature(address: u32) -> LocalFeature {
        LocalFeature::new(address, FeatureType::DeviceDiagnosis, Role::Server)
            .with_function(Function::DeviceDiagnosisHeartbeatData, Operations::read())
            .with_function(Function::DeviceDiagnosisStateData, Operations::read())
    }

    /// Declares this manager unable to be relied on ([OPEV-007]).
    ///
    /// Cars watching will fall back to their safe currents at once — which is the point:
    /// a manager that knows it is broken says so rather than letting four seconds of
    /// silence say it.
    pub fn set_failed(&mut self, engine: &mut Engine, failed: bool, now: Duration) {
        if self.failed == failed {
            return;
        }
        self.failed = failed;
        self.publish_state(engine, now);
    }

    /// Whether this manager has announced a failure.
    pub fn has_failed(&self) -> bool {
        self.failed
    }

    /// Publishes the operating state cars watch (scenario 3).
    pub fn publish_state(&self, engine: &mut Engine, now: Duration) {
        let state = if self.failed {
            DeviceDiagnosisOperatingState::Failure
        } else {
            DeviceDiagnosisOperatingState::NormalOperation
        };
        let address = self.diagnosis.clone();
        let changed = engine
            .device_mut()
            .resolve_mut(&address)
            .and_then(|feature| {
                feature
                    .set_data(super::evsecc::operating_state(state, None))
                    .ok()
            })
            .unwrap_or(false);
        if changed {
            engine.notify(&address, &Function::DeviceDiagnosisStateData, now);
        }
    }

    /// Starts curtailing a car: binds to its `LoadControl`, reads what it can take, and
    /// reads how it numbers its per-phase limits.
    ///
    /// The limit description read is what makes the write addressable at all — see
    /// [`PhaseLimits`]. Until it and the parameter descriptions have both come back there
    /// is no `limitId` for any phase, and the guard writes nothing.
    pub fn attach(&mut self, engine: &mut Engine, peer: EvPeer, now: Duration) {
        let device = peer.device.clone();
        self.peers.retain(|t| t.peer.device != device);

        let binding_request = engine.request_binding(&self.client, &peer.load_control, now);
        engine.request_subscription(&self.client, &peer.load_control, now);
        engine.read(
            &peer.load_control,
            &self.client,
            Function::LoadControlLimitDescriptionListData,
            now,
        );
        for function in [
            // The descriptions first: they are what says which set covers which phase.
            // Order is a courtesy rather than a guarantee, which is why the reader keeps
            // the value payload and re-resolves it — see `TrackedEv::permitted`.
            Function::ElectricalConnectionParameterDescriptionListData,
            Function::ElectricalConnectionPermittedValueSetListData,
        ] {
            engine.read(&peer.electrical_connection, &self.client, function, now);
        }

        self.peers.push(TrackedEv {
            peer,
            bound: false,
            binding_request: Some(binding_request),
            band: ChargingBand::new(),
            permitted: None,
            phases: PHASES.to_vec(),
            limits: PhaseLimits::new(),
            required: None,
            outstanding: None,
            applied: None,
            ready_reported: false,
        });
    }

    /// Stops curtailing a car.
    pub fn detach(&mut self, device: &AddressDevice) {
        self.peers.retain(|t| &t.peer.device != device);
    }

    /// The band a car said it can charge in, per phase, once it has.
    pub fn band_of(&self, device: &AddressDevice) -> Option<&ChargingBand> {
        let band = &self.peers.iter().find(|t| &t.peer.device == device)?.band;
        (!band.is_empty()).then_some(band)
    }

    /// Sets the currents the supply can carry for one car ([OPEV-001]).
    ///
    /// Passing [`ChargingCurrents::default`] says no curtailment is needed, which
    /// [OPEV-004] asks a manager to state rather than imply by silence.
    pub fn require(&mut self, device: &AddressDevice, currents: ChargingCurrents) {
        if let Some(tracked) = self.peers.iter_mut().find(|t| &t.peer.device == device) {
            tracked.required = Some(currents);
        }
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called.
    pub fn poll_timeout(&self) -> Duration {
        self.heartbeat.poll_timeout()
    }

    /// Sends the heartbeat and any curtailment that is due.
    pub fn handle_timeout(&mut self, engine: &mut Engine, now: Duration) {
        let diagnosis = self.diagnosis.clone();
        self.heartbeat.tick(engine, &diagnosis, now);
        for index in 0..self.peers.len() {
            self.write_if_due(engine, index, now);
        }
    }

    /// Feeds one engine event to the actor.
    pub fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: &SpineEvent,
        now: Duration,
    ) -> Option<GuardEvent> {
        match event {
            SpineEvent::ReplyReceived {
                feature,
                resolved: data,
                ..
            }
            | SpineEvent::DataNotified {
                feature,
                resolved: data,
                ..
            } => {
                let index = self.peers.iter().position(|t| {
                    &t.peer.electrical_connection == feature || &t.peer.load_control == feature
                })?;
                // How this car numbers its per-phase limits, from either half.
                let purpose = self.peers[index].peer.purpose;
                if self.peers[index].limits.learn(data, purpose) {
                    if let Some(phases) = read_phases(data) {
                        self.peers[index].phases = phases;
                    }
                    // A parameter description may be what makes sense of a Table 9 reply
                    // that arrived before it.
                    self.resolve_band(index);
                    return self.report_ready(engine, index, now);
                }
                if !matches!(
                    data,
                    CmdData::ElectricalConnectionPermittedValueSetListData(_)
                ) {
                    return None;
                }
                self.peers[index].permitted = Some(data.clone());
                self.resolve_band(index);
                self.report_ready(engine, index, now)
            }
            SpineEvent::ResultReceived { request, error } => {
                self.resolve(engine, *request, *error, now)
            }
            SpineEvent::RequestTimedOut { request, .. } => self.give_up(engine, *request, now),
            _ => None,
        }
    }

    /// Lets go of a request the car never answered.
    ///
    /// The engine raises this only once SPINE IG §2.6.2's escalation path is exhausted, so
    /// the car is absent rather than slow. The curtailment matters most: an outstanding one
    /// blocks every later write to that car.
    fn give_up(
        &mut self,
        engine: &mut Engine,
        request: MsgCounter,
        now: Duration,
    ) -> Option<GuardEvent> {
        let index = self.peers.iter().position(|t| {
            t.outstanding.is_some_and(|(c, _)| c == request) || t.binding_request == Some(request)
        })?;
        let tracked = &mut self.peers[index];
        let device = tracked.peer.device.clone();
        if tracked.binding_request == Some(request) {
            tracked.binding_request = None;
        }
        if tracked.outstanding.is_some_and(|(c, _)| c == request) {
            tracked.outstanding = None;
            // Still required, so still owed.
            self.write_if_due(engine, index, now);
        }
        Some(GuardEvent::Unresponsive { device })
    }

    fn resolve(
        &mut self,
        engine: &mut Engine,
        request: MsgCounter,
        error: ErrorNumber,
        now: Duration,
    ) -> Option<GuardEvent> {
        if let Some(index) = self
            .peers
            .iter()
            .position(|t| t.binding_request == Some(request))
        {
            self.peers[index].binding_request = None;
            self.peers[index].bound = error.is_success();
            return self.report_ready(engine, index, now);
        }

        let index = self
            .peers
            .iter()
            .position(|t| t.outstanding.is_some_and(|(c, _)| c == request))?;
        let (_, currents) = self.peers[index].outstanding.take()?;
        let device = self.peers[index].peer.device.clone();
        if error.is_success() {
            self.peers[index].applied = Some(currents);
            return Some(GuardEvent::Accepted { device, currents });
        }
        Some(GuardEvent::Refused { device, error })
    }

    /// Re-reads Table 9 against the parameter descriptions known so far.
    ///
    /// Which permitted value set covers which phase is in the descriptions and nowhere
    /// else, so this does nothing useful until they have arrived — and everything, the
    /// moment they do.
    fn resolve_band(&mut self, index: usize) {
        let Some(data) = self.peers[index].permitted.clone() else {
            return;
        };
        let parameters = self.peers[index].limits.parameters().clone();
        if let Some(band) = read_permitted_range(&data, &parameters) {
            self.peers[index].band = band;
        }
    }

    /// Reports that a car can now be curtailed, once the binding and the band are both in.
    fn report_ready(
        &mut self,
        engine: &mut Engine,
        index: usize,
        now: Duration,
    ) -> Option<GuardEvent> {
        let tracked = &self.peers[index];
        let (true, false, false, true) = (
            tracked.bound,
            tracked.band.is_empty(),
            tracked.ready_reported,
            // Nothing is addressable until the car has said how it numbers its limits.
            tracked.limits.is_known(),
        ) else {
            return None;
        };
        self.peers[index].ready_reported = true;
        let device = self.peers[index].peer.device.clone();
        let band = self.peers[index].band.clone();
        self.write_if_due(engine, index, now);
        Some(GuardEvent::Ready { device, band })
    }

    fn write_if_due(&mut self, engine: &mut Engine, index: usize, now: Duration) {
        let tracked = &self.peers[index];
        if !tracked.bound || tracked.outstanding.is_some() || !tracked.limits.is_known() {
            return;
        }
        let Some(required) = tracked.required else {
            return;
        };
        if tracked.applied == Some(required) {
            return;
        }
        // A car cannot charge below its minimum, so a limit under it would stop the
        // charge rather than slow it. Clamping here means the guard asks for something
        // the car can actually do, and knows what it asked for.
        let required = required.clamped(&tracked.band);
        let target = tracked.peer.load_control.clone();
        // [OPEV-004]: "no curtailment needed" is written on every phase the car charges
        // on, deactivated — not sent as an empty list that says nothing.
        let payload = if required.phases().next().is_some() {
            limit_data(&required, true, &tracked.limits)
        } else {
            deactivated(&tracked.phases, &tracked.limits)
        };
        let counter = engine.write(&target, &self.client, payload, true, now);
        self.peers[index].outstanding = Some((counter, required));
    }
}

impl crate::usecases::signals::Signals<Purpose> for EvCharging {
    /// What a tester reads off a car, under `opev:` or `oscev:`.
    ///
    /// The important one is `…:source`: three inputs decide the current a car draws — the
    /// manager's ceiling, the car's own safe fallback, and its electrics — and which of
    /// them won is invisible on the wire. A car at 6 A because it was told 6 A and a car at
    /// 6 A because it has heard nothing for four seconds look identical to a meter and are
    /// entirely different events.
    fn signals(&self, purpose: Purpose) -> crate::usecases::signals::SignalSet {
        use crate::usecases::signals::{Signal, SignalSet, SignalValue};
        use alloc::borrow::Cow;
        use alloc::format;

        let prefix = purpose.signal_prefix();
        let name = |data_point: &str| -> Cow<'static, str> {
            Cow::Owned(format!("{prefix}:{data_point}"))
        };

        let mut set = SignalSet::new().with(Signal::new(
            name("source"),
            SignalValue::Text(Cow::Borrowed(self.source().as_str())),
        ));
        for phase in PHASES {
            let label = match phase {
                Phase::A => "A",
                Phase::B => "B",
                Phase::C => "C",
                _ => continue,
            };
            set = set.with(
                Signal::new(
                    name(&format!("current{label}")),
                    SignalValue::number(self.effective(phase)),
                )
                .in_unit("A"),
            );
        }
        set.with(Signal::new(
            name("lastHeartbeat"),
            SignalValue::seconds(self.last_heartbeat()),
        ))
    }
}
