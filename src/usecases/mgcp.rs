//! Monitoring of the Grid Connection Point (MGCP).
//!
//! A *Monitoring Appliance* reads what is crossing the building's grid connection: the
//! momentary power, the energy fed in and drawn, current and voltage per phase, the grid
//! frequency, and the factor by which PV feed-in is curtailed. It is the fourth of the
//! use cases certifiable since July 2026, and it is what tells an energy manager whether
//! the building as a whole is importing or exporting.
//!
//! Seven scenarios make it up:
//!
//! 1. **Monitor PV feed-in power limitation factor** — the percentage of the installed
//!    PV peak power that may be fed in, as `pvCurtailmentLimitFactor`.
//! 2. **Monitor momentary power consumption/production** — the total at the connection.
//! 3. **Monitor total feed-in energy**.
//! 4. **Monitor total consumed energy**.
//! 5. **Monitor momentary current consumption/production phase details**.
//! 6. **Monitor voltage phase details**.
//! 7. **Monitor frequency**.
//!
//! Scenarios 2 to 7 are the same exchange as MPC, and share its implementation in
//! [`crate::usecases::monitoring`]; what differs is that a grid connection point names
//! the two energies from the grid's side — `gridConsumption` and `gridFeedIn` — which is
//! what [`NAMING`] selects.

use alloc::vec;

use crate::model::{
    CmdData, DeviceConfigurationKeyId, DeviceConfigurationKeyName, DeviceConfigurationKeyValueData,
    DeviceConfigurationKeyValueDescriptionData, DeviceConfigurationKeyValueDescriptionListData,
    DeviceConfigurationKeyValueListData, DeviceConfigurationKeyValueType,
    DeviceConfigurationKeyValueValue, EntityType, FeatureType, Function, Role, ScaledNumber,
    UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{
    ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor, actors, names,
};
use crate::usecases::monitoring::Naming;

/// How a Grid Connection Point names its energy scopes for this use case.
pub const NAMING: Naming = Naming::GridConnectionPoint;

/// The `keyName` of scenario 1's limitation factor (MGCP Table 23).
///
/// Its value is a percentage of the cumulated nominal peak power of the building's PV
/// systems: the maximum feed-in is that factor times that sum ([MGCP-011]).
pub const PV_CURTAILMENT_LIMIT_FACTOR: DeviceConfigurationKeyName =
    DeviceConfigurationKeyName::PvCurtailmentLimitFactor;

/// The `keyId` this implementation gives the limitation factor.
pub const CURTAILMENT_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(1);

/// The range the factor may take, as a percentage (MGCP Table 23).
pub const CURTAILMENT_RANGE: core::ops::RangeInclusive<f64> = 0.0..=100.0;

/// Builds the `DeviceConfiguration` feature scenario 1 is served from.
///
/// Reads only: the factor is set by whatever configures the connection point, not by a
/// Monitoring Appliance, which is why MGCP marks no write on it.
pub fn curtailment_feature(address: u32) -> LocalFeature {
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

/// The description of the limitation factor (MGCP Table 23).
pub fn curtailment_description() -> CmdData {
    CmdData::DeviceConfigurationKeyValueDescriptionListData(
        DeviceConfigurationKeyValueDescriptionListData {
            device_configuration_key_value_description_data: Some(vec![
                DeviceConfigurationKeyValueDescriptionData {
                    key_id: Some(CURTAILMENT_KEY),
                    key_name: Some(PV_CURTAILMENT_LIMIT_FACTOR),
                    value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
                    unit: Some(UnitOfMeasurement::Pct),
                    ..Default::default()
                },
            ]),
        },
    )
}

/// The limitation factor's current value, as a percentage (MGCP Table 24).
///
/// Values outside `0..=100` are clamped: the specification fixes the range, and a factor
/// above 100 would tell a client it may feed in more than the building can produce.
pub fn curtailment_value(percent: f64) -> CmdData {
    let percent = percent.clamp(*CURTAILMENT_RANGE.start(), *CURTAILMENT_RANGE.end());
    CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
        device_configuration_key_value_data: Some(vec![DeviceConfigurationKeyValueData {
            key_id: Some(CURTAILMENT_KEY),
            value: Some(DeviceConfigurationKeyValueValue {
                scaled_number: Some(ScaledNumber::from_f64(percent, 2)),
                ..Default::default()
            }),
            ..Default::default()
        }]),
    })
}

/// Reads the limitation factor out of a `deviceConfigurationKeyValueListData`.
///
/// Returns the percentage. On its own a percentage is not actionable — see
/// [`FeedInLimit`] for the value an inverter or an energy manager can hold itself to.
pub fn read_curtailment(data: &CmdData) -> Option<f64> {
    let CmdData::DeviceConfigurationKeyValueListData(list) = data else {
        return None;
    };
    list.device_configuration_key_value_data
        .iter()
        .flatten()
        .find(|entry| entry.key_id == Some(CURTAILMENT_KEY))
        .and_then(|entry| entry.value.as_ref())
        .and_then(|value| value.scaled_number.as_ref())
        .and_then(ScaledNumber::to_f64)
}

