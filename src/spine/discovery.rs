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
    FeatureType, Function, FunctionProperty, NetworkManagementStateChange,
    NodeManagementDetailedDiscoveryData, NodeManagementDetailedDiscoveryDeviceInformation,
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

use super::device::{LocalDevice, LocalEntity, entity_addresses};

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
            feature_information.push(feature_description(device, entity, feature));
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

/// One feature's `featureInformation` entry, as every announcement spells it.
fn feature_description(
    device: &LocalDevice,
    entity: &LocalEntity,
    feature: &super::device::LocalFeature,
) -> NodeManagementDetailedDiscoveryFeatureInformation {
    NodeManagementDetailedDiscoveryFeatureInformation {
        description: Some(
            NodeManagementDetailedDiscoveryFeatureInformationDescription {
                feature_address: Some(
                    NodeManagementDetailedDiscoveryFeatureInformationDescriptionFeatureAddress {
                        entity: Some(entity_addresses(entity.address())),
                        feature: Some(feature.address()),
                    },
                ),
                feature_type: Some(feature.feature_type().clone()),
                role: Some(feature.role()),
                supported_function: Some(device.function_properties(feature)),
                // §5.2.5.3: a feature that may take longer than the ten-second default says
                // so here, rather than leaving a client to call it unresponsive.
                max_response_delay: feature
                    .max_response_delay()
                    .map(crate::model::format_iso8601_duration)
                    .map(crate::model::MaxResponseDelay::from),
                ..Default::default()
            },
        ),
    }
}

/// Announces entities that have just appeared (§7.1.5 rule 5).
///
/// The partial `nodeManagementDetailedDiscoveryData` a device notifies when it grows an
/// entity at runtime: `lastStateChange: added` on each entity description, and **all** of
/// its features beside it, because a peer that hears about an entity and none of its
/// features has nothing to bind to.
///
/// Unchanged entities are left out, which rule 3 asks for — this is the whole message, not
/// the whole tree.
pub fn entities_added(
    device: &LocalDevice,
    entities: &[LocalEntity],
) -> NodeManagementDetailedDiscoveryData {
    changed(device, entities, NetworkManagementStateChange::Added, true)
}

/// Announces entities that have just gone (§7.1.5 rule 4).
///
/// `lastStateChange: removed` on each entity description and **no** `featureInformation`
/// at all, which rule 4b requires and rule 4c explains: the peer is told the entity is
/// gone, and that is sufficient — it does not have to be walked through the removal of
/// every feature underneath it.
///
/// This is what [`Engine::remove_entity`](super::Engine::remove_entity) sends, and what
/// [`merge_detailed_discovery`] applies at the other end.
pub fn entities_removed(
    device: &LocalDevice,
    entities: &[LocalEntity],
) -> NodeManagementDetailedDiscoveryData {
    changed(
        device,
        entities,
        NetworkManagementStateChange::Removed,
        false,
    )
}

/// Announces entities whose features changed (§7.1.5 rule 6).
///
/// `lastStateChange: modified`, with every feature of the entity listed. Rule 6b asks for
/// the *changed* features only and for each to carry its own `lastStateChange`; sending
/// all of them says the same thing without the device having to remember which ones a
/// given peer last heard about, and is what a re-send is allowed to be.
pub fn entities_modified(
    device: &LocalDevice,
    entities: &[LocalEntity],
) -> NodeManagementDetailedDiscoveryData {
    changed(
        device,
        entities,
        NetworkManagementStateChange::Modified,
        true,
    )
}

