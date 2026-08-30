//! An unofficial EEBUS implementation in Rust: SHIP transport, SPINE data model and
//! protocol, and the certifiable grid use cases on top.
//!
//! Guides, and the standard explained alongside the code:
//! <https://hupe1980.github.io/eebus/docs/>
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
//! | E-mobility | [`usecases::emobility`] | EVSECC 1.0.1, OPEV 1.0.1b |
//! | Protocol | [`spine`] | SPINE 1.3.0 Protocol Specification |
//! | Information model | [`model`] | SPINE 1.3.0 Resource Specification |
//! | Transport | [`ship`] | SHIP 1.0.1 / 1.1.0 |
//! | Wire format | [`codec`] | SHIP 1.1.0 §11.4 (XML→JSON) |
//! | Sockets | [`runtime`] | TCP, TLS 1.2, WebSocket, the connection table |
//!
//! # Sans-IO
//!
//! Nothing below [`runtime`] opens a socket or reads a clock. Both protocol cores —
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
//! A device is entities, and an entity is features. NodeManagement is created for you.
//!
//! ```
//! use core::time::Duration;
//! use eebus::prelude::*;
//!
//! let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)?;
//! device.add_entity(
//!     LocalEntity::new([1], EntityType::HeatPumpAppliance)
//!         .with_feature(limitation::load_control_feature(1))
//!         .with_feature(limitation::device_configuration_feature(2))
//!         .with_feature(limitation::device_diagnosis_feature(3)),
//! )?;
//!
//! let mut engine = Engine::new(device);
//! engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);
//!
//! // The engine turns application calls into datagrams and datagrams into events.
//! let source = engine.device().address_of(&[1], 1);
//! let peer = node_management(&device_address("i:12345", "ControlBox-1")?);
//! engine.read(&peer, &source, Function::NodeManagementDetailedDiscoveryData, Duration::ZERO);
//!
//! let datagram = engine.poll_transmit().expect("a read to send");
//! println!("{}", eebus::model::to_json(&datagram)?);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! On a network, [`runtime::Hub`] owns the sockets and the clock: it dials and accepts,
//! runs the opening discovery, routes each datagram to the peer it names, keeps idle
//! connections alive, and resolves the double connections two nodes that dial each other
//! at the same moment would otherwise both hold.
//!
//! See `examples/grid_limit.rs` for the whole §14a exchange against a virtual clock, and
//! `examples/networked.rs` for the same thing over a socket.
//!
//! # Status
//!
//! Under construction, and not yet published. The model, codec, SHIP handshake and
//! transport including certificate updates, the SPINE engine, the runtime, and all four
//! use cases certifiable since July 2026 — LPC, LPP, MPC and MGCP — are implemented and
//! tested on both sides of each, as are two of the e-mobility family
//! ([`usecases::emobility`]). The API will change.
//!
//! EEBUS® is a trademark of EEBus Initiative e.V. This project is not affiliated with or
//! endorsed by the EEBus Initiative.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(feature = "cert")]
#[cfg_attr(docsrs, doc(cfg(feature = "cert")))]
pub mod cert;
pub mod codec;
#[cfg(feature = "mdns")]
#[cfg_attr(docsrs, doc(cfg(feature = "mdns")))]
pub mod mdns;
pub mod model;
pub mod prelude;
#[cfg(feature = "runtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "runtime")))]
pub mod runtime;
pub mod ship;
pub mod spine;
#[cfg(feature = "tls")]
#[cfg_attr(docsrs, doc(cfg(feature = "tls")))]
pub mod tls;
pub mod usecases;
