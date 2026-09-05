//! Which unit of a peer something belongs to.
//!
//! One device is regularly several units. The use-case implementation guide §3.3 puts an
//! actor's features on the entity that announced it, and §7.5 keys use-case information by
//! address rather than by device, so nothing stops a device announcing the same actor
//! several times: a heat-pump gateway publishes one `HVACRoom` per room, each with its own
//! features, its own temperature and its own operation mode. Keying by device makes the
//! second room evict the first, and the failure is silent in the worst way — the bedroom's
//! temperature reported as the living room's, in the right unit and a plausible range.
//!
//! [`UnitId`] is therefore **device and entity**, and one type rather than one per family:
//! a room's [`mrt`](crate::usecases::hvac::mrt) thermometer and its
//! [`crht`](crate::usecases::hvac::crht) setpoint are the same entity, so
//! [`MonitoringApplianceActor`](crate::usecases::monitoring::MonitoringApplianceActor) and
//! [`HvacApplianceActor`](crate::usecases::hvac::HvacApplianceActor) hand back one key for
//! both facts.

use alloc::vec::Vec;

use crate::model::{AddressDevice, FeatureAddress};

/// Which unit of a peer something belongs to: an **entity**, not a device.
///
/// Obtained from [`MonitoredUnitPeer::id`](crate::usecases::monitoring::MonitoredUnitPeer::id)
/// or [`HvacPeer::id`](crate::usecases::hvac::peer::HvacPeer::id), carried by every
/// [`MonitoringEvent`](crate::usecases::monitoring::MonitoringEvent) and
/// [`HvacEvent`](crate::usecases::hvac::HvacEvent), and what every accessor on both actors
/// takes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnitId {
    /// The device the unit is on.
    pub device: AddressDevice,
    /// The entity path within it, as detailed discovery gave it: `[1]`, `[1, 2]`.
    ///
    /// Empty only for a peer whose features carried no entity address at all, which no
    /// conformant discovery reply does.
    pub entity: Vec<u32>,
}

impl UnitId {
    /// The unit a feature lives on.
    ///
    /// [`None`] for an address naming no device, which is what a locally-scoped address is
    /// and what no peer's discovery data contains.
    pub fn of(feature: &FeatureAddress) -> Option<Self> {
        Some(Self {
            device: feature.device.clone()?,
            entity: crate::spine::entity_path(feature),
        })
    }

    /// Whether `feature` is on this unit.
    ///
    /// Both halves are checked, and the device first: two devices may well number an
    /// entity and a feature the same way, and a comparison that skipped the device would
    /// route one gateway's living room to another's.
    pub fn holds(&self, feature: &FeatureAddress) -> bool {
        feature.device.as_ref() == Some(&self.device)
            && crate::spine::entity_path(feature) == self.entity
    }
}
