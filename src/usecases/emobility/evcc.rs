//! EV Commissioning and Configuration (EVCC).
//!
//! What the car is, told once when it is plugged in. Every other e-mobility use case
//! assumes this has happened, in the same way that every one of them assumes
//! [`evsecc`](super::evsecc) has: without it an energy manager has a socket with something
//! on the end of it and no idea what.
//!
//! Eight scenarios, and the two that decide whether anything else can work are the first
//! and the third:
//!
//! 1. **EV connected** — an `EV` entity appears underneath the `EVSE` entity. There is no
//!    message for this; the *appearance of the entity* is the message, which is why it is
//!    a scenario with no functions of its own.
//! 2. **EV sends communication standard** — ISO 15118 or IEC 61851. This is the one that
//!    says how much can be asked of the car at all: a car on IEC 61851 has a pilot wire
//!    and nothing else, and cannot be asked its state of charge or given a charging plan.
//! 3. **EV sends support of asymmetric charging** — whether the three phases may be
//!    limited independently, which is what [`opev`](super::opev) needs to know before it
//!    writes three different currents.
//! 4. **EV sends identification** — an EUI-48 or EUI-64, so the same car is recognised
//!    when it comes back.
//! 5. **EV sends manufacturer information** — the strings a user interface shows.
//! 6. **EV sends charging power limits** — the band it can charge in, in watts.
//! 7. **EV sleep mode** — `standby`, which means the car is still plugged in and has
//!    stopped talking. Not a fault, and not a disconnection.
//! 8. **EV disconnected** — the entity goes away again.
//!
//! Scenarios 1 and 8 have no payload of their own, and this module has nothing to offer
//! for them beyond saying so: they are the SPINE entity tree changing, which
//! [`LocalDevice`](crate::spine::LocalDevice) and detailed discovery already do.
//!
//! ```
//! use eebus::usecases::emobility::evcc::{self, CommunicationStandard, EvProfile, EvReader};
//!
//! // What a car publishes about itself.
//! let car = EvProfile::new()
//!     .communication_standard(CommunicationStandard::Iso15118Ed2)
//!     .asymmetric_charging(true)
//!     .charging_power(1_400.0, 11_000.0);
//!
//! // And what the energy manager reads back. The descriptions are not optional: they
//! // are what say which of the car's `keyId`s is which.
//! let mut reader = EvReader::new();
//! reader.apply(&car.key_descriptions());
//! reader.apply(&car.key_values());
//!
//! let learned = reader.profile();
//! assert_eq!(learned.communication_standard, Some(CommunicationStandard::Iso15118Ed2));
//! assert_eq!(learned.asymmetric_charging, Some(true));
//! assert!(evcc::EV.defines_scenario(8));
//! ```

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::model::{
    CmdData, DeviceClassificationManufacturerData, DeviceConfigurationKeyId,
    DeviceConfigurationKeyName, DeviceConfigurationKeyValueData,
    DeviceConfigurationKeyValueDescriptionData, DeviceConfigurationKeyValueDescriptionListData,
    DeviceConfigurationKeyValueListData, DeviceConfigurationKeyValueType,
    DeviceConfigurationKeyValueValue, DeviceDiagnosisOperatingState, DeviceDiagnosisStateData,
    ElectricalConnectionId, ElectricalConnectionParameterDescriptionData,
    ElectricalConnectionParameterDescriptionListData, ElectricalConnectionParameterId,
    ElectricalConnectionPermittedValueSetData, ElectricalConnectionPermittedValueSetListData,
    ElectricalConnectionPhaseName, EntityType, FeatureType, Function, IdentificationData,
    IdentificationId, IdentificationListData, IdentificationType, Role, ScaledNumber,
    ScaledNumberRange, ScaledNumberSet, ScopeType,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::addressing::{KeyIds, ParameterIds};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::{actors, names};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.1";

/// The `keyId` of the communication standard (scenario 2, `<x1>` of Table 6).
pub const COMMUNICATION_STANDARD_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(1);

/// The `keyId` of the asymmetric-charging flag (scenario 3, `<x2>` of Table 6).
pub const ASYMMETRIC_CHARGING_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(2);

/// The `identificationId` this implementation gives the car's address.
pub const IDENTIFICATION_ID: IdentificationId = IdentificationId(1);

/// The `electricalConnectionId` scenario 6's power band is published under.
pub const ELECTRICAL_CONNECTION: ElectricalConnectionId = ElectricalConnectionId(1);

/// The `parameterId` of that band.
pub const POWER_PARAMETER: ElectricalConnectionParameterId = ElectricalConnectionParameterId(1);

/// How the car and the charging station talk to each other ([EVCC-004]).
///
/// The single most consequential thing a car says about itself, because it decides what
/// else can be asked of it. A car on IEC 61851 has a pilot wire: a current can be
/// suggested to it and nothing can be read back. A car on ISO 15118 has a data link, and
/// [`evsoc`](super::evsoc) and coordinated charging become possible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommunicationStandard {
    /// `iso15118-2ed1`: ISO 15118-2, first edition.
    Iso15118Ed1,
    /// `iso15118-2ed2`: ISO 15118-2, second edition.
    Iso15118Ed2,
    /// `iec61851`: the pilot wire, and nothing else.
    Iec61851,
    /// A value this version of the specification does not define.
    ///
    /// Kept rather than refused: the list is open, and a manager that cannot name a car's
    /// protocol can still limit its current. Discarding it would leave a user interface
    /// showing "unknown" where the car said something specific.
    Other(&'static str),
}

impl CommunicationStandard {
    /// The string that crosses the wire, as `value.string` (Table 6).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Iso15118Ed1 => "iso15118-2ed1",
            Self::Iso15118Ed2 => "iso15118-2ed2",
            Self::Iec61851 => "iec61851",
            Self::Other(value) => value,
        }
    }

    /// Reads one off the wire. Unknown strings are not an error.
    ///
    /// Not [`FromStr`](core::str::FromStr): that trait's error type would have to describe
    /// a failure this function does not have. An unrecognised standard is a fact about the
    /// car, not a parse error, and the only thing that is really absent is the empty
    /// string.
    pub fn read(value: &str) -> Option<Self> {
        Some(match value {
            "iso15118-2ed1" => Self::Iso15118Ed1,
            "iso15118-2ed2" => Self::Iso15118Ed2,
            "iec61851" => Self::Iec61851,
            "" => return None,
            _ => Self::Other("other"),
        })
    }

    /// Whether the car has a data link rather than only a pilot wire.
    ///
    /// The question worth asking of this value: a car that answers `false` cannot be asked
    /// for a state of charge or given a charging plan, however willing the energy manager
    /// is. An unrecognised standard answers `false`, because assuming a data link that is
    /// not there produces requests nothing will ever answer.
    pub const fn has_data_link(self) -> bool {
        matches!(self, Self::Iso15118Ed1 | Self::Iso15118Ed2)
    }
}

