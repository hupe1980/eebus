//! Detailed discovery and use-case discovery.
//!
//! Before two SPINE nodes can do anything useful they have to learn what the other one
//! is. Detailed discovery (Protocol Specification §7.1) answers "what entities and
//! features do you have, and what may I do with them"; use-case discovery (§7.5) answers
//! "and which use cases do you play, in which role". Both are read from the peer's
//! primary NodeManagement instance, and both are re-sent as notifications when the
//! answer changes at runtime.
//!
//! This module builds those answers from a [`LocalDevice`] and reads a peer's into a
//! [`RemoteDevice`] a use case can query.

use alloc::string::String;
use alloc::vec::Vec;

use crate::model::{
    AddressDevice, AddressEntity, AddressFeature, DeviceType, EntityType, FeatureAddress,
    FeatureType, Function, FunctionProperty, NodeManagementDetailedDiscoveryData,
    NodeManagementDetailedDiscoveryDeviceInformation,
    NodeManagementDetailedDiscoveryDeviceInformationDescription,
    NodeManagementDetailedDiscoveryDeviceInformationDescriptionDeviceAddress,
    NodeManagementDetailedDiscoveryEntityInformation,
    NodeManagementDetailedDiscoveryEntityInformationDescription,
    NodeManagementDetailedDiscoveryEntityInformationDescriptionEntityAddress,
    NodeManagementDetailedDiscoveryFeatureInformation,
    NodeManagementDetailedDiscoveryFeatureInformationDescription,
    NodeManagementDetailedDiscoveryFeatureInformationDescriptionFeatureAddress,
    NodeManagementSpecificationVersionList, NodeManagementUseCaseData,
    NodeManagementUseCaseDataUseCaseInformation,
    NodeManagementUseCaseDataUseCaseInformationUseCaseSupport, Role, SpecificationVersion,
    SpecificationVersionData, UseCaseActor, UseCaseName, UseCaseScenarioSupport,
};
use crate::usecases::UseCaseDescriptor;

use super::device::{LocalDevice, entity_addresses};

/// The SPINE version this implementation speaks.
pub const SPINE_VERSION: &str = "1.3.0";

/// Builds this device's `nodeManagementDetailedDiscoveryData`.
///
/// Every entity and every feature is listed, together with what a peer may do with each
/// function. The peer needs the last part before it tries: a write on a function that
/// was never announced writeable is refused, and a partial read of one that does not
/// support partial exchange has to fall back to a full read.
pub fn detailed_discovery(device: &LocalDevice) -> NodeManagementDetailedDiscoveryData {
    let mut entity_information = Vec::new();
    let mut feature_information = Vec::new();

    for entity in device.entities() {
        entity_information.push(NodeManagementDetailedDiscoveryEntityInformation {
            description: Some(
                NodeManagementDetailedDiscoveryEntityInformationDescription {
                    entity_address: Some(
                        NodeManagementDetailedDiscoveryEntityInformationDescriptionEntityAddress {
                            entity: Some(entity_addresses(entity.address())),
                        },
                    ),
                    entity_type: Some(entity.entity_type().clone()),
                    ..Default::default()
                },
            ),
        });

        for feature in entity.features() {
            feature_information.push(NodeManagementDetailedDiscoveryFeatureInformation {
                description: Some(NodeManagementDetailedDiscoveryFeatureInformationDescription {
                    feature_address: Some(
                        NodeManagementDetailedDiscoveryFeatureInformationDescriptionFeatureAddress {
                            entity: Some(entity_addresses(entity.address())),
                            feature: Some(feature.address()),
                        },
                    ),
                    feature_type: Some(feature.feature_type().clone()),
                    role: Some(feature.role()),
                    supported_function: Some(device.function_properties(feature)),
                    ..Default::default()
                }),
            });
        }
    }

    NodeManagementDetailedDiscoveryData {
        specification_version_list: Some(NodeManagementSpecificationVersionList {
            specification_version: Some(alloc::vec![SpecificationVersionData::from(SPINE_VERSION)]),
        }),
        device_information: Some(NodeManagementDetailedDiscoveryDeviceInformation {
            description: Some(
                NodeManagementDetailedDiscoveryDeviceInformationDescription {
                    device_address: Some(
                        NodeManagementDetailedDiscoveryDeviceInformationDescriptionDeviceAddress {
                            device: Some(device.address().clone()),
                        },
                    ),
                    device_type: Some(device.device_type().clone()),
                    network_feature_set: Some(*device.feature_set()),
                    ..Default::default()
                },
            ),
        }),
        entity_information: Some(entity_information),
        feature_information: Some(feature_information),
    }
}