fn changed(
    device: &LocalDevice,
    entities: &[LocalEntity],
    change: NetworkManagementStateChange,
    with_features: bool,
) -> NodeManagementDetailedDiscoveryData {
    let mut entity_information = Vec::new();
    let mut feature_information = Vec::new();

    for entity in entities {
        entity_information.push(NodeManagementDetailedDiscoveryEntityInformation {
            description: Some(
                NodeManagementDetailedDiscoveryEntityInformationDescription {
                    entity_address: Some(
                        NodeManagementDetailedDiscoveryEntityInformationDescriptionEntityAddress {
                            entity: Some(entity_addresses(entity.address())),
                        },
                    ),
                    entity_type: Some(entity.entity_type().clone()),
                    last_state_change: Some(change),
                    ..Default::default()
                },
            ),
        });
        if !with_features {
            continue;
        }
        for feature in entity.features() {
            feature_information.push(feature_description(device, entity, feature));
        }
    }

    NodeManagementDetailedDiscoveryData {
        // §7.1.5 rule 3: unchanged data stays out, and the device's own description has
        // not changed. The address is in the datagram's header, which is where the
        // receiving side files it.
        specification_version_list: None,
        device_information: None,
        entity_information: Some(entity_information),
        feature_information: (!feature_information.is_empty()).then_some(feature_information),
    }
}

/// Applies a §7.1.5 runtime update to the document a peer sent before.
///
/// Detailed discovery is not a list in SPINE's sense — it is three parallel lists in one
/// value — so the generic partial merge cannot address its entries: it would replace
/// `entityInformation` wholesale, and an update that names one new entity, as rule 3 asks
/// it to, would take every other entity with it. This is the merge that rule describes.
///
/// * An entity marked `removed` goes, and everything addressed beneath it goes with it —
///   an entity whose parent is gone is unreachable — together with their features. Rule 4c
///   says the device need not list those features, so the receiver works them out.
/// * Any other entity is inserted, or replaces the one at its address.
/// * A feature marked `removed` goes; any other replaces the one at its address, or is
///   added.
/// * `deviceInformation` and `specificationVersionList` are taken where the update carries
///   them and left alone where it does not.
///
/// A full (non-partial) document replaces what was stored and does not come through here.
pub fn merge_detailed_discovery(
    stored: &mut NodeManagementDetailedDiscoveryData,
    update: &NodeManagementDetailedDiscoveryData,
) {
    if update.specification_version_list.is_some() {
        stored.specification_version_list = update.specification_version_list.clone();
    }
    if update.device_information.is_some() {
        stored.device_information = update.device_information.clone();
    }

    let mut entities = stored.entity_information.take().unwrap_or_default();
    let mut features = stored.feature_information.take().unwrap_or_default();

    for entry in update.entity_information.iter().flatten() {
        let Some(description) = entry.description.as_ref() else {
            continue;
        };
        let Some(path) = entity_path_of(description) else {
            // An entry that addresses nothing can neither replace nor remove anything.
            continue;
        };
        if description.last_state_change == Some(NetworkManagementStateChange::Removed) {
            entities.retain(|held| {
                held.description
                    .as_ref()
                    .and_then(entity_path_of)
                    .is_none_or(|held| !held.starts_with(&path))
            });
            features.retain(|held| {
                held.description
                    .as_ref()
                    .and_then(feature_path_of)
                    .is_none_or(|(held, _)| !held.starts_with(&path))
            });
            continue;
        }
        let held = entities.iter().position(|held| {
            held.description.as_ref().and_then(entity_path_of).as_ref() == Some(&path)
        });
        match held {
            Some(index) => entities[index] = entry.clone(),
            None if entities.len() < super::device::MAX_LIST_ENTRIES => {
                entities.push(entry.clone());
            }
            // The peer decides how many entities it announces, so the tree it can make
            // this device hold is bounded the same way a notified list is.
            None => {}
        }
    }

    for entry in update.feature_information.iter().flatten() {
        let Some(description) = entry.description.as_ref() else {
            continue;
        };
        let Some(address) = feature_path_of(description) else {
            continue;
        };
        if description.last_state_change == Some(NetworkManagementStateChange::Removed) {
            features.retain(|held| {
                held.description.as_ref().and_then(feature_path_of) != Some(address.clone())
            });
            continue;
        }
        let held = features.iter().position(|held| {
            held.description.as_ref().and_then(feature_path_of) == Some(address.clone())
        });
        match held {
            Some(index) => features[index] = entry.clone(),
            None if features.len() < super::device::MAX_LIST_ENTRIES => {
                features.push(entry.clone());
            }
            None => {}
        }
    }

    stored.entity_information = Some(entities);
    stored.feature_information = Some(features);
}