/// How the car identifies itself ([EVCC-007]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvIdentification {
    /// `eui48` or `eui64`.
    pub kind: IdentificationType,
    /// The address, `AA-BB-CC-…`, uppercase hexadecimal separated by hyphens.
    pub value: String,
}

impl EvIdentification {
    /// An EUI-48, the six-byte form.
    pub fn eui48(value: impl Into<String>) -> Self {
        Self {
            kind: IdentificationType::Eui48,
            value: value.into(),
        }
    }

    /// An EUI-64, the eight-byte form.
    pub fn eui64(value: impl Into<String>) -> Self {
        Self {
            kind: IdentificationType::Eui64,
            value: value.into(),
        }
    }

    /// Whether the value matches the pattern Table 8 fixes for its kind.
    ///
    /// Six groups for an EUI-48, eight for an EUI-64, each two uppercase hexadecimal
    /// digits, separated by hyphens. Checked rather than assumed because this string is
    /// how a car is *recognised on its next visit*: a manager that stored a
    /// lowercase form and compared it against an uppercase one would greet the same car as
    /// a stranger every time.
    pub fn is_well_formed(&self) -> bool {
        let groups = match self.kind {
            IdentificationType::Eui48 => 6,
            IdentificationType::Eui64 => 8,
            _ => return false,
        };
        let parts: Vec<&str> = self.value.split('-').collect();
        parts.len() == groups
            && parts.iter().all(|part| {
                part.len() == 2
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
            })
    }
}

/// Everything scenarios 2 to 7 say about one car.
///
/// The car fills it in and publishes it; an energy manager reads one back out of
/// [`EvReader`], which is what resolves the car's own identifiers into these fields. Every
/// field is optional in both directions, and
/// deliberately: Table 1 marks scenarios 4, 5, 6 and 7 `R` for the car, so a manager that
/// required any of them would refuse to work with a conforming car.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EvProfile {
    /// Scenario 2: how the car and the station talk.
    pub communication_standard: Option<CommunicationStandard>,
    /// Scenario 3: whether the phases may be limited independently.
    pub asymmetric_charging: Option<bool>,
    /// Scenario 4: the car's address.
    pub identification: Option<EvIdentification>,
    /// Scenario 5: the strings a user interface shows.
    pub manufacturer: Option<DeviceClassificationManufacturerData>,
    /// Scenario 6: the band it can charge in, in watts — minimum and maximum.
    pub charging_power: Option<(f64, Option<f64>)>,
    /// Scenario 7: whether the car has gone to sleep.
    pub asleep: Option<bool>,
}

