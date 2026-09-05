//! Which elements identify a list entry, and which functions merge entry by entry.
//!
//! SPINE Resource Specification §3.4 names three kinds of identifier and only two of them
//! are identity. A **PRIMARY** identifier addresses the entry and a **SUB** identifier adds
//! a dimension to it; a **FOREIGN** identifier "is used to refer to other functionality on
//! the same entity" and "is **not** used to create further dimensions of list entries"
//! (§3.4.2.1). Getting that distinction wrong produces no error anywhere: a partial update
//! whose entry does not match anything stored is *appended*, so a value that changes and
//! is wrongly counted as identity turns every change into a second entry.
//!
//! The specification carries the distinction in prose, one table per function, and the
//! generator reproduces it from Annex B.7's Table 358 plus a class test — see
//! `leading_identifiers` in `xtask/src/codegen.rs`. This file is the other half: the
//! answers, written down, so that a change to the rule is a failing test with a diff
//! rather than a silent change to what a peer's data merges into.

use eebus::model::CmdData;
use eebus::model::rfe::Identified;

/// The identity of every entry type this crate's use cases exchange.
///
/// Each row is the Resource Specification's own answer. Where an element the schema places
/// in the same run is *absent* here, the reason is in the third column and is a FOREIGN
/// identifier every time.
#[test]
fn every_exchanged_entry_type_is_identified_as_the_specification_says() {
    macro_rules! identity {
        ($ty:ty, $fields:expr, $why:literal) => {
            assert_eq!(
                <$ty as Identified>::IDENTIFIER_FIELDS,
                $fields,
                concat!(stringify!($ty), ": ", $why)
            );
        };
    }
    use eebus::model::*;

    // LPC/LPP. The limit and its description are keyed by `limitId` alone; the
    // description's `measurementId` is a FOREIGN identifier into Measurement (§4.3.15.1).
    identity!(LoadControlLimitData, ["limit_id"], "Table 22, PRIMARY");
    identity!(
        LoadControlLimitDescriptionData,
        ["limit_id"],
        "measurementId is FOREIGN — 'LoadControl can use the FOREIGN IDENTIFIER \
         measurementId from the Measurement Feature Type'"
    );
    identity!(LoadControlLimitConstraintsData, ["limit_id"], "PRIMARY");

    // MPC/MGCP/EVCEM/EVSOC/MDT — the measurement layer.
    identity!(MeasurementData, ["measurement_id"], "PRIMARY");
    identity!(MeasurementDescriptionData, ["measurement_id"], "PRIMARY");
    identity!(MeasurementConstraintsData, ["measurement_id"], "PRIMARY");
    identity!(
        ElectricalConnectionParameterDescriptionData,
        ["electrical_connection_id", "parameter_id"],
        "PRIMARY + SUB; the trailing measurementId is FOREIGN (Table 52)"
    );
    identity!(
        ElectricalConnectionDescriptionData,
        ["electrical_connection_id"],
        "PRIMARY"
    );
    identity!(
        ElectricalConnectionCharacteristicData,
        [
            "electrical_connection_id",
            "parameter_id",
            "characteristic_id"
        ],
        "three dimensions, all ElectricalConnection's own"
    );

    // DeviceConfiguration — the failsafe values and MGCP's curtailment factor.
    identity!(DeviceConfigurationKeyValueData, ["key_id"], "PRIMARY");
    identity!(
        DeviceConfigurationKeyValueDescriptionData,
        ["key_id"],
        "PRIMARY"
    );

    // CDT/MDSF — the hot water setpoint and the mode that governs it.
    identity!(SetpointData, ["setpoint_id"], "PRIMARY");
    identity!(
        SetpointDescriptionData,
        ["setpoint_id"],
        "Table 117: measurementId and timeTableId are both FOREIGN. Requiring them would \
         refuse a conformant description that links to neither"
    );
    identity!(
        HvacSystemFunctionData,
        ["system_function_id"],
        "Table 64: currentOperationModeId 'SHALL be set as FOREIGN IDENTIFIER'. It is the \
         mode the function is *in*, so counting it as identity makes every mode change a \
         second system function"
    );
    identity!(
        HvacSystemFunctionSetpointRelationData,
        ["system_function_id", "operation_mode_id"],
        "Table 66: PRIMARY + SUB. Both HVAC's own, and one entry per (function, mode)"
    );
    identity!(
        HvacOperationModeDescriptionData,
        ["operation_mode_id"],
        "PRIMARY"
    );
    identity!(HvacOverrunData, ["overrun_id"], "PRIMARY");

    // OHPCF — §3.4.1's own worked example of a two-dimensional identity: "a 'slot' with
    // number 2 of 'sequence' 5 is different to 'slot' 2 of 'sequence' 7".
    identity!(
        PowerTimeSlotScheduleData,
        ["sequence_id", "slot_number"],
        "§3.4.1: identifying a slot takes sequenceId AND slotNumber"
    );
    identity!(
        PowerTimeSlotValueData,
        ["sequence_id", "slot_number"],
        "§3.4.1"
    );

    // EVCS.
    identity!(BillData, ["bill_id"], "PRIMARY");
    identity!(
        BillDescriptionData,
        ["bill_id"],
        "PRIMARY; sessionId is FOREIGN"
    );

    // And the two that are keyed by an identifier type another class declares — which is
    // fine, because the *first* identifier is the function's own wherever it comes from.
    identity!(
        NodeManagementBindingDataBindingEntry,
        ["binding_id"],
        "BindingIdType is declared in BindingManagement and is still this entry's PRIMARY"
    );
    identity!(
        NodeManagementSubscriptionDataSubscriptionEntry,
        ["subscription_id"],
        "SubscriptionIdType, declared in SubscriptionManagement"
    );
}

