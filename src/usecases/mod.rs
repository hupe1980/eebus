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
//!   appliance's side or the grid's — and [`moi`], [`mob`], [`mps`], [`cob`],
//!   [`hvac::mdt`] and two of the [`emobility`] family besides, which are the same
//!   "describe a measurement twice and read it back" with a wider vocabulary.
//! * [`emobility::charging`] serves [`emobility::opev`] and [`emobility::oscev`], the same
//!   per-phase current ceiling for opposite reasons.
//!
//! # What is here
//!
//! | | Use case | Actors |
//! |---|---|---|
//! | **Grid** | [`lpc`], [`lpp`] | Controllable System, Energy Guard |
//! | | [`mpc`], [`mgcp`] | Monitored Unit / Grid Connection Point, Monitoring Appliance |
//! | **E-mobility** | [`emobility`] | six use cases: the wallbox, the car, its ceiling, its surplus, and what it measured |
//! | **Generation and storage** | [`moi`] | Inverter, Monitoring Appliance |
//! | | [`mps`] | PV String, Monitoring Appliance |
//! | | [`mob`] | Battery, Monitoring Appliance |
//! | | [`cob`] | Inverter, CEM — the only *control* use case outside the grid pair |
//! | **Heating** | [`hvac::cdt`] | DHW Circuit, Configuration Appliance — a hot water *setpoint* |
//! | | [`hvac::mdsf`] | DHW Circuit, Monitoring Appliance — which mode it is in |
//! | | [`hvac::mdt`] | DHW Circuit, Monitoring Appliance — what the water got to |
//!
//! Every control use case here but one can only ask an appliance to do **less**: a limit,
//! a ceiling, a current not to exceed. [`hvac`] is the exception, and that is why it is
//! here — a hot water tank is a thermal battery, and asking it for a higher temperature
//! while the roof is exporting is something no limit can express.

pub mod addressing;
pub mod cob;
pub mod descriptor;
pub mod emobility;
pub mod hvac;
pub mod limitation;
pub mod lpc;
pub mod lpp;
pub mod mgcp;
pub mod mob;
pub mod moi;
pub mod monitoring;
pub mod mpc;
pub mod mps;
pub mod signals;

pub use descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};
