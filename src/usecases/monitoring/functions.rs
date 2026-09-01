//! The feature-and-function tables the monitoring use cases share.
//!
//! Every use case built on this machinery — MPC, MGCP, EVCEM, EVSOC, MOI, MOB, MPS —
//! declares the same handful of function sets in its Table 5, differing only in which
//! scenario references which. Writing them once means a descriptor cannot claim a function
//! its actor does not actually serve, and means adding a use case is choosing from this
//! list rather than retyping it.

use crate::model::{FeatureType, Function};
use crate::usecases::descriptor::FunctionUse;

/// `Measurement` plus the `ElectricalConnection` descriptions that give it meaning.
///
/// The pair is not optional. A `measurementListData` on its own is a number with an
/// identifier; what it *is*, and which phase it was taken on, comes from the two
/// description functions, and a client that read only the first could not tell the current
/// on phase A from the current on phase B.
pub const SERVER_MEASUREMENTS: &[FunctionUse] = &[
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

/// The same, from the reader's side.
pub const CLIENT_MEASUREMENTS: &[FunctionUse] = &[
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

/// `electricalConnectionCharacteristicListData`: what a device *is*, rather than what it
/// is doing — a nominal capacity, a nameplate maximum.
pub const SERVER_CHARACTERISTICS: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionCharacteristicListData,
    ),
];

/// The same, from the reader's side.
pub const CLIENT_CHARACTERISTICS: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionCharacteristicListData,
    ),
];

/// `deviceClassificationManufacturerData`: the strings a user interface shows.
pub const SERVER_IDENTIFICATION: &[FunctionUse] = &[FunctionUse::server(
    FeatureType::DeviceClassification,
    Function::DeviceClassificationManufacturerData,
)];

/// The same, from the reader's side.
pub const CLIENT_IDENTIFICATION: &[FunctionUse] = &[FunctionUse::client(
    FeatureType::DeviceClassification,
    Function::DeviceClassificationManufacturerData,
)];

/// `deviceDiagnosisStateData`: working, standing by, or broken.
pub const SERVER_STATE: &[FunctionUse] = &[FunctionUse::server(
    FeatureType::DeviceDiagnosis,
    Function::DeviceDiagnosisStateData,
)];

/// The same, from the reader's side.
pub const CLIENT_STATE: &[FunctionUse] = &[FunctionUse::client(
    FeatureType::DeviceDiagnosis,
    Function::DeviceDiagnosisStateData,
)];

/// `deviceConfigurationKeyValue…`: named values that are configured rather than measured,
/// and that a peer only reads.
pub const SERVER_CONFIGURATION: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

/// The same, from the reader's side.
pub const CLIENT_CONFIGURATION: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

/// The same, where a bound peer may write the values.
///
/// A separate table from [`SERVER_CONFIGURATION`] on purpose. Whether a configuration key
/// is writeable is what decides whether a binding is needed at all, and a descriptor that
/// claimed a write it does not serve would send a peer looking for one.
pub const SERVER_CONFIGURATION_WRITEABLE: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::server_writeable(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

/// The same, from the writer's side.
pub const CLIENT_CONFIGURATION_WRITES: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::client_writes(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];
