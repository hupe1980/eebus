//! EVSE Commissioning and Configuration.
//!
//! The introduction every other e-mobility use case assumes has happened. A wallbox tells
//! the energy manager what it is — device name, vendor, brand, the strings an interface
//! shows a user — and, more importantly, when it has stopped working.
//!
//! Two scenarios, and the second is the one that matters operationally: a wallbox in
//! `failure` is a wallbox whose car can no longer be trusted to follow a charging plan,
//! and an energy manager that keeps planning against it will get the house fuse wrong.
//!
//! ```
//! use eebus::usecases::emobility::evsecc::{self, ManufacturerInfo};
//!
//! let wallbox = ManufacturerInfo::new()
//!     .device_name("Wallbox 11")
//!     .vendor_name("Acme")
//!     .brand_name("Acme");
//!
//! // What the energy manager reads back out.
//! let published = wallbox.to_data();
//! assert_eq!(
//!     ManufacturerInfo::read(&published).and_then(|m| m.device_name),
//!     Some("Wallbox 11".into())
//! );
//! assert_eq!(evsecc::EVSE.version, "1.0.1");
//! ```

use alloc::string::String;

use crate::model::{
    CmdData, DeviceClassificationManufacturerData, DeviceDiagnosisOperatingState,
    DeviceDiagnosisStateData, EntityType, FeatureType, Function, LastErrorCode, Role,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::{actors, names};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.1";

/// Entity types an EVSE actor may live on.
const EVSE_ENTITIES: &[EntityType] = &[EntityType::EVSE];

/// Entity types the energy manager may live on.
const CEM_ENTITIES: &[EntityType] = &[EntityType::CEM];

const SERVER_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::DeviceClassification,
        Function::DeviceClassificationManufacturerData,
    ),
    FunctionUse::server(
        FeatureType::DeviceDiagnosis,
        Function::DeviceDiagnosisStateData,
    ),
];

const CLIENT_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::DeviceClassification,
        Function::DeviceClassificationManufacturerData,
    ),
    FunctionUse::client(
        FeatureType::DeviceDiagnosis,
        Function::DeviceDiagnosisStateData,
    ),
];

/// The wallbox: the actor that introduces itself.
pub static EVSE: UseCaseDescriptor = UseCaseDescriptor {
    name: names::EVSECC,
    actor: actors::EVSE,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: EVSE_ENTITIES,
    counterpart: actors::CEM,
    scenarios: &[
        Scenario {
            number: 1,
            name: "EVSE sends manufacturer information",
            // Table 1 marks this `R` for both actors: an energy manager works without
            // it, and shows the user a serial number instead of a name.
            support: Support::Recommended,
            functions: SERVER_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: "EVSE sends error state",
            support: Support::Mandatory,
            functions: SERVER_FUNCTIONS,
        },
    ],
};

/// The energy manager: the actor that reads.
pub static CEM: UseCaseDescriptor = UseCaseDescriptor {
    name: names::EVSECC,
    actor: actors::CEM,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: CEM_ENTITIES,
    counterpart: actors::EVSE,
    scenarios: &[
        Scenario {
            number: 1,
            name: "EVSE sends manufacturer information",
            support: Support::Recommended,
            functions: CLIENT_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: "EVSE sends error state",
            support: Support::Mandatory,
            functions: CLIENT_FUNCTIONS,
        },
    ],
};

/// Builds the `DeviceClassification` feature a wallbox serves (Table 5).
pub fn device_classification_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceClassification, Role::Server).with_function(
        Function::DeviceClassificationManufacturerData,
        Operations::read(),
    )
}

/// Builds the `DeviceDiagnosis` feature that carries the operating state (Table 5).
pub fn device_diagnosis_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceDiagnosis, Role::Server)
        .with_function(Function::DeviceDiagnosisStateData, Operations::read())
}