/// Builds this device's `nodeManagementUseCaseData` from the use cases each entity plays.
///
/// `useCaseAvailable` is set only for client actors. The SPINE implementation guide §2.2
/// is explicit about this: the flag exists so a *client* can say it is temporarily not
/// running a use case, a server actor should never send it, and anything a server does
/// send is ignored on receipt.
pub fn use_case_data(
    device: &LocalDevice,
    entries: &[(Vec<u32>, u32, &UseCaseDescriptor)],
) -> NodeManagementUseCaseData {
    let mut information: Vec<NodeManagementUseCaseDataUseCaseInformation> = Vec::new();

    for (entity, feature, descriptor) in entries {
        let address = device.address_of(entity, *feature);
        let support = NodeManagementUseCaseDataUseCaseInformationUseCaseSupport {
            use_case_name: Some(descriptor.use_case_name()),
            use_case_version: Some(SpecificationVersion::from(descriptor.version)),
            use_case_available: matches!(descriptor.role, crate::usecases::ActorRole::Client)
                .then_some(true),
            scenario_support: Some(
                descriptor
                    .supported_scenarios()
                    .map(UseCaseScenarioSupport)
                    .collect(),
            ),
            use_case_document_sub_revision: Some(String::from(descriptor.document_sub_revision)),
        };

        // One `useCaseInformation` entry per (address, actor); a single entity may play
        // several use cases as the same actor.
        match information.iter_mut().find(|i| {
            i.address.as_ref() == Some(&address)
                && i.actor.as_ref() == Some(&descriptor.use_case_actor())
        }) {
            Some(existing) => existing
                .use_case_support
                .get_or_insert_with(Vec::new)
                .push(support),
            None => information.push(NodeManagementUseCaseDataUseCaseInformation {
                address: Some(address),
                actor: Some(descriptor.use_case_actor()),
                use_case_support: Some(alloc::vec![support]),
            }),
        }
    }

    NodeManagementUseCaseData {
        use_case_information: Some(information),
    }
}

/// A feature of a peer, as its discovery data described it.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteFeature {
    /// The feature's full address, including the device part.
    pub address: FeatureAddress,
    /// The feature's type.
    pub feature_type: FeatureType,
    /// Whether it serves data, consumes it, or both.
    pub role: Role,
    /// The functions it declares, and what may be done with each.
    pub functions: Vec<FunctionProperty>,
}

impl RemoteFeature {
    /// Whether the peer announced that a function may be written.
    ///
    /// Worth asking before trying: a write on a function the peer never announced
    /// writeable is refused, and the SPINE implementation guide §2.6 says a refusal is
    /// a valid exchange rather than something to retry.
    pub fn is_writeable(&self, function: &Function) -> bool {
        self.functions
            .iter()
            .filter(|f| f.function.as_ref() == Some(function))
            .any(|f| {
                f.possible_operations
                    .as_ref()
                    .is_some_and(|o| o.write.is_some())
            })
    }

    /// Whether the peer announced support for partial reads of a function.
    pub fn supports_partial_read(&self, function: &Function) -> bool {
        self.functions
            .iter()
            .filter(|f| f.function.as_ref() == Some(function))
            .any(|f| {
                f.possible_operations
                    .as_ref()
                    .and_then(|o| o.read.as_ref())
                    .is_some_and(|r| r.partial.is_some())
            })
    }

    /// Whether the peer declares the function at all.
    pub fn supports(&self, function: &Function) -> bool {
        self.functions
            .iter()
            .any(|f| f.function.as_ref() == Some(function))
    }
}

/// An entity of a peer.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteEntity {
    /// The entity's address path.
    pub address: Vec<u32>,
    /// The entity's type.
    pub entity_type: Option<EntityType>,
    /// The features on it.
    pub features: Vec<RemoteFeature>,
}

impl RemoteEntity {
    /// Finds a feature by type and role, which is how a use case locates what it needs.
    pub fn feature(&self, feature_type: &FeatureType, role: Role) -> Option<&RemoteFeature> {
        self.features
            .iter()
            .find(|f| &f.feature_type == feature_type && f.role == role)
    }
}

