//! EV Charging Summary (EVCS).
//!
//! What a charging session cost and where its energy came from: the total energy and the
//! total price since the car was plugged in, split into the share that came off the roof
//! and the share that came off the grid.
//!
//! It is the one use case in this family that goes the *other* way. Everywhere else the
//! wallbox is the server and the energy manager reads it — [`evcem`](super::evcem) reports
//! what flowed, [`evsoc`](super::evsoc) how full the battery is. Here the **EVSE serves a
//! writeable `Bill`** and the manager writes into it: only the manager knows what the roof
//! was producing at the time and what the tariff was, and the wallbox is where a driver
//! looks. `billListData` is the one function in this crate a *client* actor writes for
//! somebody else's screen.
//!
//! One scenario, mandatory for both actors:
//!
//! 1. **Energy Broker sends Charging Session Summary to EVSE** — the total cost
//!    [EVCS-001] and energy [EVCS-002] since the car was connected, the cost [EVCS-003]
//!    and amount [EVCS-004] of self-produced energy, and the cost [EVCS-005] and amount
//!    [EVCS-006] of grid energy.
//!
//! # Two names for the client
//!
//! §3.2.2.1 gives the Energy Broker two actor names and makes the choice mean something:
//! [`CEM_ACTOR`] "SHALL be used if Energy Guard and Energy Broker are represented by the
//! same CEM entity", [`ENERGY_BROKER_ACTOR`] "if \[they\] are represented by different CEM
//! entities". Both are conformant; a client that matched only one would miss half the
//! field. [`BROKER_ACTORS`] is the pair.
//!
//! # Reading it back
//!
//! `billListData` is readable as well as writeable, so an energy manager that did not
//! write the summary — a second manager, or one that restarted — can read what the wallbox
//! holds. [`ChargingSummary::read`] is that direction, and it is what makes a session's
//! accounting the *car's* rather than something inferred from the wallbox's power curve.
//!
//! ```
//! use eebus::usecases::emobility::evcs::{ChargingSummary, Share};
//! use eebus::model::Currency;
//!
//! // 18 kWh, €4.20, three quarters of it off the roof and nearly free.
//! let summary = ChargingSummary::new(18_000.0, 4.20, Currency::EUR)
//!     .from_grid(Share::new(25.0, 90.0))
//!     .self_produced(Share::new(75.0, 10.0));
//!
//! assert_eq!(summary.grid_energy_wh(), Some(4_500.0));
//! assert_eq!(summary.self_produced_energy_wh(), Some(13_500.0));
//!
//! // And it survives the wire.
//! let read = ChargingSummary::read(&summary.write()).expect("a summary");
//! assert_eq!(read.energy_wh, summary.energy_wh);
//! ```
//!
//! # Not a bill
//!
//! Tables 6 and 8 both say it: this summary "SHOULD NOT be used for actual billing as it
//! may also contain approximated values". It is what a driver is shown, not what anybody
//! is invoiced.

use alloc::vec;
use alloc::vec::Vec;

