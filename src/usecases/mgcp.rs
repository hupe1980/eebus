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
//!
//! # The two sides
//!
//! **Grid Connection Point.** [`curtailment_feature`] builds the `DeviceConfiguration`
//! feature scenario 1 is served from; [`curtailment_description`] and
//! [`curtailment_value`] are what goes in it. Scenarios 2 to 7 come from
//! [`MonitoredUnit`](crate::usecases::monitoring::MonitoredUnit).
//!
//! **Monitoring Appliance.** There is deliberately no `curtailment_client_feature` beside
//! [`curtailment_feature`], and the asymmetry is the specification's rather than an
//! omission: the LPC implementation guide §3.3 asks that "each Actor SHOULD use a client
//! Feature with featureType `Generic` for all its client functionality", so an appliance
//! holds **one** client feature for every server it reads — the `Measurement` of scenarios
//! 2 to 7 and the `DeviceConfiguration` of scenario 1 alike. Build it with
//! [`limitation::client_feature`](crate::usecases::limitation::client_feature), which is
//! the same constructor an Energy Guard uses, and hand it to
//! [`MonitoringApplianceActor`](crate::usecases::monitoring::MonitoringApplianceActor).
//! That actor serves the whole use case: [`monitoring::locate`] finds the connection
//! point's `DeviceConfiguration` feature alongside its measurements, and the factor
//! arrives as
//! [`MonitoringEvent::CurtailmentChanged`](crate::usecases::monitoring::MonitoringEvent::CurtailmentChanged).
//!
//! Reading scenario 1 by hand is [`Curtailment`], which is what the actor holds
//! internally. Either way the `keyId` comes from the connection point's own description —
//! see [`CURTAILMENT_KEY`] for why assuming it is a §9 export ceiling built on the wrong
//! number.
//!
//! [`monitoring::locate`]: crate::usecases::monitoring::locate

use alloc::vec;
use alloc::vec::Vec;

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

/// The `keyId` **this** implementation publishes the limitation factor under.
///
/// A local choice and nothing more. MGCP Table 23 spells the identifier `<k1#(1..1)>` —
/// the device picks it — and fixes the `keyName` instead. A Monitoring Appliance that
/// assumed this number would be reading whatever the connection point happens to keep at
/// index 1, which on a device with several configuration keys is some other key's value
/// carried into a §9 export ceiling as though it were a percentage. Find a peer's key
/// with [`find_curtailment_key`], or let [`Curtailment`] do it.
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
    curtailment_value_scaled(ScaledNumber::from_f64(percent, 2).unwrap_or_default())
}

/// [`curtailment_value`] for a factor given exactly, clamped to [`CURTAILMENT_RANGE`].
pub fn curtailment_value_scaled(percent: ScaledNumber) -> CmdData {
    CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
        device_configuration_key_value_data: Some(vec![DeviceConfigurationKeyValueData {
            key_id: Some(CURTAILMENT_KEY),
            value: Some(DeviceConfigurationKeyValueValue {
                scaled_number: clamp_factor(percent),
                ..Default::default()
            }),
            ..Default::default()
        }]),
    })
}

/// Finds the `keyId` a Grid Connection Point publishes the limitation factor under.
///
/// Give it that peer's `deviceConfigurationKeyValueDescriptionListData`, read once at
/// commissioning. `keyName` is the element MGCP Table 23 fixes —
/// `pvCurtailmentLimitFactor` — and the `keyId` beside it is the device's own.
pub fn find_curtailment_key(data: &CmdData) -> Option<DeviceConfigurationKeyId> {
    crate::usecases::addressing::find_key_id(data, &PV_CURTAILMENT_LIMIT_FACTOR)
}

/// Reads the limitation factor out of a `deviceConfigurationKeyValueListData`.
///
/// `key` is the identifier **on the device the payload came from** — this device's own
/// [`CURTAILMENT_KEY`] when reading back what it published, and the peer's, from
/// [`find_curtailment_key`], when reading a Grid Connection Point. [`Curtailment`] pairs
/// the two for you and is what a Monitoring Appliance should use.
///
/// Returns the percentage. On its own a percentage is not actionable — see
/// [`FeedInLimit`] for the value an inverter or an energy manager can hold itself to.
pub fn read_curtailment(data: &CmdData, key: DeviceConfigurationKeyId) -> Option<f64> {
    read_curtailment_scaled(data, key).and_then(|factor| factor.to_f64())
}

/// [`read_curtailment`], exactly as it crossed the wire.
pub fn read_curtailment_scaled(
    data: &CmdData,
    key: DeviceConfigurationKeyId,
) -> Option<ScaledNumber> {
    let CmdData::DeviceConfigurationKeyValueListData(list) = data else {
        return None;
    };
    list.device_configuration_key_value_data
        .iter()
        .flatten()
        .find(|entry| entry.key_id == Some(key))
        .and_then(|entry| entry.value.as_ref())
        .and_then(|value| value.scaled_number)
        .filter(|factor| factor.to_f64().is_some())
}