/// A FOREIGN identifier that changes must not split the list it is in.
///
/// The concrete failure the rule above prevents, over the function it actually bites on: a
/// DHW circuit reports which operation mode it is in through `currentOperationModeId`, and
/// notifies a change partially. Counted as identity, the change matches nothing stored and
/// is appended — leaving two `hvacSystemFunctionData` entries for one system function, and
/// `hvac::system_function::SystemFunction` reading whichever it finds first, for ever.
#[test]
fn a_mode_change_updates_the_system_function_rather_than_adding_a_second() {
    use eebus::model::{
        HvacOperationModeId, HvacSystemFunctionData, HvacSystemFunctionId,
        HvacSystemFunctionListData,
    };

    let entry = |mode: u32| HvacSystemFunctionData {
        system_function_id: Some(HvacSystemFunctionId(1)),
        current_operation_mode_id: Some(HvacOperationModeId(mode)),
        ..Default::default()
    };

    let mut stored = HvacSystemFunctionListData {
        hvac_system_function_data: Some(vec![entry(1)]),
    };
    eebus::model::rfe::apply_partial(
        &mut stored,
        HvacSystemFunctionListData {
            hvac_system_function_data: Some(vec![entry(2)]),
        },
    );

    let held = stored.hvac_system_function_data.as_ref().unwrap();
    assert_eq!(held.len(), 1, "one system function, not two");
    assert_eq!(
        held[0].current_operation_mode_id,
        Some(HvacOperationModeId(2)),
        "and it is in the mode it just reported"
    );
}

/// A description that links to nothing still has an identity.
///
/// Table 117 makes `measurementId` and `timeTableId` conditional — "Otherwise it SHALL be
/// omitted" — so a circuit publishing a setpoint that relates to neither is conformant.
/// Requiring them for identity refuses it: use-case IG §3.1 has the engine reject a list
/// entry whose identifiers are incomplete.
#[test]
fn a_setpoint_description_that_links_to_nothing_is_still_addressable() {
    use eebus::model::{SetpointDescriptionData, SetpointDescriptionListData};

    let bare = SetpointDescriptionData {
        setpoint_id: Some(eebus::model::SetpointId(3)),
        ..Default::default()
    };
    assert!(bare.has_identity());
    assert!(
        CmdData::SetpointDescriptionListData(SetpointDescriptionListData {
            setpoint_description_data: Some(vec![bare]),
        })
        .entries_identified(),
        "the engine would otherwise answer a conformant description with `MissingIdentifier`"
    );
}

