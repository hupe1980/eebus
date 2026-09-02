//! The local device: what this node offers, and what it holds.
//!
//! SPINE arranges a node's resources in three levels (Resource Specification §3): a
//! *device* holds *entities*, an entity holds *features*, and a feature holds the
//! *functions* that carry the data. A heat pump is one device; its appliance and its
//! compressor are entities; `LoadControl` and `DeviceDiagnosis` are features on them;
//! `loadControlLimitListData` is a function of the first.
//!
//! Two rules shape this module. Entity `[0]` always exists and always carries the
//! primary NodeManagement instance, because that is what a peer discovers everything
//! else through. And a feature type may appear at most once per entity — the SPINE
//! implementation guide §3.4 forbids the alternative, since a client looking for
//! `Measurement` would have no way to choose between two of them.

use alloc::vec;
use alloc::vec::Vec;

use crate::model::rfe::FunctionMismatch;
use crate::model::{
    AddressDevice, AddressEntity, AddressFeature, CmdData, DeviceType, EntityType, FeatureAddress,
    FeatureType, Function, FunctionProperty, NetworkManagementFeatureSet, PossibleOperations,
    PossibleOperationsRead, PossibleOperationsWrite, Role,
};

use super::address::{self, NODE_MANAGEMENT_ENTITY, NODE_MANAGEMENT_FEATURE};

/// The most list entries one function will hold.
///
/// A partial write appends any entry whose identifier matches nothing stored, so a peer
/// with a binding can grow a stored list one message at a time — a legitimate protocol
/// flow with no natural end. Nothing in SPINE caps it.
///
/// The number is generous against real devices: the largest lists in practice are a big
/// inverter's measurement descriptions, a few dozen entries. Reaching it means something
/// on the wire is wrong, and the answer is `errorNumber` 3 — overload.
pub const MAX_LIST_ENTRIES: usize = 128;

/// What a peer may do with one function of a feature.
///
/// Reported in `nodeManagementDetailedDiscoveryData` so that a peer knows, before it
/// tries, whether it may write a function and whether partial exchange is available.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Operations {
    /// The function can be read.
    pub read: bool,
    /// Reads may be partial (Restricted Function Exchange).
    pub read_partial: bool,
    /// The function can be written.
    pub write: bool,
    /// Writes may be partial.
    pub write_partial: bool,
}

impl Operations {
    /// Readable in full only.
    ///
    /// A node must not claim an operation it cannot perform: a client that sees
    /// `read.partial` will send a filter and expect the reply to honour it, and
    /// answering with the whole function instead is a quiet protocol violation. Use this
    /// for a function whose data the engine cannot narrow — one the generated
    /// Restricted Function Exchange table does not cover.
    pub const fn read_full() -> Self {
        Self {
            read: true,
            read_partial: false,
            write: false,
            write_partial: false,
        }
    }

    /// Readable, in full or in part.
    ///
    /// The default for a readable function, because the engine serves a filtered read
    /// generically for every function the schemas give selectors or an elements filter —
    /// which is nearly all of them. LPC Table 21 marks partial reads `partial (R)`;
    /// announcing them is what lets a client ask for the one limit it cares about
    /// instead of the whole list.
    pub const fn read() -> Self {
        Self {
            read: true,
            read_partial: true,
            write: false,
            write_partial: false,
        }
    }

    /// Readable and writeable, both in full and in part.
    ///
    /// This is what LPC Table 21 requires of `loadControlLimitListData`:
    /// `write (M). partial (M)`. Partial writes carry the limit updates the use case is
    /// built on.
    pub const fn read_write() -> Self {
        Self {
            read: true,
            read_partial: true,
            write: true,
            write_partial: true,
        }
    }

    /// Readable and writeable, but only in full.
    pub const fn read_write_full() -> Self {
        Self {
            read: true,
            read_partial: false,
            write: true,
            write_partial: false,
        }
    }

    fn to_model(self) -> PossibleOperations {
        PossibleOperations {
            read: self.read.then(|| PossibleOperationsRead {
                partial: self.read_partial.then_some(crate::codec::ElementTag),
            }),
            write: self.write.then(|| PossibleOperationsWrite {
                partial: self.write_partial.then_some(crate::codec::ElementTag),
            }),
        }
    }
}