use crate::model::{
    BillConstraintsData, BillConstraintsListData, BillCostId, BillCostType, BillData,
    BillDataPosition, BillDataPositionCost, BillDataPositionValue, BillDataTotal,
    BillDataTotalCost, BillDataTotalValue, BillDescriptionData, BillDescriptionListData, BillId,
    BillListData, BillPositionId, BillPositionType, BillType, BillValueId, CmdData, Currency,
    EntityType, FeatureType, Function, Role, ScaledNumber, UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

/// The version this implementation speaks.
///
/// The corpus in `tests/fixtures/devices` has wallboxes announcing both `1.0.0` (the
/// Porsche Mobile Charger Connect) and `1.0.1` (Elli, Spelsberg, Kostal). The two differ
/// in editorial matters only; this is the current one.
pub const VERSION: &str = "1.0.1";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "evChargingSummary";

/// The actor that holds the summary and shows it: the wallbox.
pub const EVSE_ACTOR: &str = "EVSE";

/// The actor that writes it, where the Energy Guard and the Energy Broker are **separate**
/// CEM entities (§3.2.2.1).
pub const ENERGY_BROKER_ACTOR: &str = "EnergyBroker";

/// The actor that writes it, where the Energy Guard and the Energy Broker are the **same**
/// CEM entity (§3.2.2.1).
///
/// The commoner of the two in the field: the Kostal Smart Energy Meter in
/// `tests/fixtures/devices` announces this use case exactly this way.
pub const CEM_ACTOR: &str = "CEM";

/// Both names the client actor may announce itself under, in the order §3.2.2.1 lists them.
pub const BROKER_ACTORS: [&str; 2] = [CEM_ACTOR, ENERGY_BROKER_ACTOR];

/// The `billId` **this** implementation publishes the summary under.
///
/// A local choice: Table 6 spells it `<x1>` and marks it the PRIMARY IDENTIFIER, so a peer
/// picks its own. Find a peer's with [`find_summary`] rather than assuming this one.
pub const BILL_ID: BillId = BillId(1);

/// The `billType` the summary carries (Tables 6 and 8).
pub const SUMMARY: BillType = BillType::ChargingSummary;

/// The identifier this implementation gives the one total value and the one total cost.
const TOTAL_VALUE: BillValueId = BillValueId(1);
const TOTAL_COST: BillCostId = BillCostId(1);

// ---- the feature an EVSE serves ------------------------------------------------------

/// Builds the `Bill` feature scenario 1 is served from (Table 5).
///
/// `billListData` is read **and write**, which is the whole shape of this use case: the
/// wallbox holds the summary and the energy manager is the one that knows what it should
/// say. The description and the constraints are the wallbox's own statement of what it
/// will hold, and are read-only for the same reason every other description in this crate
/// is.
pub fn bill_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::Bill, Role::Server)
        .with_function(Function::BillDescriptionListData, Operations::read())
        .with_function(Function::BillConstraintsListData, Operations::read())
        .with_function(Function::BillListData, Operations::read_write())
}

// ---- what an EVSE publishes ----------------------------------------------------------

/// The description of the summary the wallbox will hold (Table 6).
///
/// `update_required` is [EVCS-009], and it is the wallbox's way of *asking*: setting it
/// tells the responsible client that the charging process has finished and the summary
/// should be written. Table 6 requires the server to set it back to `false` once the
/// write has arrived — [`summary_description`] is how it says both.
pub fn summary_description(update_required: bool) -> CmdData {
    CmdData::BillDescriptionListData(BillDescriptionListData {
        bill_description_data: Some(vec![BillDescriptionData {
            bill_id: Some(BILL_ID),
            // Table 6: "If set to false or omitted, the corresponding billData SHALL NOT
            // be writeable" — and a summary nobody may write is a summary nobody can fill.
            bill_writeable: Some(true),
            update_required: Some(update_required),
            supported_bill_type: Some(vec![SUMMARY]),
            ..Default::default()
        }]),
    })
}

/// How many positions the wallbox will hold (Table 7).
///
/// `R` rather than `M`, and worth publishing: a summary has two positions — the grid share
/// and the self-produced one — so a `positionCountMax` below two is a wallbox that cannot
/// hold a complete summary, and the specification's own `≥2` says so.
pub fn summary_constraints(max_positions: u32) -> CmdData {
    CmdData::BillConstraintsListData(BillConstraintsListData {
        bill_constraints_data: Some(vec![BillConstraintsData {
            bill_id: Some(BILL_ID),
            position_count_min: Some(BillPositionId(0)),
            position_count_max: Some(BillPositionId(max_positions)),
        }]),
    })
}

/// Finds the `billId` a peer publishes its charging summary under.
///
/// `<x1>`: the wallbox numbers its own bills, and a client that assumed `1` would write a
/// charging summary over whatever else that wallbox keeps under bill 1. Matched on
/// `supportedBillType` containing `chargingSummary`, which Table 6 fixes.
///
/// Returns the identifier and whether the wallbox is currently *asking* for an update
/// ([EVCS-009]) — the two come from the same entry, and reading them apart would mean
/// walking the list twice.
pub fn find_summary(data: &CmdData) -> Option<(BillId, bool)> {
    let CmdData::BillDescriptionListData(list) = data else {
        return None;
    };
    list.bill_description_data
        .iter()
        .flatten()
        .find(|entry| {
            entry
                .supported_bill_type
                .iter()
                .flatten()
                .any(|kind| kind == &SUMMARY)
                // Table 6: a bill that is not writeable is one this use case cannot use.
                && entry.bill_writeable == Some(true)
        })
        .and_then(|entry| Some((entry.bill_id?, entry.update_required.unwrap_or(false))))
}

// ---- the summary itself --------------------------------------------------------------