/// One use case a peer plays, on one of its entities.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteUseCase {
    /// The feature the use case is anchored on.
    pub address: FeatureAddress,
    /// The actor the peer plays.
    pub actor: UseCaseActor,
    /// The use case's name.
    pub name: UseCaseName,
    /// The version the peer implements.
    pub version: Option<SpecificationVersion>,
    /// Whether a client actor currently offers it. Absent means available.
    pub available: bool,
    /// The scenarios the peer supports.
    pub scenarios: Vec<u32>,
}

impl RemoteUseCase {
    /// Whether the peer supports a scenario.
    pub fn supports_scenario(&self, scenario: u32) -> bool {
        self.scenarios.contains(&scenario)
    }
}

/// What this node has learned about a peer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteDevice {
    /// The peer's device address, once its discovery data has arrived.
    pub address: Option<AddressDevice>,
    /// The peer's device type.
    pub device_type: Option<DeviceType>,
    /// The SPINE versions the peer announced.
    pub spine_versions: Vec<SpecificationVersion>,
    /// The peer's entities and features.
    pub entities: Vec<RemoteEntity>,
    /// The use cases the peer plays.
    pub use_cases: Vec<RemoteUseCase>,
}

impl RemoteDevice {
    /// Reads a peer's `nodeManagementDetailedDiscoveryData` into this record.
    ///
    /// Replaces what was known: §7.1.5 has a peer re-send the whole document when its
    /// entities or features change, and a partial notification is merged into the
    /// document before it reaches here.
    pub fn apply_detailed_discovery(&mut self, data: &NodeManagementDetailedDiscoveryData) {
        if let Some(description) = data
            .device_information
            .as_ref()
            .and_then(|d| d.description.as_ref())
        {
            if let Some(device) = description
                .device_address
                .as_ref()
                .and_then(|a| a.device.clone())
            {
                self.address = Some(device);
            }
            if let Some(device_type) = &description.device_type {
                self.device_type = Some(device_type.clone());
            }
        }

        self.spine_versions = data
            .specification_version_list
            .as_ref()
            .and_then(|l| l.specification_version.clone())
            .unwrap_or_default();

        let mut entities: Vec<RemoteEntity> = data
            .entity_information
            .iter()
            .flatten()
            .filter_map(|e| e.description.as_ref())
            .map(|d| RemoteEntity {
                address: d
                    .entity_address
                    .as_ref()
                    .and_then(|a| a.entity.as_ref())
                    .map(|e| e.iter().map(|a| a.get()).collect())
                    .unwrap_or_default(),
                entity_type: d.entity_type.clone(),
                features: Vec::new(),
            })
            .collect();

        for description in data
            .feature_information
            .iter()
            .flatten()
            .filter_map(|f| f.description.as_ref())
        {
            let Some(address) = description.feature_address.as_ref() else {
                continue;
            };
            let path: Vec<u32> = address
                .entity
                .as_ref()
                .map(|e| e.iter().map(|a| a.get()).collect())
                .unwrap_or_default();
            let (Some(feature_type), Some(role)) = (&description.feature_type, description.role)
            else {
                // Type and role are what a use case searches by; a feature without them
                // cannot be matched to anything and is dropped rather than half-kept.
                continue;
            };

            let feature = RemoteFeature {
                address: FeatureAddress {
                    device: self.address.clone(),
                    entity: Some(path.iter().copied().map(AddressEntity).collect()),
                    feature: address.feature,
                },
                feature_type: feature_type.clone(),
                role,
                functions: description.supported_function.clone().unwrap_or_default(),
            };

            match entities.iter_mut().find(|e| e.address == path) {
                Some(entity) => entity.features.push(feature),
                None => entities.push(RemoteEntity {
                    address: path,
                    entity_type: None,
                    features: alloc::vec![feature],
                }),
            }
        }

        self.entities = entities;
    }

    /// Reads a peer's `nodeManagementUseCaseData` into this record.
    pub fn apply_use_case_data(&mut self, data: &NodeManagementUseCaseData) {
        let mut out = Vec::new();
        for information in data.use_case_information.iter().flatten() {
            let (Some(address), Some(actor)) = (&information.address, &information.actor) else {
                continue;
            };
            for support in information.use_case_support.iter().flatten() {
                let Some(name) = &support.use_case_name else {
                    continue;
                };
                out.push(RemoteUseCase {
                    address: address.clone(),
                    actor: actor.clone(),
                    name: name.clone(),
                    version: support.use_case_version.clone(),
                    // §7.5 and implementation guide §2.2: absent means available, and a
                    // value sent by a server actor is ignored — but the flag alone does
                    // not say which the peer is, so it is taken at face value here and
                    // the use-case layer decides.
                    available: support.use_case_available.unwrap_or(true),
                    scenarios: support
                        .scenario_support
                        .iter()
                        .flatten()
                        .map(|s| s.get())
                        .collect(),
                });
            }
        }
        self.use_cases = out;
    }

