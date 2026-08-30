//! An EEBUS implementation in Rust: SHIP transport, SPINE data model and protocol, and
//! the certifiable grid use cases on top.
//!
//! EEBUS is the communication standard that connects heat pumps, wallboxes, batteries,
//! inverters and energy managers to each other and to the grid operator. It is the
//! interface German §14a EnWG installations are built on, and since July 2026 it is
//! formally certifiable for the four grid use cases LPC, LPP, MPC and MGCP.
//!
//! # Layers
//!
//! | Layer | Module | Specification |
//! |---|---|---|
//! | Use cases | [`usecases`] | `EEBus_UC_TS_*` + the 2026 implementation guides |
//! | Protocol | [`spine`] | SPINE 1.3.0 Protocol Specification |
//! | Information model | [`model`] | SPINE 1.3.0 Resource Specification |
//! | Transport | [`ship`] | SHIP 1.0.1 / 1.1.0 |
//! | Wire format | [`codec`] | SHIP 1.1.0 §11.4 (XML→JSON) |
//!
//! # Sans-IO
//!
//! Nothing in this crate opens a socket or reads a clock. Both protocol cores —
//! [`ship::Handshake`] and [`spine::Engine`] — are driven the same way:
//!
//! ```text
//! handle_message(msg, now) / handle_datagram(msg, now)   // something arrived
//! handle_timeout(now)                                    // a timer fired
//! poll_transmit()                                        // what to send
//! poll_timeout()                                         // when to come back
//! poll_event()                                           // what the application should know
//! ```
//!
//! Time is a parameter, so every timeout in the standard — the two-minute hello, the
//! SPINE response deadline, the LPC heartbeat window — is an ordinary unit test against a
//! virtual clock, and the same code runs under an async runtime, in a simulator, or on a
//! microcontroller.
//!
//! # Getting started
//!
//! ```
//! use core::time::Duration;
//! use eebus::model::{DeviceType, EntityType, FeatureType, Function, Role};
//! use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature, node_management};
//!
//! // A device is entities, and an entity is features. NodeManagement is created for you.
//! let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)?;
//! device.add_entity(
//!     LocalEntity::new([1], EntityType::HeatPumpAppliance)
//!         .with_feature(LocalFeature::new(1, FeatureType::LoadControl, Role::Server)),
//! )?;
//!
//! // The engine turns application calls into datagrams and datagrams into events.
//! let mut engine = Engine::new(device);
//! let source = engine.device().address_of(&[1], 1);
//! let peer = node_management(&eebus::spine::device_address("i:12345", "ControlBox-1")?);
//! engine.read(&peer, &source, Function::NodeManagementDetailedDiscoveryData, Duration::ZERO);
//!
//! let datagram = engine.poll_transmit().expect("a read to send");
//! println!("{}", eebus::model::to_json(&datagram)?);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! See `examples/grid_limit.rs` for the whole §14a exchange: the SHIP handshake,
//! discovery, a binding, a limit, and the acknowledgement that answers it.
//!
//! # Status
//!
//! Under construction, and not yet published. The model, codec, SHIP handshake, SPINE
//! engine and all four use cases certifiable since July 2026 — LPC, LPP, MPC and MGCP —
//! are implemented and tested; TLS, mDNS-SD and a runtime adapter are the remaining
//! milestones. The API will change.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(feature = "cert")]
#[cfg_attr(docsrs, doc(cfg(feature = "cert")))]
pub mod cert;
pub mod codec;
pub mod model;
#[cfg(feature = "runtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "runtime")))]
pub mod runtime;
pub mod ship;
pub mod spine;
#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
pub mod tls;
pub mod usecases;
