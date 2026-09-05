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
//! | E-mobility | [`usecases::emobility`] | EVSECC, EVCC, OPEV, OSCEV, EVCEM, EVSOC |
//! | Generation, storage | [`usecases::moi`], [`usecases::mps`], [`usecases::mob`], [`usecases::cob`] | MOI, MPS, MOB, COB |
//! | Protocol | [`spine`] | SPINE 1.3.0 Protocol Specification |
//! | Information model | [`model`] | SPINE 1.3.0 Resource Specification |
//! | Transport | [`ship`] | SHIP 1.0.1 / 1.1.0 |
//! | Wire format | [`codec`] | SHIP 1.1.0 §11.4 (XML→JSON) |
//! | Sockets | [`runtime`] | TCP, TLS 1.2, WebSocket, the connection table |
//! | Certification | [`conformance`] | The four High-Level Test Specifications, as data |
//!
//! # The cryptography provider is yours to pick
//!
//! [`cert`], [`tls`] and [`runtime`] need one of the `ring` or `aws-lc-rs` features, and
//! **this crate names neither by default**. `rustls`' provider is process-global: a binary
//! that links both panics the first time anything asks for the default, so the choice
//! belongs to whoever builds the binary. Naming both, or naming neither, is a
//! `compile_error!` rather than a device that panics the first time a control box
//! connects; [`tls::CRYPTO_PROVIDER`] says which one a running binary got.
//!
//! Nothing here ever reads `rustls`' process default — the provider is built explicitly,
//! with exactly the cipher suites and the one key-exchange group SHIP §9 permits — so a
//! consumer that has installed its own keeps it.
//!
//! `--all-features` is therefore not a configuration that can exist. `--features full` is
//! everything, on `ring`.
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
//! On a network, [`runtime::Hub`] owns the sockets and the clock: it listens, dials and
//! browses in the background, asks the application when a peer needs a trust decision,
//! runs the opening discovery, routes each datagram to the peer it names, keeps idle
//! connections alive, and resolves the double connections two nodes that dial each other
//! at the same moment would otherwise both hold.
//!
//! # Pairing
//!
//! Two ways in, and no third: SHIP IG §2.3 forbids an "auto accept" mode.
//!
//! **A person approves a SKI.** An unapproved peer completes TLS, which proves its SKI,
//! and is held in the SHIP `hello: pending` state and reported as
//! [`runtime::HubEvent::TrustRequested`]; [`runtime::Hub::approve`] completes the
//! handshake it is waiting in, [`runtime::Hub::refuse`] ends it.
//!
//! **Or nobody is asked.** The **SHIP Pairing Service** ([`ship::pairing`]) is for a
//! control unit installed by somebody who never sees the household's screen: it is
//! configured from that device's QR code and announces a `_shippairing._tcp` request whose
//! digest proves it knows the printed secret. [`runtime::Hub::accept_pairing_requests`]
//! and [`runtime::Hub::browse_pairing`] receive; [`ship::pairing::Requester`] sends. What
//! is trusted is a [`ship::Fingerprint`], which §10.2 makes equivalent to a SKI.
//!
//! See `examples/grid_limit.rs` for the whole §14a exchange against a virtual clock, and
//! `examples/networked.rs` for the same thing over a socket. `examples/steuerbox.rs` and
//! `examples/heat_pump.rs` are the two halves as separate programs — a control box and a
//! household appliance that find each other over mDNS, remember whom they trust, and print
//! the runtime signals of [`usecases::signals`] as they go.
//!
//! # Matching on an event
//!
//! Every enum this crate *reports* things through is `#[non_exhaustive]`, so a `match` on
//! one needs a `_` arm. That is deliberate and it is the reverse of an inconvenience: the
//! crate keeps finding things worth reporting — a peer withholding a PIN, a use case
//! appearing at runtime, a request that walked the whole retry ladder — and without the
//! attribute every one of those is a breaking change for every consumer that matches
//! exhaustively.
//!
//! The line is drawn by *who closes the set*:
//!
//! | | |
//! |---|---|
//! | `#[non_exhaustive]` | anything this crate closes: [`HubEvent`](runtime::HubEvent), [`SpineEvent`](spine::SpineEvent), [`ship::Event`], every actor's event and error type |
//! | exhaustive | anything the **specification** closes: [`LimitationState`](usecases::limitation::LimitationState)'s five states, [`Trust`](ship::Trust), `PinState`, every generated enumeration |
//!
//! So a `match` on a state machine's states is still checked for completeness — which is
//! the case where you want to be told about a new one — and a `match` on a stream of events
//! is not.
//!
//! # Certification
//!
//! [`conformance`] carries the 203 abstract test cases of the LPC, LPP, MPC and MGCP
//! High-Level Test Specifications as data: what a laboratory will run, which requirement
//! each covers, and whether it is mandatory. `tests/conformance.rs` runs this crate
//! against them and prints a coverage number, which is worth having before a slot is
//! booked rather than after.
//!
//! # Status
//!
//! Under construction, and pre-1.0. The model, codec, SHIP handshake and
//! transport including certificate updates, the SPINE engine, the runtime, and all four
//! use cases certifiable since July 2026 — LPC, LPP, MPC and MGCP — are implemented and
//! tested on both sides of each. So are twenty-four more: seven of the e-mobility family
//! ([`usecases::emobility`]), four for inverters, PV strings and batteries, all twelve
//! HVAC ones ([`usecases::hvac`]), and the heat-pump compressor flexibility
//! ([`usecases::ohpcf`]) a CEM uses to start a process. The API will change.
//!
//! EEBUS® is a trademark of EEBus Initiative e.V. This project is not affiliated with or
//! endorsed by the EEBus Initiative.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

// `rustls` 0.23 and `rcgen` 0.14 can each be backed by `ring` or by `aws-lc-rs`, and the
// choice is process-global: a binary that links both panics the first time anything asks
// `rustls` for its default provider. So it is the consumer's to make, and a library that
// made it silently would be making it for every consumer downstream of it.
//
// Hence: `cert`, `tls` and `runtime` require exactly one of the two features, and a build
// that names both or neither stops here rather than at the first TLS connection.
#[cfg(all(feature = "cert", feature = "ring", feature = "aws-lc-rs"))]
compile_error!(
    "eebus: both `ring` and `aws-lc-rs` are enabled, and rustls' provider is \
     process-global — a binary linking both panics on its first connection. Enable \
     exactly one. Note that `--all-features` therefore does not work: use \
     `--features full` (which is everything, on `ring`) or name the features you want."
);
#[cfg(all(feature = "cert", not(feature = "ring"), not(feature = "aws-lc-rs")))]
compile_error!(
    "eebus: `cert`, `tls` and `runtime` need a cryptography provider, and this crate does \
     not choose one for you: enable either `ring` or `aws-lc-rs`. `--features full` is \
     everything on `ring`."
);

#[cfg(feature = "cert")]
#[cfg_attr(docsrs, doc(cfg(feature = "cert")))]
pub mod cert;
pub mod codec;
#[cfg(feature = "conformance")]
#[cfg_attr(docsrs, doc(cfg(feature = "conformance")))]
pub mod conformance;
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