impl EvProfile {
    /// An empty profile: nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets scenario 2.
    #[must_use]
    pub fn communication_standard(mut self, standard: CommunicationStandard) -> Self {
        self.communication_standard = Some(standard);
        self
    }

    /// Sets scenario 3.
    #[must_use]
    pub fn asymmetric_charging(mut self, supported: bool) -> Self {
        self.asymmetric_charging = Some(supported);
        self
    }

    /// Sets scenario 4.
    #[must_use]
    pub fn identification(mut self, identification: EvIdentification) -> Self {
        self.identification = Some(identification);
        self
    }

    /// Sets scenario 5.
    #[must_use]
    pub fn manufacturer(mut self, manufacturer: DeviceClassificationManufacturerData) -> Self {
        self.manufacturer = Some(manufacturer);
        self
    }

    /// Sets scenario 6: the band the car can charge in, in watts.
    ///
    /// [EVCC-017] makes the minimum mandatory and [EVCC-018] the maximum optional, which
    /// is the right way round — the minimum is the number that matters. Below it a car
    /// does not charge slowly, it stops, and an energy manager that offers less than the
    /// minimum has turned the charge off rather than slowed it down.
    #[must_use]
    pub fn charging_power(mut self, min: f64, max: f64) -> Self {
        self.charging_power = Some((min, Some(max)));
        self
    }

    /// Sets scenario 6 with no stated maximum.
    #[must_use]
    pub fn minimum_charging_power(mut self, min: f64) -> Self {
        self.charging_power = Some((min, None));
        self
    }

    /// Sets scenario 7.
    #[must_use]
    pub fn asleep(mut self, asleep: bool) -> Self {
        self.asleep = Some(asleep);
        self
    }