/// One function of a feature: what it is, what may be done with it, and its data.
#[derive(Clone, Debug)]
pub struct FunctionEntry {
    /// The function's name.
    pub function: Function,
    /// What a peer may do with it.
    pub operations: Operations,
    /// The data itself, once there is any.
    pub data: Option<CmdData>,
}

/// Who decides whether a peer's write is accepted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteApproval {
    /// The engine applies the write and acknowledges it.
    ///
    /// Right for a feature that is simply storage.
    #[default]
    Automatic,
    /// The application decides.
    ///
    /// The engine reports the write and waits; nothing is stored and nothing is
    /// acknowledged until [`Engine::accept_write`](crate::spine::Engine::accept_write)
    /// or [`Engine::reject_write`](crate::spine::Engine::reject_write) says so.
    ///
    /// Use cases that may refuse need this. LPC's Controllable System accepts a limit
    /// only if it can actually follow it, and the acknowledgement it returns is the
    /// evidence §14a expects the operator to be able to produce — so the decision has to
    /// come before the answer, not after.
    Deferred,
}

/// A feature of a local entity.
#[derive(Clone, Debug)]
pub struct LocalFeature {
    address: AddressFeature,
    feature_type: FeatureType,
    role: Role,
    approval: WriteApproval,
    functions: Vec<FunctionEntry>,
    max_response_delay: Option<core::time::Duration>,
}

impl LocalFeature {
    /// A feature with no functions yet.
    pub fn new(address: u32, feature_type: FeatureType, role: Role) -> Self {
        Self {
            address: AddressFeature(address),
            feature_type,
            role,
            approval: WriteApproval::Automatic,
            functions: Vec::new(),
            max_response_delay: None,
        }
    }

    /// Hands the decision on incoming writes to the application.
    #[must_use]
    pub fn with_deferred_writes(mut self) -> Self {
        self.approval = WriteApproval::Deferred;
        self
    }

    /// Announces that this feature may take longer than the ten-second default to answer.
    ///
    /// §5.2.5.3 provides `maxResponseDelay` in detailed discovery for exactly this, and a
    /// feature whose writes the application decides on is the case that needs it: a
    /// Controllable System that must ask a compressor controller before it can say whether
    /// it can follow a limit has no way to be slow *and* conformant otherwise. Announcing
    /// it does two things — a peer waits that long before calling this node unresponsive,
    /// and the engine holds a deferred write for the same period instead of abandoning it
    /// after ten seconds.
    ///
    /// Leave it unset unless the feature genuinely needs it. The default is the
    /// specification's, and a long delay announced by a feature that answers instantly
    /// slows down every client's error handling.
    #[must_use]
    pub fn with_max_response_delay(mut self, delay: core::time::Duration) -> Self {
        self.max_response_delay = Some(delay);
        self
    }

    /// How long this feature says it may take to answer, if it says.
    pub fn max_response_delay(&self) -> Option<core::time::Duration> {
        self.max_response_delay
    }

    /// Who decides whether a peer's write is accepted.
    pub fn write_approval(&self) -> WriteApproval {
        self.approval
    }

    /// Declares a function this feature offers.
    ///
    /// `read.partial` is dropped for a function the engine cannot narrow — the schemas
    /// give most functions a selectors or elements filter, but not all of them, and a
    /// node that announced partial reads it could not serve would have clients sending
    /// filters only to be answered with `errorNumber` 8.
    #[must_use]
    pub fn with_function(mut self, function: Function, operations: Operations) -> Self {
        let operations = Operations {
            read_partial: operations.read_partial
                && CmdData::supports_restriction(function.as_str()),
            ..operations
        };
        self.functions.push(FunctionEntry {
            function,
            operations,
            data: None,
        });
        self
    }

    /// The feature's address within its entity.
    pub fn address(&self) -> AddressFeature {
        self.address
    }

    /// The feature's type.
    pub fn feature_type(&self) -> &FeatureType {
        &self.feature_type
    }

    /// Whether this feature serves data, consumes it, or both.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The functions this feature declares.
    pub fn functions(&self) -> &[FunctionEntry] {
        &self.functions
    }

    /// Looks up one function.
    pub fn function(&self, function: &Function) -> Option<&FunctionEntry> {
        self.functions.iter().find(|f| &f.function == function)
    }