/// One share of a charging session: how much of the energy, and how much of the cost.
///
/// Both are percentages of the session's total, which is how Table 8 models a position:
/// `value.valuePercentage` and `cost.costPercentage`. They are *not* the same number — a
/// quarter of the energy bought from the grid at the retail tariff is most of the bill —
/// and keeping them apart is the point of the use case.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Share {
    /// The percentage of the session's energy this share accounts for.
    pub energy_percent: f64,
    /// The percentage of the session's cost it accounts for.
    pub cost_percent: f64,
}

impl Share {
    /// A share of the energy and a share of the cost, both as percentages.
    pub fn new(energy_percent: f64, cost_percent: f64) -> Self {
        Self {
            energy_percent,
            cost_percent,
        }
    }
}

/// What a charging session came to.
///
/// Everything since the car was connected [EVCS-007], and the wallbox has to be able to
/// deliver it for a minute after it is disconnected [EVCS-008] — so a summary written just
/// after the cable comes out is still a summary, not a late message.
#[derive(Clone, Debug, PartialEq)]
pub struct ChargingSummary {
    /// The total energy delivered, in watt-hours [EVCS-002]. Table 8 fixes the unit.
    pub energy_wh: f64,
    /// The total cost [EVCS-001], in [`currency`](Self::currency).
    pub cost: f64,
    /// The currency the cost is in.
    pub currency: Currency,
    /// The share that came from the grid [EVCS-005], [EVCS-006].
    pub from_grid: Option<Share>,
    /// The share that came from the building's own generation [EVCS-003], [EVCS-004].
    pub self_produced: Option<Share>,
    /// The `billId` it belongs to. [`BILL_ID`] unless a peer's was read first.
    pub bill: BillId,
}

impl ChargingSummary {
    /// A session's totals, with no split yet.
    pub fn new(energy_wh: f64, cost: f64, currency: Currency) -> Self {
        Self {
            energy_wh,
            cost,
            currency,
            from_grid: None,
            self_produced: None,
            bill: BILL_ID,
        }
    }

    /// Records the grid's share.
    #[must_use]
    pub fn from_grid(mut self, share: Share) -> Self {
        self.from_grid = Some(share);
        self
    }

    /// Records the roof's share.
    #[must_use]
    pub fn self_produced(mut self, share: Share) -> Self {
        self.self_produced = Some(share);
        self
    }

    /// Addresses it to the `billId` this peer publishes, rather than to this crate's.
    #[must_use]
    pub fn for_bill(mut self, bill: BillId) -> Self {
        self.bill = bill;
        self
    }

    /// The grid share in watt-hours, where the share is known.
    pub fn grid_energy_wh(&self) -> Option<f64> {
        Some(self.energy_wh * self.from_grid?.energy_percent / 100.0)
    }

    /// The self-produced share in watt-hours.
    pub fn self_produced_energy_wh(&self) -> Option<f64> {
        Some(self.energy_wh * self.self_produced?.energy_percent / 100.0)
    }

    /// What the grid share cost.
    pub fn grid_cost(&self) -> Option<f64> {
        Some(self.cost * self.from_grid?.cost_percent / 100.0)
    }

    /// What the self-produced share cost.
    pub fn self_produced_cost(&self) -> Option<f64> {
        Some(self.cost * self.self_produced?.cost_percent / 100.0)
    }

    /// Whether the two shares account for the whole session.
    ///
    /// Nothing on the wire enforces it and Table 8 does not state it as a rule, but a
    /// summary whose shares do not add up is one a driver will read as wrong. A tenth of a
    /// percent of slack, because the shares are `ScaledNumber`s and came back rounded.
    pub fn shares_add_up(&self) -> bool {
        let (Some(grid), Some(own)) = (self.from_grid, self.self_produced) else {
            return false;
        };
        (grid.energy_percent + own.energy_percent - 100.0).abs() <= 0.1
            && (grid.cost_percent + own.cost_percent - 100.0).abs() <= 0.1
    }