/// A factor held to [`CURTAILMENT_RANGE`], exactly: the ends are whole numbers, so a
/// factor outside them becomes the end itself rather than an approximation of it.
fn clamp_factor(percent: ScaledNumber) -> Option<ScaledNumber> {
    let value = percent.to_f64()?;
    Some(if value < *CURTAILMENT_RANGE.start() {
        ScaledNumber::whole(0)
    } else if value > *CURTAILMENT_RANGE.end() {
        ScaledNumber::whole(100)
    } else {
        percent.canonical()
    })
}

/// Scenario 1 as a Monitoring Appliance sees it: a description, then values.
///
/// The two halves arrive in separate functions and in either order, and neither is usable
/// alone — the description says which `keyId` is the curtailment factor, the value list
/// says what the numbers are. Holding them together is the whole job, and doing it wrong
/// is silent: a factor read out of the wrong key is still a number in the range 0 to 100,
/// and it becomes an export ceiling.
///
/// Feed it every payload that arrives from the connection point's `DeviceConfiguration`
/// feature, in whatever order they come.
///
/// ```
/// use eebus::usecases::mgcp::{self, Curtailment};
///
/// let mut factor = Curtailment::new();
/// factor.describe(&mgcp::curtailment_description());
/// assert_eq!(factor.apply(&mgcp::curtailment_value(70.0)), Some(70.0));
/// assert_eq!(factor.factor_percent(), Some(70.0));
/// assert_eq!(factor.limit(12_000.0).map(|l| l.watts()), Some(8_400.0));
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Curtailment {
    key: Option<DeviceConfigurationKeyId>,
    values: Vec<(DeviceConfigurationKeyId, ScaledNumber)>,
}

impl Curtailment {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Learns which `keyId` carries the factor, from a description payload.
    ///
    /// Returns whether this payload was one — a caller can hand it everything and use the
    /// answer to tell a description from a value list.
    pub fn describe(&mut self, data: &CmdData) -> bool {
        if !matches!(
            data,
            CmdData::DeviceConfigurationKeyValueDescriptionListData(_)
        ) {
            return false;
        }
        if let Some(key) = find_curtailment_key(data) {
            self.key = Some(key);
        }
        true
    }

    /// Applies a value payload, returning the factor if it is now known.
    ///
    /// Values for keys this appliance has no description for are kept rather than
    /// dropped: the two functions may arrive in either order, and a value that came first
    /// would otherwise be lost until the connection point next notified one.
    pub fn apply(&mut self, data: &CmdData) -> Option<f64> {
        let CmdData::DeviceConfigurationKeyValueListData(list) = data else {
            return None;
        };
        for entry in list.device_configuration_key_value_data.iter().flatten() {
            let (Some(id), Some(value)) = (entry.key_id, entry.value.as_ref()) else {
                continue;
            };
            let Some(number) = value.scaled_number.filter(|n| n.to_f64().is_some()) else {
                continue;
            };
            match self.values.iter_mut().find(|(known, _)| *known == id) {
                Some((_, stored)) => *stored = number,
                None => self.values.push((id, number)),
            }
        }
        self.factor_percent()
    }

    /// The `keyId` the connection point published the factor under, once described.
    pub fn key(&self) -> Option<DeviceConfigurationKeyId> {
        self.key
    }

    /// The factor, as a percentage, once both halves have arrived.
    ///
    /// [`None`] while either is missing. Not zero: an unread message is not a
    /// curtailment, and the difference is a roof that stops exporting.
    pub fn factor_percent(&self) -> Option<f64> {
        self.factor_scaled()?.to_f64()
    }

    /// The factor exactly as it crossed the wire, held to [`CURTAILMENT_RANGE`].
    pub fn factor_scaled(&self) -> Option<ScaledNumber> {
        let key = self.key?;
        self.values
            .iter()
            .find(|(known, _)| *known == key)
            .and_then(|(_, value)| clamp_factor(*value))
    }

    /// The factor as a ceiling in watts, given the building's installed peak power.
    pub fn limit(&self, nominal_peak_watts: f64) -> Option<FeedInLimit> {
        self.limit_scaled(ScaledNumber::from_f64(nominal_peak_watts.max(0.0), 3)?)
    }

    /// [`limit`](Self::limit) with the peak power given exactly.
    pub fn limit_scaled(&self, nominal_peak: ScaledNumber) -> Option<FeedInLimit> {
        Some(FeedInLimit::new_scaled(self.factor_scaled()?, nominal_peak))
    }
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
    factor: ScaledNumber,
    nominal_peak: ScaledNumber,
}