    /// The two key descriptions of scenarios 2 and 3 (Table 6).
    pub fn key_descriptions(&self) -> CmdData {
        CmdData::DeviceConfigurationKeyValueDescriptionListData(
            DeviceConfigurationKeyValueDescriptionListData {
                device_configuration_key_value_description_data: Some(vec![
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(COMMUNICATION_STANDARD_KEY),
                        key_name: Some(DeviceConfigurationKeyName::CommunicationsStandard),
                        value_type: Some(DeviceConfigurationKeyValueType::String),
                        ..Default::default()
                    },
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(ASYMMETRIC_CHARGING_KEY),
                        key_name: Some(DeviceConfigurationKeyName::AsymmetricChargingSupported),
                        value_type: Some(DeviceConfigurationKeyValueType::Boolean),
                        ..Default::default()
                    },
                ]),
            },
        )
    }

    /// The two key values of scenarios 2 and 3 (Table 7).
    pub fn key_values(&self) -> CmdData {
        let mut entries = Vec::new();
        if let Some(standard) = self.communication_standard {
            entries.push(DeviceConfigurationKeyValueData {
                key_id: Some(COMMUNICATION_STANDARD_KEY),
                value: Some(DeviceConfigurationKeyValueValue {
                    string: Some(standard.as_str().into()),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        if let Some(asymmetric) = self.asymmetric_charging {
            entries.push(DeviceConfigurationKeyValueData {
                key_id: Some(ASYMMETRIC_CHARGING_KEY),
                value: Some(DeviceConfigurationKeyValueValue {
                    boolean: Some(asymmetric),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
            device_configuration_key_value_data: Some(entries),
        })
    }

    /// Scenario 4's identification (Table 8).
    pub fn identification_data(&self) -> CmdData {
        CmdData::IdentificationListData(IdentificationListData {
            identification_data: Some(
                self.identification
                    .iter()
                    .map(|id| IdentificationData {
                        identification_id: Some(IDENTIFICATION_ID),
                        identification_type: Some(id.kind.clone()),
                        identification_value: Some(id.value.as_str().into()),
                        ..Default::default()
                    })
                    .collect(),
            ),
        })
    }

    /// Scenario 5's manufacturer information (Table 9).
    pub fn manufacturer_data(&self) -> CmdData {
        CmdData::DeviceClassificationManufacturerData(self.manufacturer.clone().unwrap_or_default())
    }

    /// Scenario 6's parameter description (Table 10).
    ///
    /// `acPowerTotal`: the band is a total across whatever phases the car uses, not a
    /// per-phase figure. [`opev`](super::opev) is where per-phase currents live.
    pub fn power_parameter_description(&self) -> CmdData {
        CmdData::ElectricalConnectionParameterDescriptionListData(
            ElectricalConnectionParameterDescriptionListData {
                electrical_connection_parameter_description_data: Some(vec![
                    ElectricalConnectionParameterDescriptionData {
                        electrical_connection_id: Some(ELECTRICAL_CONNECTION),
                        parameter_id: Some(POWER_PARAMETER),
                        ac_measured_phases: Some(ElectricalConnectionPhaseName::Abc),
                        scope_type: Some(ScopeType::AcPowerTotal),
                        ..Default::default()
                    },
                ]),
            },
        )
    }

    /// Scenario 6's permitted value set (Table 11).
    pub fn power_limits(&self) -> CmdData {
        let sets = self
            .charging_power
            .map(|(min, max)| {
                vec![ElectricalConnectionPermittedValueSetData {
                    electrical_connection_id: Some(ELECTRICAL_CONNECTION),
                    parameter_id: Some(POWER_PARAMETER),
                    permitted_value_set: Some(vec![ScaledNumberSet {
                        range: Some(vec![ScaledNumberRange {
                            min: Some(ScaledNumber::from_f64(min, 0)),
                            max: max.map(|max| ScaledNumber::from_f64(max, 0)),
                        }]),
                        ..Default::default()
                    }]),
                }]
            })
            .unwrap_or_default();
        CmdData::ElectricalConnectionPermittedValueSetListData(
            ElectricalConnectionPermittedValueSetListData {
                electrical_connection_permitted_value_set_data: Some(sets),
            },
        )
    }

    /// Scenario 7's operating state (Table 12).
    ///
    /// `standby` means asleep: still plugged in, still charging or not according to what
    /// it was last told, and no longer answering. It is not `failure`, and a manager that
    /// treated it as one would unplug a car that is working perfectly ([EVCC-020]).
    pub fn state_data(&self) -> CmdData {
        CmdData::DeviceDiagnosisStateData(DeviceDiagnosisStateData {
            operating_state: Some(match self.asleep {
                Some(true) => DeviceDiagnosisOperatingState::Standby,
                _ => DeviceDiagnosisOperatingState::NormalOperation,
            }),
            ..Default::default()
        })
    }

    /// Whether the car can be asked for more than a current.
    ///
    /// Scenario 2 is the gate on everything richer: `false` for a car on IEC 61851, and
    /// `false` while scenario 2 has not arrived, because acting on a data link that may
    /// not exist produces requests nothing answers.
    pub fn supports_data_exchange(&self) -> bool {
        self.communication_standard
            .is_some_and(CommunicationStandard::has_data_link)
    }
}

/// The manager's side of EVCC: what a car has published, resolved.
///
/// [`EvProfile`] is the value — the car builds one and publishes it, and a manager reads
/// one. This is what turns the car's payloads back into it, and it exists separately
/// because the two directions are not symmetrical: a car knows what its own `keyId` 1
/// means, and a manager does not.
///
/// Scenarios 2 and 3 are `DeviceConfiguration` keys and scenario 6 is an
/// `ElectricalConnection` parameter. All three are addressed by identifiers the *car*
/// chose (`<k1#(1..1)>`, `<p1#(1..1)>`), with the meaning in a description function
/// beside them — `keyName`, `scopeType`. So this holds the descriptions, keeps the raw
/// values that arrive before them, and resolves the two against each other whenever
/// either moves. Reading `keyId` 1 as the communication standard would take whatever that
/// car keeps there; on a car with several configuration keys that is another key's string,
/// and a manager would conclude the car speaks ISO 15118 when it does not.
///
/// ```
/// use eebus::usecases::emobility::evcc::{self, CommunicationStandard, EvProfile, EvReader};
///
/// let car = EvProfile::new().communication_standard(CommunicationStandard::Iso15118Ed2);
///
/// // The values arrive before the descriptions, which is allowed and survivable.
/// let mut reader = EvReader::new();
/// reader.apply(&car.key_values());
/// assert_eq!(reader.profile().communication_standard, None);
///
/// reader.apply(&car.key_descriptions());
/// assert_eq!(
///     reader.profile().communication_standard,
///     Some(CommunicationStandard::Iso15118Ed2)
/// );
/// ```
#[derive(Clone, Debug, Default)]
pub struct EvReader {
    profile: EvProfile,
    keys: KeyIds,
    parameters: ParameterIds,
    /// Configuration values as they arrived, by the car's own `keyId`.
    key_values: Vec<(DeviceConfigurationKeyId, DeviceConfigurationKeyValueValue)>,
    /// Permitted value sets as they arrived, by the car's own `parameterId`.
    ranges: Vec<(ElectricalConnectionParameterId, (f64, Option<f64>))>,
}

impl EvReader {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been learned so far.
    pub fn profile(&self) -> &EvProfile {
        &self.profile
    }

    /// The identifiers this car chose for its configuration keys.
    pub fn keys(&self) -> &KeyIds {
        &self.keys
    }

    /// The identifiers this car chose for its electrical-connection parameters.
    pub fn parameters(&self) -> &ParameterIds {
        &self.parameters
    }

    /// Takes in whatever a car has published, whichever scenario it belongs to.
    ///
    /// Returns whether the *profile* changed — not whether a payload was understood — so
    /// a manager can act on the change rather than on every notification. A description
    /// that completes a value that arrived earlier therefore returns `true`, and a
    /// notification that repeats a value returns `false`. A payload of a kind this use
    /// case does not carry is ignored, not an error: the same features carry other use
    /// cases' data.
    pub fn apply(&mut self, data: &CmdData) -> bool {
        let before = self.profile.clone();
        match data {
            CmdData::DeviceConfigurationKeyValueListData(list) => {
                for entry in list.device_configuration_key_value_data.iter().flatten() {
                    let (Some(id), Some(value)) = (entry.key_id, entry.value.as_ref()) else {
                        continue;
                    };
                    match self.key_values.iter_mut().find(|(known, _)| *known == id) {
                        Some((_, stored)) => *stored = value.clone(),
                        None => self.key_values.push((id, value.clone())),
                    }
                }
                self.resolve_keys();
            }
            CmdData::ElectricalConnectionPermittedValueSetListData(list) => {
                for entry in list
                    .electrical_connection_permitted_value_set_data
                    .iter()
                    .flatten()
                {
                    let Some(id) = entry.parameter_id else {
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
                    let Some(min) = range.min.as_ref().and_then(ScaledNumber::to_f64) else {
                        continue;
                    };
                    let band = (min, range.max.as_ref().and_then(ScaledNumber::to_f64));
                    match self.ranges.iter_mut().find(|(known, _)| *known == id) {
                        Some((_, stored)) => *stored = band,
                        None => self.ranges.push((id, band)),
                    }
                }
                self.resolve_ranges();
            }
            CmdData::IdentificationListData(list) => {
                if let Some(entry) = list.identification_data.iter().flatten().next()
                    && let (Some(kind), Some(value)) = (
                        entry.identification_type.clone(),
                        entry.identification_value.as_ref(),
                    )
                {
                    self.profile.identification = Some(EvIdentification {
                        kind,
                        value: value.as_str().to_string(),
                    });
                }
            }
            CmdData::DeviceClassificationManufacturerData(manufacturer) => {
                self.profile.manufacturer = Some(manufacturer.clone());
            }
            CmdData::DeviceDiagnosisStateData(state) => {
                if let Some(operating) = state.operating_state.as_ref() {
                    self.profile.asleep =
                        Some(*operating == DeviceDiagnosisOperatingState::Standby);
                }
            }
            // A description says what the identifiers mean, which may make sense of
            // values that arrived before it.
            _ => {
                if self.keys.learn(data) {
                    self.resolve_keys();
                } else if self.parameters.learn(data) {
                    self.resolve_ranges();
                }
            }
        }
        self.profile != before
    }

    /// Re-reads the stored configuration values against the names now known.
    fn resolve_keys(&mut self) {
        for (id, value) in &self.key_values {
            match self.keys.name_of(*id) {
                Some(DeviceConfigurationKeyName::CommunicationsStandard) => {
                    self.profile.communication_standard = value
                        .string
                        .as_ref()
                        .and_then(|value| CommunicationStandard::read(value.as_str()));
                }
                Some(DeviceConfigurationKeyName::AsymmetricChargingSupported) => {
                    self.profile.asymmetric_charging = value.boolean;
                }
                _ => {}
            }
        }
    }

    /// Re-reads the stored permitted value sets against the parameters now known.
    ///
    /// Scenario 6's parameter is the one describing total AC power (Table 11). A car that
    /// also publishes a per-phase current parameter has two, and taking whichever came
    /// first would clamp a charging power to a band in amperes.
    fn resolve_ranges(&mut self) {
        let Some(parameter) = self.parameters.by_scope(&ScopeType::AcPowerTotal) else {
            return;
        };
        if let Some((_, band)) = self.ranges.iter().find(|(id, _)| *id == parameter) {
            self.profile.charging_power = Some(*band);
        }
    }
}

// ---- the features a car serves ---------------------------------------------------

/// Builds the `DeviceConfiguration` feature of scenarios 2 and 3 (Table 5).
pub fn device_configuration_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceConfiguration, Role::Server)
        .with_function(
            Function::DeviceConfigurationKeyValueDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::DeviceConfigurationKeyValueListData,
            Operations::read(),
        )
}

/// Builds the `Identification` feature of scenario 4.
pub fn identification_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::Identification, Role::Server)
        .with_function(Function::IdentificationListData, Operations::read())
}

/// Builds the `DeviceClassification` feature of scenario 5.
pub fn device_classification_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceClassification, Role::Server).with_function(
        Function::DeviceClassificationManufacturerData,
        Operations::read(),
    )
}

/// Builds the `ElectricalConnection` feature of scenario 6.
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

/// Builds the `DeviceDiagnosis` feature of scenario 7.
pub fn device_diagnosis_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceDiagnosis, Role::Server)
        .with_function(Function::DeviceDiagnosisStateData, Operations::read())
}