    /// Builds the `billListData` an Energy Broker writes (Table 8).
    ///
    /// Every element Table 8 marks `M \W` is here; a share that was not recorded produces
    /// no position rather than a position of zero, because "no self-production was
    /// measured" and "none was used" are different claims.
    pub fn write(&self) -> CmdData {
        let mut position = Vec::new();
        if let Some(grid) = self.from_grid {
            position.push(self.position(1, BillPositionType::GridElectricEnergy, grid));
        }
        if let Some(own) = self.self_produced {
            position.push(self.position(2, BillPositionType::SelfProducedElectricEnergy, own));
        }

        CmdData::BillListData(BillListData {
            bill_data: Some(vec![BillData {
                bill_id: Some(self.bill),
                bill_type: Some(SUMMARY),
                total: Some(BillDataTotal {
                    value: Some(vec![BillDataTotalValue {
                        value_id: Some(TOTAL_VALUE),
                        unit: Some(UnitOfMeasurement::Wh),
                        value: Some(ScaledNumber::from_f64(self.energy_wh, 0)),
                    }]),
                    cost: Some(vec![BillDataTotalCost {
                        cost_id: Some(TOTAL_COST),
                        cost_type: Some(BillCostType::AbsolutePrice),
                        value_id: Some(TOTAL_VALUE),
                        currency: Some(self.currency.clone()),
                        cost: Some(ScaledNumber::from_f64(self.cost, 2)),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                position: Some(position),
                ..Default::default()
            }]),
        })
    }

    fn position(&self, id: u32, kind: BillPositionType, share: Share) -> BillDataPosition {
        BillDataPosition {
            position_id: Some(BillPositionId(id)),
            position_type: Some(kind),
            value: Some(vec![BillDataPositionValue {
                value_id: Some(TOTAL_VALUE),
                value_percentage: Some(ScaledNumber::from_f64(share.energy_percent, 2)),
                ..Default::default()
            }]),
            cost: Some(vec![BillDataPositionCost {
                cost_id: Some(TOTAL_COST),
                cost_percentage: Some(ScaledNumber::from_f64(share.cost_percent, 2)),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    /// Reads a `billListData` back.
    ///
    /// **Give it the resolved state, not a partial update**: an omitted element means
    /// *unchanged* (SPINE IG §3.3), and a fragment read as a whole summary is a session
    /// nobody had.
    ///
    /// Returns [`None`] for a payload that is not a charging summary, or one without the
    /// total energy and cost Table 8 makes mandatory — a summary missing either is not a
    /// summary, and reporting one with a zero in it would be a session that cost nothing.
    pub fn read(data: &CmdData) -> Option<Self> {
        let CmdData::BillListData(list) = data else {
            return None;
        };
        let entry = list
            .bill_data
            .iter()
            .flatten()
            .find(|entry| entry.bill_type.as_ref() == Some(&SUMMARY))?;
        let total = entry.total.as_ref()?;

        let value = total
            .value
            .iter()
            .flatten()
            .find(|value| value.unit.as_ref() == Some(&UnitOfMeasurement::Wh))?;
        let cost = total
            .cost
            .iter()
            .flatten()
            .find(|cost| cost.cost_type.as_ref() == Some(&BillCostType::AbsolutePrice))?;

        let mut summary = Self {
            energy_wh: value.value.as_ref().and_then(ScaledNumber::to_f64)?,
            cost: cost.cost.as_ref().and_then(ScaledNumber::to_f64)?,
            currency: cost.currency.clone()?,
            from_grid: None,
            self_produced: None,
            bill: entry.bill_id.unwrap_or(BILL_ID),
        };

        for position in entry.position.iter().flatten() {
            let Some(share) = share_of(position) else {
                continue;
            };
            match position.position_type.as_ref() {
                Some(BillPositionType::GridElectricEnergy) => summary.from_grid = Some(share),
                Some(BillPositionType::SelfProducedElectricEnergy) => {
                    summary.self_produced = Some(share);
                }
                _ => {}
            }
        }
        Some(summary)
    }
}

fn share_of(position: &BillDataPosition) -> Option<Share> {
    let energy_percent = position
        .value
        .iter()
        .flatten()
        .find_map(|value| value.value_percentage.as_ref())
        .and_then(ScaledNumber::to_f64)?;
    let cost_percent = position
        .cost
        .iter()
        .flatten()
        .find_map(|cost| cost.cost_percentage.as_ref())
        .and_then(ScaledNumber::to_f64)?;
    Some(Share::new(energy_percent, cost_percent))
}

// ---- what an Energy Broker finds -----------------------------------------------------

/// Where one wallbox keeps its charging summary.
#[derive(Clone, Debug, PartialEq)]
pub struct EvseBillPeer {
    /// The peer's device address.
    pub device: crate::model::AddressDevice,
    /// Its `Bill` feature: read for the description, written for the summary.
    pub bill: crate::model::FeatureAddress,
}

/// Finds a wallbox's `Bill` feature from its detailed discovery and use-case data.
///
/// Matched on the **role** rather than on the actor name, and deliberately: §3.2.2 gives
/// the Energy Broker "only client functionality", so the peer announcing this use case and
/// serving a `Bill` server is the EVSE whatever it calls itself. That matters against real
/// firmware — the Porsche Mobile Charger Connect in `tests/fixtures/devices` announces this
/// use case under actor `EV`, which the specification does not define for it, and a lookup
/// keyed on [`EVSE_ACTOR`] alone would not find a wallbox that is plainly there. A CEM
/// announcing the use case as [`CEM_ACTOR`] is excluded by the same rule, because it serves
/// no `Bill`.
///
/// Returns [`None`] until the peer has announced both the use case and the feature.
pub fn locate(remote: &crate::spine::RemoteDevice) -> Option<EvseBillPeer> {
    let found = remote
        .use_cases
        .iter()
        .filter(|use_case| use_case.name.as_str() == NAME)
        .find_map(|use_case| remote.address_of(use_case, &FeatureType::Bill, Role::Server))?;
    Some(EvseBillPeer {
        device: remote.address.clone()?,
        bill: found,
    })
}

// ---- descriptors ---------------------------------------------------------------------

/// The EVSE lives on its own entity type (§3.2.1.1).
const EVSE_ENTITIES: &[EntityType] = &[EntityType::EVSE];
/// The Energy Broker sits on a CEM entity (§3.2.2.1).
const BROKER_ENTITIES: &[EntityType] = &[EntityType::CEM];

const SERVER_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::server(FeatureType::Bill, Function::BillDescriptionListData),
    FunctionUse::server(FeatureType::Bill, Function::BillConstraintsListData),
    FunctionUse::server_writeable(FeatureType::Bill, Function::BillListData),
];

const CLIENT_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::client(FeatureType::Bill, Function::BillDescriptionListData),
    FunctionUse::client(FeatureType::Bill, Function::BillConstraintsListData),
    FunctionUse::client_writes(FeatureType::Bill, Function::BillListData),
];

const SCENARIO_NAME: &str = "Energy Broker sends Charging Session Summary to EVSE";

/// The EVSE: the actor that holds the summary.
pub static EVSE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: EVSE_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: EVSE_ENTITIES,
    counterpart: ENERGY_BROKER_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: SERVER_FUNCTIONS,
    }],
};

/// The Energy Broker, where it is a CEM entity of its own ([`ENERGY_BROKER_ACTOR`]).
pub static ENERGY_BROKER: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: ENERGY_BROKER_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: BROKER_ENTITIES,
    counterpart: EVSE_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: CLIENT_FUNCTIONS,
    }],
};

/// The Energy Broker, where the same CEM entity is also the Energy Guard ([`CEM_ACTOR`]).
///
/// The same actor under the other of the two names §3.2.2.1 permits. Announce this one on
/// a device whose CEM entity plays LPC's Energy Guard as well — which is most of them.
pub static CEM: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: CEM_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: BROKER_ENTITIES,
    counterpart: EVSE_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: CLIENT_FUNCTIONS,
    }],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn a_session() -> ChargingSummary {
        ChargingSummary::new(18_000.0, 4.20, Currency::EUR)
            .from_grid(Share::new(25.0, 90.0))
            .self_produced(Share::new(75.0, 10.0))
    }

    /// [EVCS-001] to [EVCS-006]: the totals and both shares survive the round trip.
    #[test]
    fn a_summary_survives_being_written_and_read_back() {
        let summary = a_session();
        let read = ChargingSummary::read(&summary.write()).expect("a summary");
        assert_eq!(read, summary);
        assert!(read.shares_add_up());
    }

    /// The split is the whole point: a quarter of the energy is nearly all of the bill.
    #[test]
    fn the_energy_split_and_the_cost_split_are_different_numbers() {
        let summary = a_session();
        assert_eq!(summary.grid_energy_wh(), Some(4_500.0));
        assert_eq!(summary.self_produced_energy_wh(), Some(13_500.0));
        assert!((summary.grid_cost().unwrap() - 3.78).abs() < 1e-9);
        assert!((summary.self_produced_cost().unwrap() - 0.42).abs() < 1e-9);
    }

    /// Table 8 fixes the unit and the cost type; a reader that ignored them would take a
    /// kilowatt-hour figure for a watt-hour one.
    #[test]
    fn the_wire_carries_watt_hours_and_an_absolute_price() {
        let CmdData::BillListData(list) = a_session().write() else {
            panic!("expected billListData");
        };
        let entry = &list.bill_data.as_ref().unwrap()[0];
        assert_eq!(entry.bill_type.as_ref(), Some(&SUMMARY));
        let total = entry.total.as_ref().unwrap();
        assert_eq!(
            total.value.as_ref().unwrap()[0].unit.as_ref(),
            Some(&UnitOfMeasurement::Wh)
        );
        assert_eq!(
            total.cost.as_ref().unwrap()[0].cost_type.as_ref(),
            Some(&BillCostType::AbsolutePrice)
        );
        assert_eq!(entry.position.as_ref().unwrap().len(), 2);
    }

    /// A share that was never measured produces no position, rather than a zero.
    #[test]
    fn an_unmeasured_share_is_absent_rather_than_zero() {
        let summary = ChargingSummary::new(18_000.0, 4.20, Currency::EUR);
        let CmdData::BillListData(list) = summary.write() else {
            panic!("expected billListData");
        };
        assert!(
            list.bill_data.as_ref().unwrap()[0]
                .position
                .as_ref()
                .unwrap()
                .is_empty()
        );

        let read = ChargingSummary::read(&summary.write()).expect("a summary");
        assert_eq!(read.self_produced_energy_wh(), None);
        assert!(!read.shares_add_up());
    }

    /// [EVCS-009]: the wallbox asks, and the identifier is its own.
    #[test]
    fn the_description_says_which_bill_and_whether_an_update_is_wanted() {
        assert_eq!(
            find_summary(&summary_description(false)),
            Some((BILL_ID, false))
        );
        assert_eq!(
            find_summary(&summary_description(true)),
            Some((BILL_ID, true)),
            "the wallbox is asking for the summary now"
        );
    }

    /// A bill that is not writeable is not this use case's (Table 6).
    #[test]
    fn a_bill_that_may_not_be_written_is_not_found() {
        let data = CmdData::BillDescriptionListData(BillDescriptionListData {
            bill_description_data: Some(vec![BillDescriptionData {
                bill_id: Some(BillId(7)),
                bill_writeable: Some(false),
                supported_bill_type: Some(vec![SUMMARY]),
                ..Default::default()
            }]),
        });
        assert_eq!(find_summary(&data), None);
    }

    /// Table 7: two positions, so a maximum below two cannot hold a complete summary.
    #[test]
    fn the_constraints_say_how_many_positions_fit() {
        let CmdData::BillConstraintsListData(list) = summary_constraints(2) else {
            panic!("expected the constraints");
        };
        let entry = &list.bill_constraints_data.as_ref().unwrap()[0];
        assert_eq!(entry.position_count_max, Some(BillPositionId(2)));
        assert!(
            entry.position_count_max.is_some_and(|max| max.get() >= 2),
            "Table 7 says ≥2"
        );
    }

    /// §3.2.2.1: the client actor has two conformant names, and both are here.
    #[test]
    fn the_broker_has_two_names_and_both_are_described() {
        assert_eq!(BROKER_ACTORS, [CEM.actor, ENERGY_BROKER.actor]);
        for descriptor in [&ENERGY_BROKER, &CEM] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.role, ActorRole::Client);
            assert_eq!(descriptor.counterpart, EVSE_ACTOR);
            assert_eq!(descriptor.required_scenarios().collect::<Vec<_>>(), [1]);
        }
        assert_eq!(EVSE.role, ActorRole::Server);
        assert!(EVSE.permits_entity(&EntityType::EVSE));
        assert!(CEM.permits_entity(&EntityType::CEM));
    }

    /// The one function in this crate a client writes on somebody else's screen.
    #[test]
    fn the_bill_list_is_the_writeable_one() {
        let feature = bill_feature(1);
        let writeable = |function| {
            feature
                .function(&function)
                .is_some_and(|entry| entry.operations.write)
        };
        assert!(writeable(Function::BillListData));
        assert!(!writeable(Function::BillDescriptionListData));
        assert!(!writeable(Function::BillConstraintsListData));
    }
}