/// What a wallbox says about itself (scenario 1).
///
/// Every field is a `SHOULD`: a wallbox that omits all of them is still compliant, and an
/// energy manager still has to show the user something. That is why nothing here is
/// required, and why [`read`](Self::read) hands back whatever arrived rather than
/// refusing an incomplete answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManufacturerInfo {
    /// A name for the device, for an interface to show ([EVSECC-010]).
    pub device_name: Option<String>,
    /// The manufacturer's code for this model ([EVSECC-011]).
    pub device_code: Option<String>,
    /// The vendor's name ([EVSECC-012]).
    pub vendor_name: Option<String>,
    /// The vendor's code ([EVSECC-013]).
    pub vendor_code: Option<String>,
    /// The brand, where it differs from the vendor ([EVSECC-014]).
    pub brand_name: Option<String>,
    /// A free-text label ([EVSECC-015]).
    pub manufacturer_label: Option<String>,
}

impl ManufacturerInfo {
    /// Nothing said yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the device name ([EVSECC-010]).
    #[must_use]
    pub fn device_name(mut self, name: impl Into<String>) -> Self {
        self.device_name = Some(name.into());
        self
    }

    /// Sets the device code ([EVSECC-011]).
    #[must_use]
    pub fn device_code(mut self, code: impl Into<String>) -> Self {
        self.device_code = Some(code.into());
        self
    }

    /// Sets the vendor name ([EVSECC-012]).
    #[must_use]
    pub fn vendor_name(mut self, name: impl Into<String>) -> Self {
        self.vendor_name = Some(name.into());
        self
    }

    /// Sets the vendor code ([EVSECC-013]).
    #[must_use]
    pub fn vendor_code(mut self, code: impl Into<String>) -> Self {
        self.vendor_code = Some(code.into());
        self
    }

    /// Sets the brand name ([EVSECC-014]).
    #[must_use]
    pub fn brand_name(mut self, name: impl Into<String>) -> Self {
        self.brand_name = Some(name.into());
        self
    }

    /// Sets the manufacturer label ([EVSECC-015]).
    #[must_use]
    pub fn manufacturer_label(mut self, label: impl Into<String>) -> Self {
        self.manufacturer_label = Some(label.into());
        self
    }

    /// The `deviceClassificationManufacturerData` to publish.
    pub fn to_data(&self) -> CmdData {
        CmdData::DeviceClassificationManufacturerData(DeviceClassificationManufacturerData {
            device_name: self.device_name.clone().map(Into::into),
            device_code: self.device_code.clone().map(Into::into),
            vendor_name: self.vendor_name.clone().map(Into::into),
            vendor_code: self.vendor_code.clone().map(Into::into),
            brand_name: self.brand_name.clone().map(Into::into),
            manufacturer_label: self.manufacturer_label.clone().map(Into::into),
            ..Default::default()
        })
    }

    /// Reads what a peer published, if that is what `data` is.
    pub fn read(data: &CmdData) -> Option<Self> {
        let CmdData::DeviceClassificationManufacturerData(manufacturer) = data else {
            return None;
        };
        Some(Self {
            device_name: manufacturer.device_name.as_ref().map(|v| v.as_str().into()),
            device_code: manufacturer.device_code.as_ref().map(|v| v.as_str().into()),
            vendor_name: manufacturer.vendor_name.as_ref().map(|v| v.as_str().into()),
            vendor_code: manufacturer.vendor_code.as_ref().map(|v| v.as_str().into()),
            brand_name: manufacturer.brand_name.as_ref().map(|v| v.as_str().into()),
            manufacturer_label: manufacturer
                .manufacturer_label
                .as_ref()
                .map(|v| v.as_str().into()),
        })
    }
}

/// The `deviceDiagnosisStateData` a wallbox publishes (scenario 2, [EVSECC-020]).
///
/// `last_error_code` outlives the failure it describes: the specification asks for it to
/// stay put once the state is back to normal, so that whoever looks afterwards can see
/// what happened.
pub fn operating_state(state: DeviceDiagnosisOperatingState, last_error: Option<&str>) -> CmdData {
    CmdData::DeviceDiagnosisStateData(DeviceDiagnosisStateData {
        operating_state: Some(state),
        last_error_code: last_error.map(|code| LastErrorCode::from(String::from(code))),
        ..Default::default()
    })
}

/// Reads a peer's operating state, if that is what `data` is.
pub fn read_operating_state(data: &CmdData) -> Option<DeviceDiagnosisOperatingState> {
    let CmdData::DeviceDiagnosisStateData(state) = data else {
        return None;
    };
    state.operating_state.clone()
}