    /// Finds an entity by its address path.
    pub fn entity(&self, address: &[u32]) -> Option<&RemoteEntity> {
        self.entities.iter().find(|e| e.address == address)
    }

    /// Finds the use case a peer plays as `actor`, if any.
    pub fn use_case(&self, name: &str, actor: &str) -> Option<&RemoteUseCase> {
        self.use_cases
            .iter()
            .find(|u| u.name.as_str() == name && u.actor.as_str() == actor)
    }

    /// Finds the feature a use case needs on the entity that plays it.
    ///
    /// The use-case implementation guide §3.3 requires an actor's client and server
    /// features to live on the same entity as the use case itself, which is what makes
    /// this lookup well defined.
    pub fn feature_for(
        &self,
        use_case: &RemoteUseCase,
        feature_type: &FeatureType,
        role: Role,
    ) -> Option<&RemoteFeature> {
        let path = super::address::entity_path(&use_case.address);
        self.entity(&path)?.feature(feature_type, role)
    }

    /// The address of a feature on the entity that plays `use_case`.
    pub fn address_of(
        &self,
        use_case: &RemoteUseCase,
        feature_type: &FeatureType,
        role: Role,
    ) -> Option<FeatureAddress> {
        self.feature_for(use_case, feature_type, role)
            .map(|f| f.address.clone())
    }

    /// The peer's primary NodeManagement address, once its device address is known.
    pub fn node_management(&self) -> Option<FeatureAddress> {
        Some(super::address::node_management(self.address.as_ref()?))
    }
}

