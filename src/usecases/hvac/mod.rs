//! Heating, ventilation, air conditioning — the hot water, and the building around it.
//!
//! Every grid use case in this crate points the same way: [`limitation`] sets a ceiling,
//! [`emobility::opev`] a current not to exceed, [`cob`] a battery between limits. A ceiling
//! an appliance is already under changes nothing, so a manager holding only those can never
//! spend a surplus. This family sets *targets*, and a target can go up.
//!
//! # All twelve of them, as three exchanges
//!
//! | | monitor | configure |
//! |---|---|---|
//! | **hot water** — mode & overrun | [`mdsf`] | [`cdsf`] |
//! | **hot water** — temperature | [`mdt`] | [`cdt`] |
//! | **room heating** — mode | [`mrhsf`] | [`crhsf`] |
//! | **room cooling** — mode | [`mrcsf`] | [`crcsf`] |
//! | **room** — temperature | [`mrt`] | [`crht`], [`crct`] |
//! | **outdoors** — temperature | [`mot`] | — |
//!
//! * [`system_function`] — the operation mode. Six use cases, told apart by
//!   `systemFunctionType` and by whether the actor may write.
//! * [`setpoint`] — the temperature setpoint. Three, told apart by `scopeType` and by the
//!   system function their relations name.
//! * [`temperature`] — the thermometer. Three, told apart by `scopeType`.
//!
//! # How a manager uses them
//!
//! 1. **Will the write reach anything?** A setpoint is addressed *through* an operation
//!    mode, so one written into a mode the appliance is not in is applied, acknowledged and
//!    changes nothing. [`system_function::SystemFunction::current_setpoints`] is the join;
//!    [`setpoint::Setpoints::write_effective`] is the gate.
//! 2. **Ask.** [`cdt`], [`crht`] and [`crct`] write the temperature;
//!    [`cdsf`], [`crhsf`] and [`crcsf`] change the mode; [`cdsf`] scenario 2 starts a
//!    one-time hot water loading outright.
//! 3. **Did it work?** [`mdt`] and [`mrt`] are thermometers. A setpoint is a request, and
//!    what the water and the room got to is a different number.
//!
//! [`mrt`], [`mot`] and the heat delivered between them are also the three signals a
//! building's thermal model is fitted from — which is what turns a forecast into a
//! compressor schedule for [`ohpcf`](crate::usecases::ohpcf).
//!
//! # Two rules that hold across the whole family
//!
//! **Nothing here binds.** All twelve say "Binding SHOULD NOT be used for this Scenario",
//! including the six that write — the opposite of every grid use case, where an unbound
//! write is refused with `errorNumber` 9. The writeable features here are built with
//! [`with_unbound_writes`](crate::spine::LocalFeature::with_unbound_writes) and
//! [`with_deferred_writes`](crate::spine::LocalFeature::with_deferred_writes): a server that
//! insisted on a binding would refuse every conformant Configuration Appliance, and what
//! replaces it is the application's own decision. See
//! [`WriteBinding`](crate::spine::WriteBinding).
//!
//! **One entity holds one feature of a type** (§3.2.2.2.1). So a heat pump that heats water,
//! heats a room and cools it publishes all three system functions in the *same* lists on the
//! *same* `HVAC` feature, and both room setpoints on the same `Setpoint` feature under the
//! same `scopeType: roomAirTemperature`. Every reader is therefore told which system
//! function it follows, and the setpoint relations are keyed by `systemFunctionId` **and**
//! `operationModeId`. [`system_function`] and [`setpoint`] set out what follows from that.
//!
//! # Why the pairs go together
//!
//! [CDT-005], [CRHT-005] and [CRCT-005]: a server that does not serve the monitoring
//! system-function use case **SHALL** serve the configuration one. One of the two is
//! mandatory because the setpoints are addressed through the modes, so a Configuration
//! Appliance holding only the temperature half can write a value that applies to a mode the
//! appliance is not in.
//!
//! They share their identifiers, and the specifications say so rather than leaving it to
//! taste: [`system_function_id`] is the same number in all three DHW use cases, the
//! `operationModeId`s are the ones the system-function use case describes, and the
//! setpoint's `measurementId` SHALL be the one the matching thermometer publishes
//! (§3.2.1.2.2.1) — which is what [`cdt::setpoint_description_measuring`] takes.
//!
//! [`limitation`]: crate::usecases::limitation
//! [`emobility::opev`]: crate::usecases::emobility::opev
//! [`cob`]: crate::usecases::cob

use crate::model::{
    CmdData, HvacOperationModeId, HvacOperationModeType, HvacSystemFunctionId,
    HvacSystemFunctionType,
};

pub mod cdsf;
pub mod cdt;
pub mod crcsf;
pub mod crct;
pub mod crhsf;
pub mod crht;
pub mod mdsf;
pub mod mdt;
pub mod mot;
pub mod mrcsf;
pub mod mrhsf;
pub mod mrt;
pub mod setpoint;
pub mod system_function;
pub mod temperature;

