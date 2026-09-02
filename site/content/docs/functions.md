+++
title = "Which functions can be exchanged in part"
description = "The generated Restricted Function Exchange table: every SPINE function this crate can serve a filtered read, a partial write or a filtered delete on, and every function it cannot."
weight = 65
[extra]
group = "Protocol"
+++

SPINE admits thousands of implementation variations, and "which ones does this implement" is the first question an integrator has to answer. For Restricted Function Exchange the answer is a table, and this is it — **generated from the same source the compiler reads**, by `cargo xtask rfe-table`, so it cannot drift from what the engine does.

Of the **142** functions the SPINE payload defines, **141** can be exchanged in part: 98 are lists of identified entries and 43 are single values.

## What the columns mean

Nothing in the XML Schemas links a data type to its selectors and elements filters — the link is a naming convention, which the generator resolves. **Selectors** choose which entries of a list a command addresses; **elements** choose which parts of them. A function with both can be read, written and deleted at either granularity; one with neither is exchanged whole.

This table is also what `possibleOperations` is derived from. A feature announces `read.partial` only for a function listed here, and a filtered request for anything else is answered `errorNumber` 8 rather than approximately — see [Conformance](@/docs/conformance.md).

A **delete** is a restricted exchange too: its `selectors` choose the entries and its `elements` choose the parts of them to remove, and a command may carry a delete filter and a partial-update filter at once. See [Restricted Function Exchange](@/docs/spine.md#restricted-function-exchange).

## Exchangeable in part

| Function | Shape | Addressable by |
|---|---|---|
| `actuatorLevelData` | value | elements |
| `actuatorLevelDescriptionData` | value | elements |
| `actuatorSwitchData` | value | elements |
| `actuatorSwitchDescriptionData` | value | elements |
| `alarmListData` | list | entries and elements |
| `billConstraintsListData` | list | entries and elements |
| `billDescriptionListData` | list | entries and elements |
| `billListData` | list | entries and elements |
| `bindingManagementDeleteCall` | value | elements |
| `bindingManagementEntryListData` | list | entries and elements |
| `bindingManagementRequestCall` | value | elements |
| `commodityListData` | list | entries and elements |
| `dataTunnelingCall` | value | elements |
| `deviceClassificationManufacturerData` | value | elements |
| `deviceClassificationUserData` | value | elements |
| `deviceConfigurationKeyValueConstraintsListData` | list | entries and elements |
| `deviceConfigurationKeyValueDescriptionListData` | list | entries and elements |
| `deviceConfigurationKeyValueListData` | list | entries and elements |
| `deviceDiagnosisHeartbeatData` | value | elements |
| `deviceDiagnosisServiceData` | value | elements |
| `deviceDiagnosisStateData` | value | elements |
| `directControlActivityListData` | list | entries |
| `directControlDescriptionData` | value | elements |
| `electricalConnectionCharacteristicListData` | list | entries and elements |
| `electricalConnectionDescriptionListData` | list | entries and elements |
| `electricalConnectionParameterDescriptionListData` | list | entries and elements |
| `electricalConnectionPermittedValueSetListData` | list | entries and elements |
| `electricalConnectionStateListData` | list | entries and elements |
| `hvacOperationModeDescriptionListData` | list | entries and elements |
| `hvacOverrunDescriptionListData` | list | entries and elements |
| `hvacOverrunListData` | list | entries and elements |
| `hvacSystemFunctionDescriptionListData` | list | entries and elements |
| `hvacSystemFunctionListData` | list | entries and elements |
| `hvacSystemFunctionOperationModeRelationListData` | list | entries and elements |
| `hvacSystemFunctionPowerSequenceRelationListData` | list | entries and elements |
| `hvacSystemFunctionSetpointRelationListData` | list | entries and elements |
| `identificationListData` | list | entries and elements |
| `incentiveDescriptionListData` | list | entries and elements |
| `incentiveListData` | list | entries and elements |
| `incentiveTableConstraintsData` | list | entries |
| `incentiveTableData` | list | entries |
| `incentiveTableDescriptionData` | list | entries |
| `loadControlEventListData` | list | entries |
| `loadControlLimitConstraintsListData` | list | entries and elements |
| `loadControlLimitDescriptionListData` | list | entries and elements |
| `loadControlLimitListData` | list | entries and elements |
| `loadControlNodeData` | value | elements |
| `loadControlStateListData` | list | entries |
| `measurementConstraintsListData` | list | entries and elements |
| `measurementDescriptionListData` | list | entries and elements |
| `measurementListData` | list | entries and elements |
| `measurementSeriesListData` | list | entries and elements |
| `measurementThresholdRelationListData` | list | entries and elements |
| `messagingListData` | list | entries |
| `networkManagementAbortCall` | value | elements |
| `networkManagementAddNodeCall` | value | elements |
| `networkManagementDeviceDescriptionListData` | list | entries |
| `networkManagementDiscoverCall` | value | elements |
| `networkManagementEntityDescriptionListData` | list | entries |
| `networkManagementFeatureDescriptionListData` | list | entries |
| `networkManagementJoiningModeData` | value | elements |
| `networkManagementModifyNodeCall` | value | elements |
| `networkManagementProcessStateData` | value | elements |
| `networkManagementRemoveNodeCall` | value | elements |
| `networkManagementReportCandidateData` | value | elements |
| `networkManagementScanNetworkCall` | value | elements |
| `nodeManagementBindingData` | list | entries |
| `nodeManagementBindingDeleteCall` | value | elements |
| `nodeManagementBindingRequestCall` | value | elements |
| `nodeManagementDestinationListData` | list | entries |
| `nodeManagementDetailedDiscoveryData` | value | elements |
| `nodeManagementSubscriptionData` | list | entries |
| `nodeManagementSubscriptionDeleteCall` | value | elements |
| `nodeManagementSubscriptionRequestCall` | value | elements |
| `nodeManagementUseCaseData` | list | entries |
| `operatingConstraintsDurationListData` | list | entries and elements |
| `operatingConstraintsInterruptListData` | list | entries and elements |
| `operatingConstraintsPowerDescriptionListData` | list | entries and elements |
| `operatingConstraintsPowerLevelListData` | list | entries and elements |
| `operatingConstraintsPowerRangeListData` | list | entries and elements |
| `operatingConstraintsResumeImplicationListData` | list | entries and elements |
| `powerSequenceAlternativesRelationListData` | list | entries and elements |
| `powerSequenceDescriptionListData` | list | entries and elements |
| `powerSequenceNodeScheduleInformationData` | value | elements |
| `powerSequencePriceCalculationRequestCall` | value | elements |
| `powerSequencePriceListData` | list | entries and elements |
| `powerSequenceScheduleConfigurationRequestCall` | value | elements |
| `powerSequenceScheduleConstraintsListData` | list | entries and elements |
| `powerSequenceScheduleListData` | list | entries and elements |
| `powerSequenceSchedulePreferenceListData` | list | entries and elements |
| `powerSequenceStateListData` | list | entries and elements |
| `powerTimeSlotScheduleConstraintsListData` | list | entries and elements |
| `powerTimeSlotScheduleListData` | list | entries and elements |
| `powerTimeSlotValueListData` | list | entries and elements |
| `sensingDescriptionData` | value | elements |
| `sensingListData` | list | entries |
| `sessionIdentificationListData` | list | entries and elements |
| `sessionMeasurementRelationListData` | list | entries and elements |
| `setpointConstraintsListData` | list | entries and elements |
| `setpointDescriptionListData` | list | entries and elements |
| `setpointListData` | list | entries and elements |
| `smartEnergyManagementPsConfigurationRequestCall` | value | elements |
| `smartEnergyManagementPsData` | value | elements |
| `smartEnergyManagementPsPriceCalculationRequestCall` | value | elements |
| `smartEnergyManagementPsPriceData` | list | entries |
| `specificationVersionListData` | list | entries |
| `stateInformationListData` | list | entries and elements |
| `subscriptionManagementDeleteCall` | value | elements |
| `subscriptionManagementEntryListData` | list | entries and elements |
| `subscriptionManagementRequestCall` | value | elements |
| `supplyConditionDescriptionListData` | list | entries and elements |
| `supplyConditionListData` | list | entries and elements |
| `supplyConditionThresholdRelationListData` | list | entries and elements |
| `tariffBoundaryRelationListData` | list | entries and elements |
| `tariffDescriptionListData` | list | entries and elements |
| `tariffListData` | list | entries and elements |
| `tariffOverallConstraintsData` | value | elements |
| `tariffTierRelationListData` | list | entries and elements |
| `taskManagementJobDescriptionListData` | list | entries and elements |
| `taskManagementJobListData` | list | entries and elements |
| `taskManagementJobRelationListData` | list | entries and elements |
| `taskManagementOverviewData` | value | elements |
| `thresholdConstraintsListData` | list | entries and elements |
| `thresholdDescriptionListData` | list | entries and elements |
| `thresholdListData` | list | entries and elements |
| `tierBoundaryDescriptionListData` | list | entries and elements |
| `tierBoundaryListData` | list | entries and elements |
| `tierDescriptionListData` | list | entries and elements |
| `tierIncentiveRelationListData` | list | entries and elements |
| `tierListData` | list | entries and elements |
| `timeDistributorData` | value | elements |
| `timeDistributorEnquiryCall` | value | elements |
| `timeInformationData` | value | elements |
| `timePrecisionData` | value | elements |
| `timeSeriesConstraintsListData` | list | entries and elements |
| `timeSeriesDescriptionListData` | list | entries and elements |
| `timeSeriesListData` | list | entries and elements |
| `timeTableConstraintsListData` | list | entries |
| `timeTableDescriptionListData` | list | entries |
| `timeTableListData` | list | entries and elements |
| `useCaseInformationListData` | list | entries |

## Whole function only

The schemas give one function no selectors and no elements filter, so there is nothing to narrow it by. A feature serving one announces `read` without `partial`, and a peer that sends a filter for it is answered `errorNumber` 8 — the honest answer, and the one that stops a client acting on a reply it thinks was filtered.

- `resultData`
