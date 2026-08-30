//! EEBUS use cases.
//!
//! A use case is the application layer of EEBUS: it names the *actors* involved, the
//! *scenarios* they run, and the SPINE features and functions each scenario needs. Four
//! of them — Limitation of Power Consumption and Production, and Monitoring of Power
//! Consumption and of the Grid Connection Point — have been certifiable since July 2026
//! and are what German §14a EnWG installations are built on.
//!
//! Each use case is a module holding its [`UseCaseDescriptor`]s, one per actor, which is
//! what a device publishes in `nodeManagementUseCaseData` and what a peer reads to find
//! out what it is talking to. Where two use cases share their machinery, that machinery
//! lives in a module of its own and the use-case modules carry only what distinguishes
//! them — which, for both of the pairs here, is a handful of lines:
//!
//! * [`limitation`] serves [`lpc`] and [`lpp`], the same state machine pointed in
//!   opposite directions.
//! * [`monitoring`] serves [`mpc`] and [`mgcp`], the same measurements named from the
//!   appliance's side or the grid's.

pub mod descriptor;
pub mod emobility;
pub mod limitation;
pub mod lpc;
pub mod lpp;
pub mod mgcp;
pub mod monitoring;
pub mod mpc;

pub use descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};