impl FeedInLimit {
    /// A ceiling from the factor and the building's cumulated nominal PV peak power.
    ///
    /// The factor is clamped to [`CURTAILMENT_RANGE`], as
    /// [`curtailment_value`] clamps it on the way out: a factor above 100 % would
    /// otherwise permit more feed-in than the array can produce, which is not a
    /// limitation at all. Both go to exact values at three decimals.
    pub fn new(factor_percent: f64, nominal_peak_watts: f64) -> Self {
        Self::new_scaled(
            ScaledNumber::from_f64(factor_percent, 3).unwrap_or_default(),
            ScaledNumber::from_f64(nominal_peak_watts.max(0.0), 3).unwrap_or_default(),
        )
    }

    /// A ceiling from the factor and the peak power, exactly.
    ///
    /// ```
    /// use eebus::model::ScaledNumber;
    /// use eebus::usecases::mgcp::FeedInLimit;
    ///
    /// let limit = FeedInLimit::new_scaled(ScaledNumber::whole(7), ScaledNumber::whole(9_900));
    /// assert_eq!(limit.scaled(), Some(ScaledNumber::whole(693)));
    /// assert_eq!(limit.watts(), 693.0);
    /// ```
    pub fn new_scaled(factor_percent: ScaledNumber, nominal_peak: ScaledNumber) -> Self {
        let nominal_peak = if nominal_peak.to_f64().is_some_and(|w| w < 0.0) {
            ScaledNumber::whole(0)
        } else {
            nominal_peak.canonical()
        };
        Self {
            factor: clamp_factor(factor_percent).unwrap_or_default(),
            nominal_peak,
        }
    }

    /// A ceiling read straight off a `deviceConfigurationKeyValueListData`.
    ///
    /// `key` is the identifier on the device the payload came from; see
    /// [`read_curtailment`], and prefer [`Curtailment`], which finds it from the peer's
    /// own description rather than asking a caller to have it already.
    ///
    /// [`None`] when the payload carries no factor — which is not the same as a factor of
    /// zero, and must not be treated as one: an unread message is not a curtailment.
    pub fn from_data(
        data: &CmdData,
        key: DeviceConfigurationKeyId,
        nominal_peak_watts: f64,
    ) -> Option<Self> {
        let peak = ScaledNumber::from_f64(nominal_peak_watts.max(0.0), 3)?;
        read_curtailment_scaled(data, key).map(|factor| Self::new_scaled(factor, peak))
    }

    /// The factor as it crossed the wire, as a percentage.
    pub fn factor_percent(self) -> f64 {
        self.factor.to_f64().unwrap_or(f64::NAN)
    }

    /// The factor, exactly.
    pub fn factor_scaled(self) -> ScaledNumber {
        self.factor
    }

    /// The building's cumulated nominal PV peak power, in watts.
    pub fn nominal_peak_watts(self) -> f64 {
        self.nominal_peak.to_f64().unwrap_or(f64::NAN)
    }

    /// The most that may be fed in, in watts, exactly: `factor × peak / 100` on mantissa
    /// and scale. [`None`] only when the product overflows an `i64` mantissa.
    pub fn scaled(self) -> Option<ScaledNumber> {
        let product = self.factor.checked_mul(&self.nominal_peak)?;
        let scale = product.scale.map_or(0, |s| s.get()).checked_sub(2)?;
        Some(ScaledNumber::new(product.number?.get(), scale).canonical())
    }