// ---- descriptors -------------------------------------------------------------------

const EV_ENTITIES: &[EntityType] = &[EntityType::EV];
const CEM_ENTITIES: &[EntityType] = &[EntityType::CEM];

/// Scenario 1 and 8 have no functions: the entity appearing and disappearing *is* the
/// message, and detailed discovery is what carries it.
const NO_FUNCTIONS: &[FunctionUse] = &[];

const EV_CONFIGURATION: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

const EV_IDENTIFICATION: &[FunctionUse] = &[FunctionUse::server(
    FeatureType::Identification,
    Function::IdentificationListData,
)];

const EV_MANUFACTURER: &[FunctionUse] = &[FunctionUse::server(
    FeatureType::DeviceClassification,
    Function::DeviceClassificationManufacturerData,
)];

const EV_POWER: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionPermittedValueSetListData,
    ),
];

const EV_STATE: &[FunctionUse] = &[FunctionUse::server(
    FeatureType::DeviceDiagnosis,
    Function::DeviceDiagnosisStateData,
)];

const CEM_CONFIGURATION: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

const CEM_IDENTIFICATION: &[FunctionUse] = &[FunctionUse::client(
    FeatureType::Identification,
    Function::IdentificationListData,
)];

const CEM_MANUFACTURER: &[FunctionUse] = &[FunctionUse::client(
    FeatureType::DeviceClassification,
    Function::DeviceClassificationManufacturerData,
)];

