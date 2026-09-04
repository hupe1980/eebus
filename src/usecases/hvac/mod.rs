//! Heating, ventilation, air conditioning — and, mostly, hot water.
//!
//! The grid use cases in this crate all point the same way: [`limitation`] sets a ceiling,
//! [`emobility::opev`] sets a current not to exceed, [`cob`] drives a battery between
//! limits. Every one of them can only ask an appliance to do **less**, and a ceiling an
//! appliance is already under changes nothing at all.
//!
//! This family is where that stops being true. A hot water tank is the cheapest thermal
//! battery most buildings have, and asking a heat pump for a higher tank temperature while
//! the roof is exporting stores kilowatt-hours that would otherwise be sold at the
//! feed-in tariff and bought back at the retail one. No limit can express that.
//!
//! Three use cases, and they are three parts of one thing:
//!
//! * [`mdsf`] — **Monitoring of DHW System Function**: which operation mode the hot water
//!   is in (`auto`, `on`, `off`, `eco`), and whether a one-time heating is running.
//! * [`cdt`] — **Configuration of DHW Temperature**: the temperature setpoint itself.
//! * [`mdt`] — **Monitoring of DHW Temperature**: what the water actually got to.
//!
//! Written as an energy manager uses them: `mdsf` says whether a write will reach
//! anything, `cdt` writes it, `mdt` says whether it worked.
//!
//! # Why both
//!
//! CDT §2.4.2 and §2.4.3 are unusually firm about it: a DHW Circuit that does not serve
//! "Monitoring of DHW System Function" **SHALL** serve "Configuration of DHW System
//! Function" [CDT-005]. One of the two is mandatory, and it is not decoration — CDT's
//! setpoints are addressed *through* the operation modes. Table 10 relates each mode to
//! the setpoints it uses, so "write 60 °C" is only a complete instruction once you know
//! which mode the circuit is in and which setpoint that mode reads. A Configuration
//! Appliance holding only CDT can write a temperature and have it apply to a mode the
//! circuit is not in.
//!
//! They also share their identifiers, and the specification says so rather than leaving it
//! to taste. `systemFunctionId` is the same number in CDT and MDSF ([`SYSTEM_FUNCTION_ID`]
//! here); CDT's `operationModeId`s are the ones MDSF describes (CDT Table 10's two
//! footnotes); and CDT's setpoint `measurementId` SHALL be the one MDT publishes
//! (§3.2.1.2.2.1), which is what
//! [`cdt::setpoint_description_measuring`] takes.
//!
//! # What is not here
//!
//! Nine further HVAC use cases are specified in the same two documents: the room heating
//! and cooling temperatures, their system functions, the outdoor temperature, and
//! "Configuration of DHW System Function" — the writeable counterpart of [`mdsf`], which a
//! circuit may serve *instead* of it to satisfy [CDT-005].
//!
//! [`limitation`]: crate::usecases::limitation
//! [`emobility::opev`]: crate::usecases::emobility::opev
//! [`cob`]: crate::usecases::cob

use crate::model::{
    CmdData, HvacOperationModeId, HvacOperationModeType, HvacSystemFunctionDescriptionData,
    HvacSystemFunctionDescriptionListData, HvacSystemFunctionId, HvacSystemFunctionType,
};

pub mod cdt;
pub mod mdsf;
pub mod mdt;

/// The `systemFunctionId` **this** implementation publishes the DHW system function under.
///
/// A local choice, `<sf1#(1..1)>` — but one with a cross-use-case obligation attached: CDT
/// Table 10 and MDSF Table 9 both address the *same* system function, and the number has
/// to agree between them on one device. Everything in this family uses this constant for
/// that reason, and a peer's is found with [`find_dhw_system_function`].
pub const SYSTEM_FUNCTION_ID: HvacSystemFunctionId = HvacSystemFunctionId(1);

/// The `systemFunctionType` that marks the hot water (MDSF Table 9, CDT §3.2.1.2.3.1).
pub const DHW: HvacSystemFunctionType = HvacSystemFunctionType::Dhw;

/// The system function description both use cases publish (MDSF Table 9).
pub fn system_function_description() -> CmdData {
    CmdData::HvacSystemFunctionDescriptionListData(HvacSystemFunctionDescriptionListData {
        hvac_system_function_description_data: Some(alloc::vec![
            HvacSystemFunctionDescriptionData {
                system_function_id: Some(SYSTEM_FUNCTION_ID),
                system_function_type: Some(DHW),
                ..Default::default()
            }
        ]),
    })
}

/// Finds the `systemFunctionId` a peer publishes its **hot water** under.
///
/// An HVAC feature carries every system function the appliance has — a heat pump serves
/// heating and hot water from the same one, and `<sf1#(1..1)>` is the appliance's own
/// numbering. `systemFunctionType` is the element the specification fixes, so that is what
/// this matches on: a client that assumed `1` would read the *heating* circuit's operation
/// mode and write the hot water setpoint against it.
pub fn find_dhw_system_function(data: &CmdData) -> Option<HvacSystemFunctionId> {
    let CmdData::HvacSystemFunctionDescriptionListData(list) = data else {
        return None;
    };
    list.hvac_system_function_description_data
        .iter()
        .flatten()
        .find(|entry| entry.system_function_type.as_ref() == Some(&DHW))
        .and_then(|entry| entry.system_function_id)
}

/// The `operationModeId`s a peer gave each DHW operation mode.
///
/// `<om1#(2..4)>`: a circuit supports **two or more** of `auto`, `on`, `off` and `eco`
/// (MDSF §2.3.1.1) and numbers them itself, and exactly one of them is enabled at any
/// moment [MDSF-001]. `operationModeType` is what the specification fixes.
///
/// This is what both use cases in the family read the modes through, which is what keeps
/// [`cdt::DhwSetpoints::for_mode`] and [`mdsf::DhwSystemFunction::mode`] talking about the
/// same numbers.
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