    /// The most that may be fed in, in watts: [`scaled`](Self::scaled), correctly rounded.
    pub fn watts(self) -> f64 {
        self.scaled()
            .and_then(|watts| watts.to_f64())
            .unwrap_or_else(|| self.factor_percent() * self.nominal_peak_watts() / 100.0)
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
        match self.scaled() {
            Some(watts) => crate::usecases::limitation::LimitWrite::active_scaled(watts),
            None => crate::usecases::limitation::LimitWrite::active(self.watts()),
        }
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

    /// A ceiling that is a whole number of watts comes out as that number.
    ///
    /// `factor / 100 × peak` rounds twice: 7 % of 9 900 W was 693.0000000000001, and
    /// one in twelve whole-watt ceilings came out a fraction above themselves. On
    /// mantissa and scale it is exact.
    #[test]
    fn a_feed_in_ceiling_is_exact() {
        for percent in 0..=100i64 {
            for peak in (1_000..=30_000i64).step_by(100) {
                let limit = FeedInLimit::new(percent as f64, peak as f64);
                let exact = percent * peak;
                assert_eq!(
                    limit.scaled().map(ScaledNumber::canonical),
                    Some(ScaledNumber::new(exact, -2).canonical()),
                    "{percent} % of {peak} W"
                );
                if exact % 100 == 0 {
                    assert_eq!(
                        limit.watts(),
                        (exact / 100) as f64,
                        "{percent} % of {peak} W"
                    );
                }
            }
        }
        let mut factor = Curtailment::new();
        factor.describe(&curtailment_description());
        factor.apply(&curtailment_value_scaled(ScaledNumber::new(705, -1)));
        assert_eq!(factor.factor_scaled(), Some(ScaledNumber::new(705, -1)));
        assert_eq!(
            factor
                .limit_scaled(ScaledNumber::whole(10_000))
                .and_then(FeedInLimit::scaled),
            Some(ScaledNumber::whole(7_050))
        );
    }

    #[test]
    fn mgcp_011_the_curtailment_factor_round_trips() {
        let published = curtailment_value(70.0);
        assert_eq!(read_curtailment(&published, CURTAILMENT_KEY), Some(70.0));

        // MGCP Table 23 fixes the range at 0 to 100.
        assert_eq!(
            read_curtailment(&curtailment_value(140.0), CURTAILMENT_KEY),
            Some(100.0)
        );
        assert_eq!(
            read_curtailment(&curtailment_value(-5.0), CURTAILMENT_KEY),
            Some(0.0)
        );

        // A fraction of a percent survives, which whole-number rounding would lose.
        assert_eq!(
            read_curtailment(&curtailment_value(62.5), CURTAILMENT_KEY),
            Some(62.5)
        );
    }

    /// The `keyId` comes from the peer's description, never from this crate's own.
    ///
    /// A connection point with several configuration keys keeps the factor wherever it
    /// likes. Reading key 1 there returns another key's number — in range, plausible, and
    /// on its way to becoming a §9 export ceiling.
    #[test]
    fn the_factor_is_found_by_its_name_and_not_by_our_own_identifier() {
        let their_key = DeviceConfigurationKeyId(4);
        let description = CmdData::DeviceConfigurationKeyValueDescriptionListData(
            DeviceConfigurationKeyValueDescriptionListData {
                device_configuration_key_value_description_data: Some(vec![
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(CURTAILMENT_KEY),
                        key_name: Some(DeviceConfigurationKeyName::PeakPowerOfPvSystem),
                        ..Default::default()
                    },
                    DeviceConfigurationKeyValueDescriptionData {
                        key_id: Some(their_key),
                        key_name: Some(PV_CURTAILMENT_LIMIT_FACTOR),
                        value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
                        unit: Some(UnitOfMeasurement::Pct),
                        ..Default::default()
                    },
                ]),
            },
        );
        assert_eq!(find_curtailment_key(&description), Some(their_key));

        let values =
            CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
                device_configuration_key_value_data: Some(vec![
                    DeviceConfigurationKeyValueData {
                        key_id: Some(CURTAILMENT_KEY),
                        value: Some(DeviceConfigurationKeyValueValue {
                            scaled_number: ScaledNumber::from_f64(9.0, 3),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    DeviceConfigurationKeyValueData {
                        key_id: Some(their_key),
                        value: Some(DeviceConfigurationKeyValueValue {
                            scaled_number: ScaledNumber::from_f64(60.0, 2),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ]),
            });

        let mut factor = Curtailment::new();
        assert!(factor.describe(&description));
        assert_eq!(factor.apply(&values), Some(60.0), "not the 9 kWp on key 1");
        assert_eq!(factor.key(), Some(their_key));
    }

    /// The value list may arrive before the description, and must not be lost.
    #[test]
    fn a_value_that_arrives_before_its_description_is_kept() {
        let mut factor = Curtailment::new();
        assert_eq!(factor.apply(&curtailment_value(55.0)), None);
        assert_eq!(factor.factor_percent(), None, "nothing is known yet");
        factor.describe(&curtailment_description());
        assert_eq!(factor.factor_percent(), Some(55.0));
    }

    /// The equation of [MGCP-011], with the numbers a German §9 installation uses.
    #[test]
    fn mgcp_011_the_ceiling_is_the_factor_times_the_installed_peak_power() {
        let published = curtailment_value(70.0);
        let limit =
            FeedInLimit::from_data(&published, CURTAILMENT_KEY, 12_000.0).expect("a factor");
        assert_eq!(limit.watts(), 8_400.0);
        assert_eq!(limit.factor_percent(), 70.0);
        assert_eq!(limit.nominal_peak_watts(), 12_000.0);

        // And the value LPP is written with, which is how an inverter hears about it.
        assert_eq!(limit.as_production_limit().watts(), 8_400.0);
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
        assert_eq!(
            FeedInLimit::from_data(&empty, CURTAILMENT_KEY, 12_000.0),
            None
        );
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