/// Which functions merge entry by entry, pinned.
///
/// A function counts as a mergeable list when its entries carry identifiers: a partial
/// update then addresses entries within it, and a delete removes the ones it names. A
/// function that does not — because SPINE gives its entries no identifier — can only be
/// *replaced* by a partial update, and a delete clears it outright.
///
/// That is a real difference in what a peer's message does to stored state, and it is
/// derived rather than declared, so it is pinned here. **A diff in this list is a review
/// item, not a test to update in passing**: a function that moves from the second list to
/// the first starts merging where it used to replace.
#[test]
fn the_set_of_functions_that_merge_entry_by_entry_is_the_reviewed_one() {
    let mut mergeable: Vec<&str> = Vec::new();
    let mut replaced: Vec<&str> = Vec::new();
    for key in CmdData::KEYS {
        let Some(empty) = CmdData::empty(key) else {
            continue;
        };
        if empty.is_mergeable_list() {
            mergeable.push(key);
        } else if key.ends_with("ListData") || key.ends_with("Data") {
            replaced.push(key);
        }
    }

    // The functions this crate's nineteen use cases exchange are all in the first list.
    for key in [
        "loadControlLimitListData",
        "loadControlLimitDescriptionListData",
        "loadControlLimitConstraintsListData",
        "measurementListData",
        "measurementDescriptionListData",
        "measurementConstraintsListData",
        "electricalConnectionDescriptionListData",
        "electricalConnectionParameterDescriptionListData",
        "electricalConnectionCharacteristicListData",
        "electricalConnectionPermittedValueSetListData",
        "deviceConfigurationKeyValueListData",
        "deviceConfigurationKeyValueDescriptionListData",
        "setpointListData",
        "setpointDescriptionListData",
        "setpointConstraintsListData",
        "hvacSystemFunctionListData",
        "hvacOperationModeDescriptionListData",
        "hvacOverrunListData",
        "hvacOverrunDescriptionListData",
        "billListData",
        "billDescriptionListData",
        "billConstraintsListData",
    ] {
        assert!(
            mergeable.contains(&key),
            "{key} must merge entry by entry: a use case here exchanges it"
        );
    }

    // And these do not, because SPINE gives their entries no identifier to merge on. Each
    // is a deliberate answer, not an oversight:
    //
    // * `nodeManagementUseCaseData` and `useCaseInformationListData` are keyed by an
    //   `address` the wire makes optional — absent from every SPINE 1.1.1 peer in
    //   `tests/fixtures/devices` — and by an `actor`, which is not an identifier type. The
    //   engine merges this one by §7.5.4's own rules instead; see `Engine::absorb_use_cases`.
    // * `nodeManagementDetailedDiscoveryData` is three parallel lists in one value, and
    //   §7.1.5 gives it `lastStateChange` rather than an identifier. Same answer, in
    //   `Engine::absorb_discovery`.
    // * `sensingListData`, `specificationVersionListData`, `directControlActivityListData`
    //   and the `networkManagement*` tables are genuinely unkeyed, or keyed by an address
    //   held as a nested structure. Nothing here exchanges them.
    for key in [
        "nodeManagementUseCaseData",
        "nodeManagementDetailedDiscoveryData",
        "sensingListData",
        "specificationVersionListData",
    ] {
        assert!(
            replaced.contains(&key),
            "{key} was expected to replace rather than merge — if that changed, say why"
        );
    }

    assert_eq!(
        mergeable.len(),
        81,
        "the number of functions that merge entry by entry changed: {mergeable:?}"
    );
}