/// Whether an operating state means the device cannot be relied on.
///
/// [EVSECC-020]: an EVSE in error is one whose car may no longer follow a charging plan,
/// and whose updates may no longer hold valid data. `standby` and `finished` are not
/// errors — a wallbox with nothing plugged in is working perfectly.
pub fn is_failure(state: &DeviceDiagnosisOperatingState) -> bool {
    matches!(
        state,
        DeviceDiagnosisOperatingState::Failure
            | DeviceDiagnosisOperatingState::ServiceNeeded
            | DeviceDiagnosisOperatingState::InAlarm
            | DeviceDiagnosisOperatingState::NotReachable
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptors_say_what_the_specification_says() {
        assert_eq!(EVSE.use_case_name().as_str(), names::EVSECC);
        assert_eq!(EVSE.use_case_actor().as_str(), "EVSE");
        assert_eq!(CEM.use_case_actor().as_str(), "CEM");
        assert_eq!(EVSE.version, "1.0.1");

        // Table 1: scenario 1 is recommended, scenario 2 mandatory. So a device that
        // announces only what it must announces scenario 2.
        let required: alloc::vec::Vec<_> = EVSE.required_scenarios().collect();
        assert_eq!(
            required,
            [1, 2],
            "recommended counts as required to announce"
        );
        assert_eq!(
            EVSE.scenarios[0].support,
            Support::Recommended,
            "the manufacturer data is a SHOULD"
        );
        assert_eq!(EVSE.scenarios[1].support, Support::Mandatory);
    }

    #[test]
    fn the_features_a_wallbox_serves_are_the_two_in_table_5() {
        let features: alloc::vec::Vec<_> = EVSE.server_features().collect();
        assert_eq!(
            features,
            [
                &FeatureType::DeviceClassification,
                &FeatureType::DeviceDiagnosis
            ]
        );
        // A wallbox takes no bindings: nothing here is writeable.
        assert_eq!(EVSE.features_needing_binding().count(), 0);
        assert_eq!(CEM.features_needing_binding().count(), 0);
    }

    #[test]
    fn manufacturer_information_round_trips() {
        let original = ManufacturerInfo::new()
            .device_name("Wallbox 11")
            .device_code("WB-11")
            .vendor_name("Acme")
            .vendor_code("4711")
            .brand_name("Acme")
            .manufacturer_label("11 kW, three phase");

        let back = ManufacturerInfo::read(&original.to_data()).expect("the same function");
        assert_eq!(back, original);
    }

    #[test]
    fn a_wallbox_that_says_nothing_is_still_readable() {
        let nothing = ManufacturerInfo::new();
        let back = ManufacturerInfo::read(&nothing.to_data()).expect("still the function");
        assert_eq!(back, nothing);
        assert_eq!(
            ManufacturerInfo::read(&operating_state(
                DeviceDiagnosisOperatingState::NormalOperation,
                None
            )),
            None
        );
    }

    /// [EVSECC-020]: an error is what tells an energy manager to stop planning against
    /// this wallbox. Standby is not an error.
    #[test]
    fn evsecc_020_only_a_real_failure_reads_as_one() {
        let published = operating_state(DeviceDiagnosisOperatingState::Failure, Some("E42"));
        let state = read_operating_state(&published).expect("a state");
        assert!(is_failure(&state));

        for fine in [
            DeviceDiagnosisOperatingState::NormalOperation,
            DeviceDiagnosisOperatingState::Standby,
            DeviceDiagnosisOperatingState::Finished,
        ] {
            assert!(!is_failure(&fine), "{fine:?}");
        }
    }

    /// The error code stays put after the state recovers, so it can still be read.
    #[test]
    fn the_last_error_code_outlives_the_failure() {
        let recovered =
            operating_state(DeviceDiagnosisOperatingState::NormalOperation, Some("E42"));
        let CmdData::DeviceDiagnosisStateData(state) = &recovered else {
            panic!("expected the state");
        };
        assert_eq!(
            state.last_error_code.as_ref().map(|c| c.as_str()),
            Some("E42")
        );
        assert_eq!(
            state.operating_state.as_ref(),
            Some(&DeviceDiagnosisOperatingState::NormalOperation)
        );
    }
}