    /// Checks that a write would be permitted, without performing it.
    ///
    /// Used before deferring a decision to the application: a write that the feature
    /// would refuse anyway is refused at once rather than put to a use case.
    ///
    /// `restricted` covers both halves of §5.3.4 — a partial update *and* a delete — since
    /// `possibleOperations` has one flag for the pair. A feature that announces `write`
    /// without `write.partial` is saying it exchanges whole functions only, and serving a
    /// delete on it would be doing something it never announced. That is D14's rule in the
    /// other direction, and it costs nothing to keep.
    pub fn check_write(&self, update: &CmdData, restricted: bool) -> Result<(), FeatureError> {
        let function = Function::from(update.key());
        let entry = self
            .function(&function)
            .ok_or(FeatureError::UnknownFunction)?;
        if !entry.operations.write {
            return Err(FeatureError::NotWriteable);
        }
        if restricted && !entry.operations.write_partial {
            return Err(FeatureError::PartialNotSupported);
        }
        // Use-case IG §3.1: every message carries an entry's primary and sub identifiers.
        if !update.entries_identified() {
            return Err(FeatureError::MissingIdentifier);
        }
        Ok(())
    }

    fn function_mut(&mut self, function: &Function) -> Option<&mut FunctionEntry> {
        self.functions.iter_mut().find(|f| &f.function == function)
    }

    /// The data currently stored for a function.
    pub fn data(&self, function: &Function) -> Option<&CmdData> {
        self.function(function)?.data.as_ref()
    }

    /// Replaces a function's data outright, and says whether that changed anything.
    ///
    /// Used by the application to publish a value; a peer's write goes through
    /// [`apply`](Self::apply) instead, which honours the partial rules.
    ///
    /// The answer matters: the SPINE implementation guide §2.4 asks a server not to
    /// notify data that has not changed, and an energy manager holding a dozen
    /// subscriptions is the reason — a device that re-notifies every second because it
    /// re-published the same reading floods the peer it depends on.
    pub fn set_data(&mut self, data: CmdData) -> Result<bool, FeatureError> {
        let function = Function::from(data.key());
        let entry = self
            .function_mut(&function)
            .ok_or(FeatureError::UnknownFunction)?;
        if entry.data.as_ref() == Some(&data) {
            return Ok(false);
        }
        entry.data = Some(data);
        Ok(true)
    }

    /// Applies a peer's write.
    ///
    /// `partial` comes from the command's `cmdControl`. A partial write merges into the
    /// stored value; a full one replaces it. Writing a function that was not declared
    /// writeable is refused, which SPINE reports as `errorNumber` 7.
    pub fn apply(&mut self, update: CmdData, partial: bool) -> Result<(), FeatureError> {
        let function = Function::from(update.key());
        let entry = self
            .function_mut(&function)
            .ok_or(FeatureError::UnknownFunction)?;
        if !entry.operations.write {
            return Err(FeatureError::NotWriteable);
        }
        if partial && !entry.operations.write_partial {
            return Err(FeatureError::PartialNotSupported);
        }
        if !update.entries_identified() {
            return Err(FeatureError::MissingIdentifier);
        }
        // A partial write appends every entry whose identifier matches nothing stored,
        // which is correct and also a way to grow a list one message at a time. The bound
        // is deliberately the worst case — nothing in the update matches — because
        // counting the matches means the merge, and a refusal has to happen before it.
        let stored_entries = entry.data.as_ref().map_or(0, CmdData::entry_count);
        let would_hold = if partial {
            stored_entries + update.entry_count()
        } else {
            update.entry_count()
        };
        if would_hold > MAX_LIST_ENTRIES {
            return Err(FeatureError::TooManyEntries);
        }
        match &mut entry.data {
            Some(stored) => stored.apply(update, partial)?,
            None => entry.data = Some(update),
        }
        Ok(())
    }

    /// Deletes what `update` identifies, for a command whose `cmdControl` says `delete`.
    pub fn delete(&mut self, update: &CmdData) -> Result<(), FeatureError> {
        let entry = self.writeable_function(update)?;
        if let Some(stored) = &mut entry.data {
            stored.delete(update)?;
        }
        Ok(())
    }