const CEM_POWER: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionPermittedValueSetListData,
    ),
];

const CEM_STATE: &[FunctionUse] = &[FunctionUse::client(
    FeatureType::DeviceDiagnosis,
    Function::DeviceDiagnosisStateData,
)];

/// The scenario names, shared by the two descriptors so they cannot drift.
const NAMES: [&str; 8] = [
    "EV connected",
    "EV sends communication standard",
    "EV sends support of asymmetric charging",
    "EV sends identification",
    "EV sends manufacturer information",
    "EV sends charging power limits",
    "EV sleep mode",
    "EV disconnected",
];

/// The car: the actor that describes itself.
pub static EV: UseCaseDescriptor = UseCaseDescriptor {
    name: names::EVCC,
    actor: actors::EV,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: EV_ENTITIES,
    counterpart: actors::CEM,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: NO_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: EV_CONFIGURATION,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Mandatory,
            functions: EV_CONFIGURATION,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Recommended,
            functions: EV_IDENTIFICATION,
        },
        Scenario {
            number: 5,
            name: NAMES[4],
            support: Support::Recommended,
            functions: EV_MANUFACTURER,
        },
        Scenario {
            number: 6,
            name: NAMES[5],
            support: Support::Recommended,
            functions: EV_POWER,
        },
        Scenario {
            number: 7,
            name: NAMES[6],
            support: Support::Recommended,
            functions: EV_STATE,
        },
        Scenario {
            number: 8,
            name: NAMES[7],
            support: Support::Mandatory,
            functions: NO_FUNCTIONS,
        },
    ],
};