/// The address of a feature, for a peer whose device address is known.
pub fn remote_feature_address(
    device: &AddressDevice,
    entity: &[u32],
    feature: AddressFeature,
) -> FeatureAddress {
    FeatureAddress {
        device: Some(device.clone()),
        entity: Some(entity.iter().copied().map(AddressEntity).collect()),
        feature: Some(feature),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeviceType, EntityType, FeatureType, Function, Role};
    use crate::spine::{LocalEntity, LocalFeature, Operations};
    use crate::usecases::lpc;

    fn heat_pump() -> LocalDevice {
        let mut device =
            LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        let appliance = LocalEntity::new([1], EntityType::HeatPumpAppliance)
            .with_feature(
                LocalFeature::new(1, FeatureType::LoadControl, Role::Server)
                    .with_function(
                        Function::LoadControlLimitDescriptionListData,
                        Operations::read(),
                    )
                    .with_function(Function::LoadControlLimitListData, Operations::read_write()),
            )
            .with_feature(
                LocalFeature::new(2, FeatureType::DeviceDiagnosis, Role::Server)
                    .with_function(Function::DeviceDiagnosisHeartbeatData, Operations::read()),
            );
        device.add_entity(appliance).unwrap();
        device
    }

    #[test]
    fn discovery_lists_every_entity_and_feature() {
        let device = heat_pump();
        let data = detailed_discovery(&device);

        assert_eq!(data.entity_information.as_ref().unwrap().len(), 2);
        // NodeManagement's four functions, plus three on the appliance.
        assert_eq!(data.feature_information.as_ref().unwrap().len(), 3);
        assert_eq!(
            data.device_information
                .as_ref()
                .unwrap()
                .description
                .as_ref()
                .unwrap()
                .device_address
                .as_ref()
                .unwrap()
                .device
                .as_ref()
                .unwrap()
                .as_str(),
            "d:_i:46925_HeatPump-1"
        );
    }

    /// A peer reads our discovery data and finds what it needs — the whole point of the
    /// exchange.
    #[test]
    fn discovery_round_trips_into_a_remote_device() {
        let device = heat_pump();
        let mut remote = RemoteDevice::default();
        remote.apply_detailed_discovery(&detailed_discovery(&device));

        assert_eq!(
            remote.address.as_ref().map(|a| a.as_str()),
            Some("d:_i:46925_HeatPump-1")
        );
        assert_eq!(remote.device_type, Some(DeviceType::HeatGenerationSystem));
        assert_eq!(remote.entities.len(), 2);

        let appliance = remote.entity(&[1]).expect("the appliance");
        assert_eq!(appliance.entity_type, Some(EntityType::HeatPumpAppliance));

        let load_control = appliance
            .feature(&FeatureType::LoadControl, Role::Server)
            .expect("load control");
        assert!(load_control.is_writeable(&Function::LoadControlLimitListData));
        assert!(!load_control.is_writeable(&Function::LoadControlLimitDescriptionListData));
        assert!(
            !load_control.supports_partial_read(&Function::LoadControlLimitListData),
            "partial reads are not announced, so a peer performs a full read"
        );
        assert!(!load_control.supports(&Function::DeviceDiagnosisHeartbeatData));
    }

    /// The heat pump plays the Controllable System, so `useCaseAvailable` is not sent:
    /// implementation guide §2.2 reserves that flag for client actors.
    #[test]
    fn a_server_actor_does_not_send_use_case_available() {
        let device = heat_pump();
        let data = use_case_data(&device, &[(alloc::vec![1], 1, &lpc::CONTROLLABLE_SYSTEM)]);

        let information = &data.use_case_information.as_ref().unwrap()[0];
        assert_eq!(
            information.actor.as_ref().map(|a| a.as_str()),
            Some("ControllableSystem")
        );
        let support = &information.use_case_support.as_ref().unwrap()[0];
        assert_eq!(support.use_case_available, None, "server actors omit it");
        assert_eq!(
            support.use_case_name.as_ref().map(|n| n.as_str()),
            Some("limitationOfPowerConsumption")
        );
        assert_eq!(
            support.scenario_support.as_ref().unwrap(),
            &alloc::vec![
                UseCaseScenarioSupport(1),
                UseCaseScenarioSupport(2),
                UseCaseScenarioSupport(3),
                UseCaseScenarioSupport(4)
            ]
        );
    }

    #[test]
    fn a_client_actor_sends_use_case_available() {
        let mut device = LocalDevice::new(
            "i:12345",
            "ControlBox-1",
            DeviceType::ElectricitySupplySystem,
        )
        .unwrap();
        device
            .add_entity(LocalEntity::new([1], EntityType::GridGuard))
            .unwrap();
        let data = use_case_data(&device, &[(alloc::vec![1], 1, &lpc::ENERGY_GUARD)]);
        let support = &data.use_case_information.as_ref().unwrap()[0]
            .use_case_support
            .as_ref()
            .unwrap()[0];
        assert_eq!(support.use_case_available, Some(true));
    }

    #[test]
    fn use_case_discovery_round_trips() {
        let device = heat_pump();
        let mut remote = RemoteDevice::default();
        remote.apply_detailed_discovery(&detailed_discovery(&device));
        remote.apply_use_case_data(&use_case_data(
            &device,
            &[(alloc::vec![1], 1, &lpc::CONTROLLABLE_SYSTEM)],
        ));

        let use_case = remote
            .use_case("limitationOfPowerConsumption", "ControllableSystem")
            .expect("the use case");
        assert!(use_case.available, "an absent flag reads as available");
        assert!(use_case.supports_scenario(1));
        assert!(!use_case.supports_scenario(9));

        // And from the use case, the features it needs.
        let load_control = remote
            .feature_for(use_case, &FeatureType::LoadControl, Role::Server)
            .expect("load control");
        assert!(load_control.is_writeable(&Function::LoadControlLimitListData));
    }

    #[test]
    fn several_use_cases_on_one_entity_share_an_information_entry() {
        let mut device = LocalDevice::new(
            "i:12345",
            "ControlBox-1",
            DeviceType::ElectricitySupplySystem,
        )
        .unwrap();
        device
            .add_entity(LocalEntity::new([1], EntityType::GridGuard))
            .unwrap();
        let data = use_case_data(
            &device,
            &[
                (alloc::vec![1], 1, &lpc::ENERGY_GUARD),
                (alloc::vec![1], 1, &lpc::ENERGY_GUARD),
            ],
        );
        let information = data.use_case_information.as_ref().unwrap();
        assert_eq!(information.len(), 1, "one entry per address and actor");
        assert_eq!(information[0].use_case_support.as_ref().unwrap().len(), 2);
    }
}
