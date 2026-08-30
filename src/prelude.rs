//! The types an application ends up importing anyway.
//!
//! ```
//! use eebus::prelude::*;
//!
//! let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)?;
//! device.add_entity(
//!     LocalEntity::new([1], EntityType::HeatPumpAppliance)
//!         .with_feature(limitation::load_control_feature(1)),
//! )?;
//! let engine = Engine::new(device);
//! # let _ = engine;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Nothing here is unavailable elsewhere; the module exists so that building a device
//! does not begin with a dozen `use` lines. What it deliberately leaves out is anything
//! ambiguous: the use-case modules are re-exported as modules rather than glob-imported,
//! because `lpc::ENERGY_GUARD` and `lpp::ENERGY_GUARD` are different constants — as are
//! `limitation::GuardEvent` and `emobility::opev::GuardEvent` — and a prelude that hid
//! the difference would be a trap.

pub use crate::model::{
    AddressDevice, CmdData, DeviceType, EntityType, ErrorNumber, FeatureAddress, FeatureType,
    Function, Role, ScaledNumber,
};
pub use crate::ship::{OwnKeys, PeerKeys, Ski, Trust};
pub use crate::spine::{
    Engine, HeartbeatMonitor, HeartbeatProducer, LocalDevice, LocalEntity, LocalFeature,
    Operations, SpineEvent, device_address, feature_address, node_management,
};
pub use crate::usecases::{ActorRole, UseCaseDescriptor};

pub use crate::usecases::{emobility, limitation, lpc, lpp, mgcp, monitoring, mpc};

#[cfg(feature = "cert")]
#[cfg_attr(docsrs, doc(cfg(feature = "cert")))]
pub use crate::cert::{self, CertParams, Identity};

#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
pub use crate::tls::ShipTls;

#[cfg(feature = "runtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "runtime")))]
pub use crate::runtime::{Hub, HubEvent, Node, TrustStore};

#[cfg(feature = "mdns")]
#[cfg_attr(docsrs, doc(cfg(feature = "mdns")))]
pub use crate::mdns::{Discovered, Mdns};