    /// Applies one `cmdControl: delete` filter, honouring what it addresses.
    ///
    /// A delete carries the same two filters a read does (SPINE §5.3.4.3): the selectors
    /// say which entries, the elements say which parts of them. Reading only the payload
    /// and ignoring both — which is what [`delete`](Self::delete) does — turns LPC UC TS
    /// §3.4.1.4's "withdraw this limit's `endTime`" into "remove this limit", and a
    /// curtailment that should have become open-ended is lifted instead.
    ///
    /// A filter naming a selector this implementation cannot match by comparison is
    /// refused with `errorNumber` 8 rather than served approximately, exactly as a
    /// partial read is.
    pub fn delete_filtered(
        &mut self,
        update: &CmdData,
        filter: &crate::model::Filter,
    ) -> Result<(), FeatureError> {
        let entry = self.writeable_function(update)?;
        let Some(stored) = &mut entry.data else {
            return Ok(());
        };
        let elements = filter.elements.as_ref();
        match filter.selectors.as_deref().unwrap_or_default() {
            [] => stored.delete_restricted(update, None, elements)?,
            // Two selectors address two sets of entries, and a delete of their union is
            // a delete of each in turn.
            many => {
                for selector in many {
                    stored.delete_restricted(update, Some(selector), elements)?;
                }
            }
        }
        Ok(())
    }

    /// The function `update` addresses, once it is known to be writeable and addressable.
    fn writeable_function(&mut self, update: &CmdData) -> Result<&mut FunctionEntry, FeatureError> {
        let function = Function::from(update.key());
        let entry = self
            .function_mut(&function)
            .ok_or(FeatureError::UnknownFunction)?;
        if !entry.operations.write {
            return Err(FeatureError::NotWriteable);
        }
        if !update.entries_identified() {
            return Err(FeatureError::MissingIdentifier);
        }
        Ok(entry)
    }

    fn to_function_properties(&self) -> Vec<FunctionProperty> {
        self.functions
            .iter()
            .map(|f| FunctionProperty {
                function: Some(f.function.clone()),
                possible_operations: Some(f.operations.to_model()),
            })
            .collect()
    }
}

