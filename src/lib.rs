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
//! | Use cases | `usecases` | `EEBus_UC_TS_*` + implementation guides |
//! | Information model | [`model`] | SPINE 1.3.0 Resource Specification |
//! | Wire format | [`codec`] | SHIP 1.1.0 §11.4 (XML→JSON) |
//! | Transport | `ship` | SHIP 1.0.1 / 1.1.0 |
//!
//! # Status
//!
//! Under construction. The SPINE model and its codec are in place; SHIP, the SPINE
//! engine and the use cases are landing milestone by milestone.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

pub mod codec;
pub mod model;
pub mod ship;