/// The energy manager: the actor that learns what the car is.
///
/// Table 1 marks scenarios 6 and 7 `M` here where the car has them as `R`, which is not a
/// contradiction: a manager must be able to *understand* a car that sends its power limits
/// and its sleep state, and a car need not send them.
pub static CEM: UseCaseDescriptor = UseCaseDescriptor {
    name: names::EVCC,
    actor: actors::CEM,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: CEM_ENTITIES,
    counterpart: actors::EV,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: NO_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: CEM_CONFIGURATION,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Mandatory,
            functions: CEM_CONFIGURATION,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Recommended,
            functions: CEM_IDENTIFICATION,
        },
        Scenario {
            number: 5,
            name: NAMES[4],
            support: Support::Recommended,
            functions: CEM_MANUFACTURER,
        },
        Scenario {
            number: 6,
            name: NAMES[5],
            support: Support::Mandatory,
            functions: CEM_POWER,
        },
        Scenario {
            number: 7,
            name: NAMES[6],
            support: Support::Mandatory,
            functions: CEM_STATE,
        },
        Scenario {
            number: 8,
            name: NAMES[7],
            support: Support::Mandatory,
            functions: NO_FUNCTIONS,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn a_car() -> EvProfile {
        EvProfile::new()
            .communication_standard(CommunicationStandard::Iso15118Ed2)
            .asymmetric_charging(true)
            .identification(EvIdentification::eui48("AA-BB-CC-DD-EE-FF"))
            .charging_power(1_400.0, 11_000.0)
            .asleep(false)
    }

    /// Everything a car publishes, read back by the energy manager unchanged.
    #[test]
    fn a_profile_survives_the_round_trip_through_every_scenario() {
        let car = a_car();
        let mut reader = EvReader::new();

        for payload in [
            car.key_descriptions(),
            car.key_values(),
            car.power_parameter_description(),
            car.identification_data(),
            car.power_limits(),
            car.state_data(),
        ] {
            reader.apply(&payload);
        }
        let learned = reader.profile();

        assert_eq!(
            learned.communication_standard,
            Some(CommunicationStandard::Iso15118Ed2)
        );
        assert_eq!(learned.asymmetric_charging, Some(true));
        assert_eq!(
            learned.identification,
            Some(EvIdentification::eui48("AA-BB-CC-DD-EE-FF"))
        );
        assert_eq!(learned.charging_power, Some((1_400.0, Some(11_000.0))));
        assert_eq!(learned.asleep, Some(false));
    }

    /// `apply` says whether anything changed, so a manager acts on changes rather than on
    /// every notification — and a repeated notification is not a change.
    #[test]
    fn applying_the_same_payload_twice_reports_no_change() {
        let car = a_car();
        let mut reader = EvReader::new();
        reader.apply(&car.key_descriptions());

        assert!(reader.apply(&car.key_values()));
        assert!(!reader.apply(&car.key_values()));
    }

    /// A car that numbers its configuration keys its own way is read correctly.
    ///
    /// `keyId` 1 here is a key this use case knows nothing about, holding a string. Read
    /// by number it would be handed to `CommunicationStandard::read`, and a manager that
    /// happened to find a matching word there would conclude the car speaks a protocol it
    /// does not — and then ask it for a state of charge nothing answers.
    #[test]
    fn a_cars_configuration_keys_are_found_by_name() {
        let descriptions = CmdData::DeviceConfigurationKeyValueDescriptionListData(
            DeviceConfigurationKeyValueDescriptionListData {
                device_configuration_key_value_description_data: Some(vec![
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(DeviceConfigurationKeyId(1)),
                        key_name: Some(DeviceConfigurationKeyName::PeakPowerOfPvSystem),
                        value_type: Some(DeviceConfigurationKeyValueType::String),
                        ..Default::default()
                    },
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(DeviceConfigurationKeyId(6)),
                        key_name: Some(DeviceConfigurationKeyName::CommunicationsStandard),
                        value_type: Some(DeviceConfigurationKeyValueType::String),
                        ..Default::default()
                    },
                ]),
            },
        );
        let values =
            CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
                device_configuration_key_value_data: Some(vec![
                    DeviceConfigurationKeyValueData {
                        key_id: Some(DeviceConfigurationKeyId(1)),
                        value: Some(DeviceConfigurationKeyValueValue {
                            string: Some("iec61851".to_string().into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    DeviceConfigurationKeyValueData {
                        key_id: Some(DeviceConfigurationKeyId(6)),
                        value: Some(DeviceConfigurationKeyValueValue {
                            string: Some("iso15118-2ed2".to_string().into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ]),
            });

        let mut reader = EvReader::new();
        reader.apply(&values);
        assert_eq!(
            reader.profile().communication_standard,
            None,
            "nothing can be read until the car says what its keys are"
        );

        assert!(
            reader.apply(&descriptions),
            "the description completes the value, which is a change"
        );
        assert_eq!(
            reader.profile().communication_standard,
            Some(CommunicationStandard::Iso15118Ed2),
            "key 6, not the `iec61851` string sitting on key 1"
        );
        assert!(reader.profile().supports_data_exchange());
    }

    /// [EVCC-004]: the communication standard decides what else the car can be asked.
    #[test]
    fn evcc_004_the_communication_standard_gates_everything_richer() {
        assert!(CommunicationStandard::Iso15118Ed1.has_data_link());
        assert!(CommunicationStandard::Iso15118Ed2.has_data_link());
        assert!(
            !CommunicationStandard::Iec61851.has_data_link(),
            "a pilot wire and nothing else"
        );
        assert!(
            !CommunicationStandard::Other("something-new").has_data_link(),
            "assuming a data link that may not be there produces requests nothing answers"
        );

        assert!(!EvProfile::new().supports_data_exchange(), "not yet known");
        assert!(a_car().supports_data_exchange());
    }

    /// Table 6 fixes the three strings; an unfamiliar one is kept rather than refused.
    #[test]
    fn the_communication_standard_round_trips_and_survives_an_unknown_value() {
        for standard in [
            CommunicationStandard::Iso15118Ed1,
            CommunicationStandard::Iso15118Ed2,
            CommunicationStandard::Iec61851,
        ] {
            assert_eq!(
                CommunicationStandard::read(standard.as_str()),
                Some(standard)
            );
        }
        assert_eq!(
            CommunicationStandard::read("iso15118-20"),
            Some(CommunicationStandard::Other("other"))
        );
        assert_eq!(CommunicationStandard::read(""), None);
    }

    /// Table 8 fixes the pattern, and it matters: this string is how a car is recognised
    /// on its next visit.
    #[test]
    fn evcc_007_an_identification_is_checked_against_its_pattern() {
        assert!(EvIdentification::eui48("AA-BB-CC-DD-EE-FF").is_well_formed());
        assert!(EvIdentification::eui64("00-11-22-33-44-55-66-77").is_well_formed());

        assert!(
            !EvIdentification::eui48("aa-bb-cc-dd-ee-ff").is_well_formed(),
            "lowercase would compare unequal against the same car next time"
        );
        assert!(
            !EvIdentification::eui48("AA-BB-CC-DD-EE").is_well_formed(),
            "five groups is not an EUI-48"
        );
        assert!(!EvIdentification::eui64("AA-BB-CC-DD-EE-FF").is_well_formed());
        assert!(!EvIdentification::eui48("AABBCCDDEEFF").is_well_formed());
    }

    /// [EVCC-020]: asleep is not broken. A manager that confused the two would unplug a
    /// car that is working perfectly.
    #[test]
    fn evcc_020_standby_is_not_a_failure() {
        let CmdData::DeviceDiagnosisStateData(awake) = a_car().state_data() else {
            unreachable!("built above");
        };
        assert_eq!(
            awake.operating_state,
            Some(DeviceDiagnosisOperatingState::NormalOperation)
        );

        let CmdData::DeviceDiagnosisStateData(asleep) = a_car().asleep(true).state_data() else {
            unreachable!("built above");
        };
        assert_eq!(
            asleep.operating_state,
            Some(DeviceDiagnosisOperatingState::Standby)
        );
        assert_ne!(
            asleep.operating_state,
            Some(DeviceDiagnosisOperatingState::Failure)
        );
    }

    /// Table 1, and the asymmetry in it: scenarios 6 and 7 are `R` for the car and `M` for
    /// the manager. A manager must understand what a car need not send.
    #[test]
    fn the_descriptors_say_what_table_1_says() {
        assert_eq!(EV.use_case_name().as_str(), names::EVCC);
        assert_eq!(EV.version, "1.0.1");
        assert_eq!(
            EV.required_scenarios().collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(
            CEM.required_scenarios().collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );

        let car_six = EV.scenarios.iter().find(|s| s.number == 6).unwrap();
        let manager_six = CEM.scenarios.iter().find(|s| s.number == 6).unwrap();
        assert_eq!(car_six.support, Support::Recommended);
        assert_eq!(manager_six.support, Support::Mandatory);

        // Nothing binds: every scenario here is a read.
        assert_eq!(CEM.features_needing_binding().count(), 0);
    }

    /// Scenarios 1 and 8 are the entity tree changing, and carry no functions of their own.
    #[test]
    fn the_connection_scenarios_have_no_payload() {
        for number in [1, 8] {
            let scenario = EV.scenarios.iter().find(|s| s.number == number).unwrap();
            assert!(
                scenario.functions.is_empty(),
                "scenario {number} has no message of its own"
            );
        }
    }
}