/// Why an operation on a feature failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FeatureError {
    /// The write would leave the function holding more entries than [`MAX_LIST_ENTRIES`].
    #[error("the function would hold more than {MAX_LIST_ENTRIES} entries")]
    TooManyEntries,
    /// The feature does not declare that function.
    #[error("the feature does not support this function")]
    UnknownFunction,
    /// The function is not writeable.
    #[error("the function is not writeable")]
    NotWriteable,
    /// A partial write was attempted on a function that only accepts full writes.
    #[error("the function does not support partial writes")]
    PartialNotSupported,
    /// A list entry arrived without the identifiers that say which entry it is.
    #[error("a list entry is missing its identifiers")]
    MissingIdentifier,
    /// The payload belonged to a different function than the stored data.
    #[error(transparent)]
    Mismatch(#[from] FunctionMismatch),
    /// A filter addressed the data in a way this implementation cannot serve.
    #[error(transparent)]
    Restricted(#[from] crate::model::rfe::RestrictError),
}

impl FeatureError {
    /// The SPINE `errorNumber` this failure is reported with (SPINE IG §3.5).
    pub fn error_number(self) -> super::ErrorNumber {
        match self {
            Self::UnknownFunction => super::ErrorNumber::CommandNotSupported,
            Self::NotWriteable | Self::MissingIdentifier | Self::Mismatch(_) => {
                super::ErrorNumber::CommandRejected
            }
            Self::PartialNotSupported => super::ErrorNumber::RestrictedExchangeNotSupported,
            Self::Restricted(error) => error.error_number(),
            Self::TooManyEntries => super::ErrorNumber::Overload,
        }
    }
}

/// An entity of the local device.
#[derive(Clone, Debug)]
pub struct LocalEntity {
    address: Vec<u32>,
    entity_type: EntityType,
    features: Vec<LocalFeature>,
}

impl LocalEntity {
    /// An entity at `address` — a path, because entities nest.
    pub fn new(address: impl Into<Vec<u32>>, entity_type: EntityType) -> Self {
        Self {
            address: address.into(),
            entity_type,
            features: Vec::new(),
        }
    }

    /// Adds a feature.
    ///
    /// Returns [`DeviceError::DuplicateFeatureType`] if the entity already has a feature
    /// of that type **in that role**.
    ///
    /// The implementation guide §3.4 forbids two because discovery could not tell them
    /// apart, and a server and a client of one type are told apart by exactly what
    /// discovery reports. Real devices rely on this — `evcc` and the Porsche Mobile
    /// Charger Connect each carry a `DeviceDiagnosis` server and client on one entity — and
    /// so does LPC, whose Controllable System serves its own heartbeat and subscribes to
    /// the guard's.
    pub fn add_feature(&mut self, feature: LocalFeature) -> Result<(), DeviceError> {
        if self
            .features
            .iter()
            .any(|f| f.feature_type == feature.feature_type && f.role == feature.role)
        {
            return Err(DeviceError::DuplicateFeatureType);
        }
        self.features.push(feature);
        Ok(())
    }

    /// Adds a feature, panicking on a duplicate type.
    ///
    /// For building a device from a literal, where a duplicate is a programming error.
    #[must_use]
    pub fn with_feature(mut self, feature: LocalFeature) -> Self {
        self.add_feature(feature).expect("duplicate feature type");
        self
    }

    /// The entity's address path.
    pub fn address(&self) -> &[u32] {
        &self.address
    }

    /// The entity's type.
    pub fn entity_type(&self) -> &EntityType {
        &self.entity_type
    }

    /// The entity's features.
    pub fn features(&self) -> &[LocalFeature] {
        &self.features
    }

    /// Looks up a feature by its address.
    pub fn feature(&self, address: AddressFeature) -> Option<&LocalFeature> {
        self.features.iter().find(|f| f.address == address)
    }

    /// Looks up a feature by its address, for modification.
    pub fn feature_mut(&mut self, address: AddressFeature) -> Option<&mut LocalFeature> {
        self.features.iter_mut().find(|f| f.address == address)
    }

    /// Looks up a feature by type and role, which is how a use case finds the one it
    /// needs.
    pub fn feature_by_type(&self, feature_type: &FeatureType, role: Role) -> Option<&LocalFeature> {
        self.features
            .iter()
            .find(|f| &f.feature_type == feature_type && f.role == role)
    }

    /// Looks up a feature by type and role, for modification.
    pub fn feature_by_type_mut(
        &mut self,
        feature_type: &FeatureType,
        role: Role,
    ) -> Option<&mut LocalFeature> {
        self.features
            .iter_mut()
            .find(|f| &f.feature_type == feature_type && f.role == role)
    }
}

/// Why a device could not be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    /// The entity already has a feature of that type (SPINE IG §3.4).
    #[error("an entity may have at most one feature of each type")]
    DuplicateFeatureType,
    /// Two entities claim the same address.
    #[error("an entity with that address already exists")]
    DuplicateEntityAddress,
    /// The device address did not match the pattern of §7.1.1.2.
    #[error(transparent)]
    Address(#[from] super::AddressError),
}

/// This node, as SPINE sees it.
///
/// ```
/// use eebus::model::{DeviceType, EntityType, FeatureType, Function, Role};
/// use eebus::spine::{LocalDevice, LocalEntity, LocalFeature, Operations};
///
/// let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
///
/// let mut appliance = LocalEntity::new([1], EntityType::HeatPumpAppliance);
/// appliance
///     .add_feature(
///         LocalFeature::new(1, FeatureType::LoadControl, Role::Server)
///             .with_function(Function::LoadControlLimitListData, Operations::read_write()),
///     )
///     .unwrap();
/// device.add_entity(appliance).unwrap();
///
/// // Entity 0 is created for you: it carries the primary NodeManagement instance.
/// assert_eq!(device.entities().len(), 2);
/// assert!(device.node_management().is_some());
/// ```
#[derive(Clone, Debug)]
pub struct LocalDevice {
    address: AddressDevice,
    device_type: DeviceType,
    feature_set: NetworkManagementFeatureSet,
    entities: Vec<LocalEntity>,
}

impl LocalDevice {
    /// A device with the mandatory entity `[0]` already in place.
    ///
    /// `vendor` is `i:<IANA PEN>` or `n:<vendor name>`, and `unique` is whatever makes
    /// the address unique within that vendor.
    pub fn new(vendor: &str, unique: &str, device_type: DeviceType) -> Result<Self, DeviceError> {
        Ok(Self::from_address(
            address::device_address(vendor, unique)?,
            device_type,
        ))
    }

    /// A device with an address it already has, rather than one built from its parts.
    ///
    /// A SPINE device address is the identity a peer's bindings, subscriptions and audit
    /// records are filed under, so a device that restarts has to come back with the same
    /// one — and [`address`](Self::address) is where it was kept. Deriving it again from
    /// `vendor` and `unique` works only for a device that stored those two strings instead,
    /// and re-parsing the address to get them back is the kind of round trip that goes
    /// wrong quietly.
    ///
    /// The address is taken as given: it came from [`address`](Self::address), from a
    /// peer's discovery data, or from a capture, and each of those has already been through
    /// the parser.
    ///
    /// ```
    /// use eebus::prelude::*;
    ///
    /// let device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)?;
    /// let restored = LocalDevice::from_address(
    ///     device.address().clone(),
    ///     DeviceType::HeatGenerationSystem,
    /// );
    /// assert_eq!(restored.address(), device.address());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn from_address(address: AddressDevice, device_type: DeviceType) -> Self {
        let mut node_management =
            LocalEntity::new([NODE_MANAGEMENT_ENTITY], EntityType::DeviceInformation);
        node_management
            .add_feature(
                LocalFeature::new(
                    NODE_MANAGEMENT_FEATURE,
                    FeatureType::NodeManagement,
                    // "special": NodeManagement both serves its own data and consumes a
                    // peer's, so neither `server` nor `client` describes it.
                    Role::Special,
                )
                .with_function(
                    Function::NodeManagementDetailedDiscoveryData,
                    Operations::read(),
                )
                .with_function(Function::NodeManagementUseCaseData, Operations::read())
                .with_function(Function::NodeManagementBindingData, Operations::read())
                .with_function(Function::NodeManagementSubscriptionData, Operations::read()),
            )
            .expect("entity 0 is fresh, so its NodeManagement feature is its first");

        Self {
            address,
            device_type,
            feature_set: NetworkManagementFeatureSet::Smart,
            entities: vec![node_management],
        }
    }

    /// The device's address.
    pub fn address(&self) -> &AddressDevice {
        &self.address
    }

    /// The device's type.
    pub fn device_type(&self) -> &DeviceType {
        &self.device_type
    }

    /// The network-management feature set this device declares.
    pub fn feature_set(&self) -> &NetworkManagementFeatureSet {
        &self.feature_set
    }

    /// Sets the network-management feature set.
    #[must_use]
    pub fn with_feature_set(mut self, feature_set: NetworkManagementFeatureSet) -> Self {
        self.feature_set = feature_set;
        self
    }

    /// Adds an entity.
    pub fn add_entity(&mut self, entity: LocalEntity) -> Result<(), DeviceError> {
        if self.entities.iter().any(|e| e.address == entity.address) {
            return Err(DeviceError::DuplicateEntityAddress);
        }
        self.entities.push(entity);
        Ok(())
    }

    /// The device's entities, entity `[0]` first.
    pub fn entities(&self) -> &[LocalEntity] {
        &self.entities
    }

    /// Looks up an entity by its address path.
    pub fn entity(&self, address: &[u32]) -> Option<&LocalEntity> {
        self.entities.iter().find(|e| e.address == address)
    }

    /// Looks up an entity by its address path, for modification.
    pub fn entity_mut(&mut self, address: &[u32]) -> Option<&mut LocalEntity> {
        self.entities.iter_mut().find(|e| e.address == address)
    }

    /// The primary NodeManagement feature.
    pub fn node_management(&self) -> Option<&LocalFeature> {
        self.entity(&[NODE_MANAGEMENT_ENTITY])?
            .feature(AddressFeature(NODE_MANAGEMENT_FEATURE))
    }

    /// Resolves an address to the feature it names.
    pub fn resolve(&self, address: &FeatureAddress) -> Option<&LocalFeature> {
        if let Some(device) = &address.device
            && device != &self.address
        {
            return None;
        }
        let entity = self.entity(&address::entity_path(address))?;
        entity.feature(address.feature?)
    }

    /// Resolves an address to the feature it names, for modification.
    pub fn resolve_mut(&mut self, address: &FeatureAddress) -> Option<&mut LocalFeature> {
        if let Some(device) = &address.device
            && device != &self.address
        {
            return None;
        }
        let feature = address.feature?;
        let path = address::entity_path(address);
        self.entity_mut(&path)?.feature_mut(feature)
    }

    /// The full address of a feature of this device.
    pub fn address_of(&self, entity: &[u32], feature: u32) -> FeatureAddress {
        address::feature_address(&self.address, entity, feature)
    }

    /// Every feature of the device, with the entity path it lives on.
    pub fn features(&self) -> impl Iterator<Item = (&[u32], &LocalFeature)> {
        self.entities
            .iter()
            .flat_map(|e| e.features.iter().map(move |f| (e.address.as_slice(), f)))
    }

    pub(crate) fn function_properties(&self, feature: &LocalFeature) -> Vec<FunctionProperty> {
        feature.to_function_properties()
    }
}