/// The ceiling scenario 1 puts on feed-in, in watts.
///
/// [MGCP-011] states the rule as an equation rather than a value:
///
/// ```text
/// P_PV,feed-in  ≤  PLF_PV,feed-in,max,pct  ×  Σ P_PV,AC,nom
/// ```
///
/// The factor is what crosses the wire; the sum of the installed PV systems' nominal peak
/// power is a property of the building that no EEBUS message carries. Only the two
/// together are a number of watts, and it is the watts that an energy manager acts on —
/// in Germany, as the export ceiling of EEG §9. Keeping both terms in one value is what
/// stops a caller acting on a percentage as though it were a power.
///
/// ```
/// use eebus::usecases::mgcp::FeedInLimit;
///
/// // 70 % of a 12 kWp array: the classic §9 configuration.
/// let limit = FeedInLimit::new(70.0, 12_000.0);
/// assert_eq!(limit.watts(), 8_400.0);
/// assert!(limit.permits(8_000.0));
/// assert!(!limit.permits(9_000.0));
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedInLimit {
    factor_percent: f64,
    nominal_peak_watts: f64,
}

impl FeedInLimit {
    /// A ceiling from the factor and the building's cumulated nominal PV peak power.
    ///
    /// The factor is clamped to [`CURTAILMENT_RANGE`], as
    /// [`curtailment_value`] clamps it on the way out: a factor above 100 % would
    /// otherwise permit more feed-in than the array can produce, which is not a
    /// limitation at all.
    pub fn new(factor_percent: f64, nominal_peak_watts: f64) -> Self {
        Self {
            factor_percent: factor_percent
                .clamp(*CURTAILMENT_RANGE.start(), *CURTAILMENT_RANGE.end()),
            nominal_peak_watts: nominal_peak_watts.max(0.0),
        }
    }

    /// A ceiling read straight off a `deviceConfigurationKeyValueListData`.
    ///
    /// [`None`] when the payload carries no factor — which is not the same as a factor of
    /// zero, and must not be treated as one: an unread message is not a curtailment.
    pub fn from_data(data: &CmdData, nominal_peak_watts: f64) -> Option<Self> {
        read_curtailment(data).map(|factor| Self::new(factor, nominal_peak_watts))
    }

    /// The factor as it crossed the wire, as a percentage.
    pub fn factor_percent(self) -> f64 {
        self.factor_percent
    }

    /// The building's cumulated nominal PV peak power, in watts.
    pub fn nominal_peak_watts(self) -> f64 {
        self.nominal_peak_watts
    }

    /// The most that may be fed in, in watts.
    pub fn watts(self) -> f64 {
        self.factor_percent / 100.0 * self.nominal_peak_watts
    }

    /// Whether feeding in this much is within the ceiling.
    pub fn permits(self, watts: f64) -> bool {
        watts <= self.watts()
    }

    /// The ceiling as a [`LimitWrite`] an Energy Guard can hand to LPP.
    ///
    /// This is the join between the two use cases, and it is the ordinary way a §9
    /// installation is built: MGCP scenario 1 says what the connection point permits, and
    /// LPP is how an inverter is told. The limit carries no duration — the factor is a
    /// standing configuration, not an event — so it holds until the next one arrives.
    ///
    /// [`LimitWrite`]: crate::usecases::limitation::LimitWrite
    pub fn as_production_limit(self) -> crate::usecases::limitation::LimitWrite {
        crate::usecases::limitation::LimitWrite::active(self.watts())
    }
}

/// Entity types a Grid Connection Point may live on (MGCP §3.2.2.1.1).
///
/// On a `CEM` the actor is a surrogate: the energy manager copies the values from the
/// real connection point and serves them on to other appliances.
const GRID_CONNECTION_POINT_ENTITIES: &[EntityType] =
    &[EntityType::CEM, EntityType::GridConnectionPointOfPremises];

const SERVER_MEASUREMENTS: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::Measurement,
        Function::MeasurementDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::Measurement,
        Function::MeasurementConstraintsListData,
    ),
    FunctionUse::server(FeatureType::Measurement, Function::MeasurementListData),
];

const CLIENT_MEASUREMENTS: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::Measurement,
        Function::MeasurementDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::Measurement,
        Function::MeasurementConstraintsListData,
    ),
    FunctionUse::client(FeatureType::Measurement, Function::MeasurementListData),
];