fn entity_path_of(
    description: &NodeManagementDetailedDiscoveryEntityInformationDescription,
) -> Option<Vec<u32>> {
    Some(
        description
            .entity_address
            .as_ref()?
            .entity
            .as_ref()?
            .iter()
            .map(|e| e.get())
            .collect(),
    )
}

fn feature_path_of(
    description: &NodeManagementDetailedDiscoveryFeatureInformationDescription,
) -> Option<(Vec<u32>, AddressFeature)> {
    let address = description.feature_address.as_ref()?;
    Some((
        address.entity.as_ref()?.iter().map(|e| e.get()).collect(),
        address.feature?,
    ))
}

/// Narrows detailed discovery data to what a partial read asked for (§7.1.3).
///
/// Detailed discovery is the one function whose selectors do not follow the generic
/// list pattern: it holds three parallel lists, and the selectors address them
/// separately by type or by address. A client that only wants to know where the peer's
/// `LoadControl` feature lives sends `featureInformation.featureType`, and gets back an
/// answer that is a few hundred bytes rather than the whole device tree.
///
/// Entity and feature selectors are applied independently, which is what the
/// specification's table describes. The device information is dropped only when a
/// device selector is given and does not match — a client asking about entities still
/// needs to know whose entities they are.
pub fn restrict_detailed_discovery(
    data: &NodeManagementDetailedDiscoveryData,
    selectors: &crate::model::NodeManagementDetailedDiscoveryDataSelectors,
) -> NodeManagementDetailedDiscoveryData {
    let mut out = data.clone();

    if let Some(wanted) = &selectors.device_information {
        let described = data
            .device_information
            .as_ref()
            .and_then(|d| d.description.as_ref());
        // An element the selector leaves unset constrains nothing — the same rule the
        // entity and feature blocks below follow.
        let wanted_device = wanted
            .device_address
            .as_ref()
            .and_then(|a| a.device.as_ref());
        let stored_device = described
            .and_then(|d| d.device_address.as_ref())
            .and_then(|a| a.device.as_ref());
        let address_matches = wanted_device.is_none() || stored_device == wanted_device;
        let type_matches = wanted.device_type.is_none()
            || described.and_then(|d| d.device_type.as_ref()) == wanted.device_type.as_ref();
        if !(address_matches && type_matches) {
            out.device_information = None;
        }
    }

    if let Some(wanted) = &selectors.entity_information {
        let wanted_entity = wanted
            .entity_address
            .as_ref()
            .and_then(|a| a.entity.clone());
        out.entity_information = out.entity_information.map(|entities| {
            entities
                .into_iter()
                .filter(|e| {
                    let description = e.description.as_ref();
                    let address = description
                        .and_then(|d| d.entity_address.as_ref())
                        .and_then(|a| a.entity.clone());
                    (wanted_entity.is_none() || address == wanted_entity)
                        && (wanted.entity_type.is_none()
                            || description.and_then(|d| d.entity_type.as_ref())
                                == wanted.entity_type.as_ref())
                })
                .collect()
        });
    }

    if let Some(wanted) = &selectors.feature_information {
        let wanted_address = wanted.feature_address.as_ref();
        out.feature_information = out.feature_information.map(|features| {
            features
                .into_iter()
                .filter(|f| {
                    let description = f.description.as_ref();
                    let address = description.and_then(|d| d.feature_address.as_ref());
                    let address_matches = wanted_address.is_none_or(|w| {
                        address.is_some_and(|a| {
                            (w.entity.is_none() || a.entity == w.entity)
                                && (w.feature.is_none() || a.feature == w.feature)
                        })
                    });
                    address_matches
                        && (wanted.feature_type.is_none()
                            || description.and_then(|d| d.feature_type.as_ref())
                                == wanted.feature_type.as_ref())
                })
                .collect()
        });
    }

    out
}