/// Converts an entity path to the model's representation.
pub(crate) fn entity_addresses(path: &[u32]) -> Vec<AddressEntity> {
    path.iter().copied().map(AddressEntity).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LoadControlLimitData, LoadControlLimitId, LoadControlLimitListData};

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
    fn a_new_device_carries_the_primary_node_management_instance() {
        let device = LocalDevice::new("i:46925", "X", DeviceType::Generic).unwrap();
        let nm = device.node_management().expect("node management");
        assert_eq!(nm.feature_type(), &FeatureType::NodeManagement);
        assert_eq!(nm.role(), Role::Special);
        assert!(
            nm.function(&Function::NodeManagementDetailedDiscoveryData)
                .is_some()
        );
    }

    /// SPINE implementation guide §3.4: one feature of each type per entity.
    #[test]
    fn a_second_feature_of_the_same_type_is_refused() {
        let mut entity = LocalEntity::new([1], EntityType::HeatPumpAppliance);
        entity
            .add_feature(LocalFeature::new(1, FeatureType::LoadControl, Role::Server))
            .unwrap();
        assert_eq!(
            entity.add_feature(LocalFeature::new(2, FeatureType::LoadControl, Role::Server)),
            Err(DeviceError::DuplicateFeatureType)
        );
    }

    #[test]
    fn addresses_resolve_to_features() {
        let device = heat_pump();
        let address = device.address_of(&[1], 1);
        let feature = device.resolve(&address).expect("resolved");
        assert_eq!(feature.feature_type(), &FeatureType::LoadControl);

        assert!(device.resolve(&device.address_of(&[9], 1)).is_none());
        assert!(device.resolve(&device.address_of(&[1], 9)).is_none());
    }

    #[test]
    fn a_partial_write_merges_and_a_full_write_replaces() {
        let mut device = heat_pump();
        let address = device.address_of(&[1], 1);
        let feature = device.resolve_mut(&address).unwrap();

        let initial = CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(LoadControlLimitId(1)),
                is_limit_changeable: Some(true),
                value: Some(crate::model::ScaledNumber::new(4_200, 0)),
                ..Default::default()
            }]),
        });
        feature.set_data(initial).unwrap();

        let update = CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(LoadControlLimitId(1)),
                is_limit_active: Some(true),
                ..Default::default()
            }]),
        });
        feature.apply(update, true).unwrap();

        let Some(CmdData::LoadControlLimitListData(list)) =
            feature.data(&Function::LoadControlLimitListData)
        else {
            panic!("expected the limit list");
        };
        let entry = &list.load_control_limit_data.as_ref().unwrap()[0];
        assert_eq!(entry.is_limit_active, Some(true), "the update applied");
        assert_eq!(
            entry.is_limit_changeable,
            Some(true),
            "the stored element survived the partial write"
        );
    }

    #[test]
    fn writing_a_read_only_function_is_refused() {
        let mut device = heat_pump();
        let address = device.address_of(&[1], 2);
        let feature = device.resolve_mut(&address).unwrap();
        let update = CmdData::DeviceDiagnosisHeartbeatData(Default::default());
        assert_eq!(
            feature.apply(update, false),
            Err(FeatureError::NotWriteable)
        );
        assert_eq!(
            FeatureError::NotWriteable.error_number(),
            super::super::ErrorNumber::CommandRejected
        );
    }

    #[test]
    fn writing_an_undeclared_function_is_not_supported() {
        let mut device = heat_pump();
        let address = device.address_of(&[1], 1);
        let feature = device.resolve_mut(&address).unwrap();
        let update = CmdData::DeviceDiagnosisHeartbeatData(Default::default());
        assert_eq!(
            feature.apply(update, false),
            Err(FeatureError::UnknownFunction)
        );
        assert_eq!(
            FeatureError::UnknownFunction.error_number(),
            super::super::ErrorNumber::CommandNotSupported
        );
    }
}