/// The `systemFunctionId` **this** implementation publishes each system function under.
///
/// Local choices, `<sf1#(1..1)>` — but with a cross-use-case obligation attached, and one
/// the specifications spell out: the monitoring and the configuration use case of a
/// function address the *same* number, and so does the temperature use case that writes
/// setpoints into it. Everything in this family goes through this function for that reason.
///
/// **Three different numbers**, because one `HVAC` feature carries every function the
/// appliance has (§3.2.2.2.1 gives an entity at most one feature of a type). An appliance
/// that heated water and a room under the same identifier would be publishing one system
/// function that claims to be both, and a client would read whichever description arrived
/// last.
///
/// A peer's is never assumed: it is found by `systemFunctionType`, which is what
/// [`find_system_function`] and
/// [`SystemFunction`](system_function::SystemFunction) do.
pub const fn system_function_id(kind: &HvacSystemFunctionType) -> HvacSystemFunctionId {
    HvacSystemFunctionId(match kind {
        HvacSystemFunctionType::Dhw => 1,
        HvacSystemFunctionType::Heating => 2,
        HvacSystemFunctionType::Cooling => 3,
        HvacSystemFunctionType::Ventilation => 4,
        // The enumeration is extensible. A vendor-specific function this crate does not
        // name is published under the same number as ventilation only if a caller asks for
        // it by hand, which nothing here does.
        _ => 5,
    })
}

/// The `systemFunctionType` that marks the hot water (MDSF Table 9, CDT §3.2.1.2.3.1).
pub const DHW: HvacSystemFunctionType = HvacSystemFunctionType::Dhw;

/// The `systemFunctionType` that marks room heating (MRHSF, CRHT).
pub const HEATING: HvacSystemFunctionType = HvacSystemFunctionType::Heating;

/// The `systemFunctionType` that marks room cooling (MRCSF, CRCT).
pub const COOLING: HvacSystemFunctionType = HvacSystemFunctionType::Cooling;

/// Finds the `systemFunctionId` a peer publishes one kind of system function under.
///
/// An `HVAC` feature carries every system function the appliance has — a heat pump serves
/// heating and hot water from the same one, and `<sf1#(1..1)>` is the appliance's own
/// numbering. `systemFunctionType` is the element the specification fixes, so that is what
/// this matches on: a client that assumed `1` would read the *heating* circuit's operation
/// mode and write the hot water setpoint against it.
pub fn find_system_function(
    data: &CmdData,
    kind: &HvacSystemFunctionType,
) -> Option<HvacSystemFunctionId> {
    let CmdData::HvacSystemFunctionDescriptionListData(list) = data else {
        return None;
    };
    list.hvac_system_function_description_data
        .iter()
        .flatten()
        .find(|entry| entry.system_function_type.as_ref() == Some(kind))
        .and_then(|entry| entry.system_function_id)
}

/// The `operationModeId`s a peer gave each DHW operation mode.
///
/// `<om1#(2..4)>`: a circuit supports **two or more** of `auto`, `on`, `off` and `eco`
/// (MDSF §2.3.1.1) and numbers them itself, and exactly one of them is enabled at any
/// moment [MDSF-001]. `operationModeType` is what the specification fixes.
///
/// This is what both use cases in the family read the modes through, which is what keeps
/// [`setpoint::Setpoints::for_mode`] and [`system_function::SystemFunction::mode`] talking
/// about the same numbers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OperationModes {
    known: alloc::vec::Vec<(HvacOperationModeId, HvacOperationModeType)>,
}

impl OperationModes {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records what one `hvacOperationModeDescriptionListData` says.
    pub fn learn(&mut self, data: &CmdData) -> bool {
        let CmdData::HvacOperationModeDescriptionListData(list) = data else {
            return false;
        };
        for entry in list.hvac_operation_mode_description_data.iter().flatten() {
            let (Some(id), Some(kind)) =
                (entry.operation_mode_id, entry.operation_mode_type.clone())
            else {
                continue;
            };
            match self.known.iter_mut().find(|(known, _)| *known == id) {
                Some((_, stored)) => *stored = kind,
                None => self.known.push((id, kind)),
            }
        }
        true
    }

    /// The identifier a named mode is published under.
    pub fn id_of(&self, kind: &HvacOperationModeType) -> Option<HvacOperationModeId> {
        self.known
            .iter()
            .find(|(_, known)| known == kind)
            .map(|(id, _)| *id)
    }

    /// What the mode with this identifier is.
    pub fn kind_of(&self, id: HvacOperationModeId) -> Option<&HvacOperationModeType> {
        self.known
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, kind)| kind)
    }

    /// Every mode described, in the order the circuit gave them.
    pub fn all(&self) -> impl Iterator<Item = (HvacOperationModeId, &HvacOperationModeType)> {
        self.known.iter().map(|(id, kind)| (*id, kind))
    }

    /// Whether the circuit described the two modes §2.3.1.1 requires of it.
    ///
    /// Fewer than two is a circuit that cannot be in a *different* mode, which is what the
    /// use case exists to report. Worth checking at commissioning rather than being
    /// surprised later.
    pub fn is_sufficient(&self) -> bool {
        self.known.len() >= 2
    }

    /// Whether any mode has been described.
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}