/// Scenario 1 alone uses `DeviceConfiguration`, not `Measurement`: the curtailment factor
/// is a configured value, not something the connection point measures.
const SERVER_CURTAILMENT: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

const CLIENT_CURTAILMENT: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

/// The Monitoring Appliance: the actor that reads.
pub static MONITORING_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: names::MGCP,
    actor: actors::MONITORING_APPLIANCE,
    role: ActorRole::Client,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: &[],
    counterpart: actors::GRID_CONNECTION_POINT,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Monitor PV feed-in power limitation factor",
            support: Support::Optional,
            functions: CLIENT_CURTAILMENT,
        },
        Scenario {
            number: 2,
            name: "Monitor momentary power consumption/production",
            support: Support::Recommended,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 3,
            name: "Monitor total feed-in energy",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: "Monitor total consumed energy",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 5,
            name: "Monitor momentary current consumption/production phase details",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: "Monitor voltage phase details",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 7,
            name: "Monitor frequency",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
    ],
};

/// The Grid Connection Point: the actor that measures.
pub static GRID_CONNECTION_POINT: UseCaseDescriptor = UseCaseDescriptor {
    name: names::MGCP,
    actor: actors::GRID_CONNECTION_POINT,
    role: ActorRole::Server,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: GRID_CONNECTION_POINT_ENTITIES,
    counterpart: actors::MONITORING_APPLIANCE,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Monitor PV feed-in power limitation factor",
            support: Support::Optional,
            functions: SERVER_CURTAILMENT,
        },
        Scenario {
            number: 2,
            name: "Monitor momentary power consumption/production",
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 3,
            name: "Monitor total feed-in energy",
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: "Monitor total consumed energy",
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 5,
            name: "Monitor momentary current consumption/production phase details",
            support: Support::Recommended,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: "Monitor voltage phase details",
            support: Support::Optional,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 7,
            name: "Monitor frequency",
            support: Support::Optional,
            functions: SERVER_MEASUREMENTS,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mgcp_011_the_curtailment_factor_round_trips() {
        let published = curtailment_value(70.0);
        assert_eq!(read_curtailment(&published), Some(70.0));

        // MGCP Table 23 fixes the range at 0 to 100.
        assert_eq!(read_curtailment(&curtailment_value(140.0)), Some(100.0));
        assert_eq!(read_curtailment(&curtailment_value(-5.0)), Some(0.0));

        // A fraction of a percent survives, which whole-number rounding would lose.
        assert_eq!(read_curtailment(&curtailment_value(62.5)), Some(62.5));
    }

    /// The equation of [MGCP-011], with the numbers a German §9 installation uses.
    #[test]
    fn mgcp_011_the_ceiling_is_the_factor_times_the_installed_peak_power() {
        let published = curtailment_value(70.0);
        let limit = FeedInLimit::from_data(&published, 12_000.0).expect("a factor");
        assert_eq!(limit.watts(), 8_400.0);
        assert_eq!(limit.factor_percent(), 70.0);
        assert_eq!(limit.nominal_peak_watts(), 12_000.0);

        // And the value LPP is written with, which is how an inverter hears about it.
        assert_eq!(limit.as_production_limit().watts, 8_400.0);
        assert!(limit.as_production_limit().is_active);
    }

    /// A payload with no factor in it yields no ceiling. Reading it as zero would curtail
    /// a building to nothing on a message that said nothing at all.
    #[test]
    fn a_payload_without_a_factor_is_not_a_ceiling_of_zero() {
        let empty =
            CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
                device_configuration_key_value_data: Some(vec![]),
            });
        assert_eq!(FeedInLimit::from_data(&empty, 12_000.0), None);
    }

    /// A building with no PV has no feed-in to limit, and a negative array is not a
    /// building.
    #[test]
    fn the_terms_of_the_equation_stay_inside_their_ranges() {
        assert_eq!(FeedInLimit::new(70.0, 0.0).watts(), 0.0);
        assert_eq!(FeedInLimit::new(70.0, -1.0).nominal_peak_watts(), 0.0);
        assert_eq!(FeedInLimit::new(140.0, 10_000.0).factor_percent(), 100.0);
        assert_eq!(FeedInLimit::new(-5.0, 10_000.0).watts(), 0.0);
    }

    #[test]
    fn the_description_names_the_key_the_specification_asks_for() {
        let CmdData::DeviceConfigurationKeyValueDescriptionListData(list) =
            curtailment_description()
        else {
            unreachable!("built above");
        };
        let entry = &list
            .device_configuration_key_value_description_data
            .as_ref()
            .unwrap()[0];
        assert_eq!(
            entry.key_name.as_ref().map(|k| k.as_str()),
            Some("pvCurtailmentLimitFactor")
        );
        assert_eq!(entry.unit, Some(UnitOfMeasurement::Pct));
    }
}