/// Builds this device's `nodeManagementUseCaseData` from the use cases each entity plays.
///
/// `useCaseAvailable` is set only for client actors. The SPINE implementation guide §2.2
/// is explicit about this: the flag exists so a *client* can say it is temporarily not
/// running a use case, a server actor should never send it, and anything a server does
/// send is ignored on receipt.
pub fn use_case_data(
    device: &LocalDevice,
    entries: &[(Vec<u32>, u32, &UseCaseDescriptor, Vec<u32>)],
) -> NodeManagementUseCaseData {
    let mut information: Vec<NodeManagementUseCaseDataUseCaseInformation> = Vec::new();

    for (entity, feature, descriptor, scenarios) in entries {
        let address = device.address_of(entity, *feature);
        let support = NodeManagementUseCaseDataUseCaseInformationUseCaseSupport {
            use_case_name: Some(descriptor.use_case_name()),
            use_case_version: Some(SpecificationVersion::from(descriptor.version)),
            use_case_available: matches!(descriptor.role, crate::usecases::ActorRole::Client)
                .then_some(true),
            scenario_support: Some(
                scenarios
                    .iter()
                    .copied()
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
    /// How long this feature says it may take to answer, if it said.
    ///
    /// §5.2.5.3 lets a server that needs longer than the ten-second default announce it
    /// here. [`Engine`](super::Engine) waits this long for anything it sends to this
    /// feature: a client that guesses instead reports a conformant peer as unresponsive,
    /// which under the implementation guide §2.6.2 is what calls for a staggered retry.
    pub max_response_delay: Option<core::time::Duration>,
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
    /// The feature the use case is anchored on, if the peer named one.
    ///
    /// **Absent is normal, not an error.** SPINE 1.1.1's `useCaseInformation` has no
    /// address element at all, and `eebus-go` — and therefore evcc — leaves it out. It
    /// means "somewhere on this device", and [`RemoteDevice::feature_for`] resolves it
    /// from the features instead.
    pub address: Option<FeatureAddress>,
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
    /// **Replaces what was known**, so it takes the whole document rather than a fragment.
    /// A §7.1.5 runtime update *is* a fragment — rule 3 has the device send only what
    /// changed — and merging it is [`merge_detailed_discovery`]'s job, which
    /// `Engine::absorb_discovery` does before calling this. Handing a fragment straight to
    /// this method replaces the peer's tree with whatever that fragment happened to carry.
    pub fn apply_detailed_discovery(&mut self, data: &NodeManagementDetailedDiscoveryData) {
        if let Some(description) = data
            .device_information
            .as_ref()
            .and_then(|d| d.description.as_ref())
        {
            // The address is the record's key, and the engine files the record under the
            // header's source — the address SHIP authenticated. A payload is taken at its
            // word only for a record that has no address yet, which is the opening read's
            // reply and nothing else; letting it rename a record would let a peer file its
            // discovery under a device that never sent it.
            if self.address.is_none()
                && let Some(device) = description
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
                max_response_delay: description
                    .max_response_delay
                    .as_ref()
                    .and_then(|d| crate::model::parse_iso8601_duration(d.as_str())),
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
            // The address is optional on the wire and absent from every SPINE 1.1.1 peer,
            // so only the actor is required here: dropping the entry for want of an
            // address discards every use case such a peer plays.
            let Some(actor) = &information.actor else {
                continue;
            };
            for support in information.use_case_support.iter().flatten() {
                let Some(name) = &support.use_case_name else {
                    continue;
                };
                out.push(RemoteUseCase {
                    address: information.address.clone(),
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

    /// Finds one of this peer's features by its full address.
    ///
    /// The address a use case holds is a `FeatureAddress`; this is how it gets back to
    /// what discovery said about that feature, such as its `maxResponseDelay`.
    pub fn feature_at(&self, address: &FeatureAddress) -> Option<&RemoteFeature> {
        let path = super::address::entity_path(address);
        self.entity(&path)?
            .features
            .iter()
            .find(|f| f.address.feature == address.feature)
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
    /// this lookup well defined — *when the peer says which entity that is*.
    ///
    /// When it does not — which every SPINE 1.1.1 peer and `eebus-go` do — the actor is
    /// somewhere on the device and the feature is what says where. One entity carrying a
    /// feature of that type and role resolves it; two is ambiguous, and a guess would bind
    /// the wrong entity, so the answer is [`None`].
    ///
    /// **An address that names only a device names no entity**, and takes the same route.
    /// Three of the eight devices in `tests/fixtures/devices` announce their use cases as
    /// `address: { device: … }` with no `entity` — the Elli Charger Connect Pro, the
    /// Spelsberg Wallbox Smart Pro and the Kostal Smart Energy Meter — which is what §7.5
    /// permits and what the guide's §3.3 rule cannot be read off. Taking that as the
    /// entity path `[]` would look for an entity no device has, and locate nothing on any
    /// of them.
    pub fn feature_for(
        &self,
        use_case: &RemoteUseCase,
        feature_type: &FeatureType,
        role: Role,
    ) -> Option<&RemoteFeature> {
        let path = use_case
            .address
            .as_ref()
            .map(super::address::entity_path)
            .filter(|path| !path.is_empty());
        let Some(path) = path else {
            let mut candidates = self
                .entities
                .iter()
                .filter_map(|entity| entity.feature(feature_type, role));
            let only = candidates.next()?;
            return candidates.next().is_none().then_some(only);
        };
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
            load_control.supports_partial_read(&Function::LoadControlLimitListData),
            "LPC Table 21 recommends partial reads, and the engine serves them"
        );
        assert!(!load_control.supports(&Function::DeviceDiagnosisHeartbeatData));
    }

    /// The heat pump plays the Controllable System, so `useCaseAvailable` is not sent:
    /// implementation guide §2.2 reserves that flag for client actors.
    #[test]
    fn a_server_actor_does_not_send_use_case_available() {
        let device = heat_pump();
        let data = use_case_data(
            &device,
            &[(
                alloc::vec![1],
                1,
                &lpc::CONTROLLABLE_SYSTEM,
                lpc::CONTROLLABLE_SYSTEM.required_scenarios().collect(),
            )],
        );

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
        let data = use_case_data(
            &device,
            &[(
                alloc::vec![1],
                1,
                &lpc::ENERGY_GUARD,
                lpc::ENERGY_GUARD.required_scenarios().collect(),
            )],
        );
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
            &[(
                alloc::vec![1],
                1,
                &lpc::CONTROLLABLE_SYSTEM,
                lpc::CONTROLLABLE_SYSTEM.required_scenarios().collect(),
            )],
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
                (
                    alloc::vec![1],
                    1,
                    &lpc::ENERGY_GUARD,
                    lpc::ENERGY_GUARD.required_scenarios().collect(),
                ),
                (
                    alloc::vec![1],
                    1,
                    &lpc::ENERGY_GUARD,
                    lpc::ENERGY_GUARD.required_scenarios().collect(),
                ),
            ],
        );
        let information = data.use_case_information.as_ref().unwrap();
        assert_eq!(information.len(), 1, "one entry per address and actor");
        assert_eq!(information[0].use_case_support.as_ref().unwrap().len(), 2);
    }
}
