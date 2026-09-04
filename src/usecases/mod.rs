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

//! # Driving an actor
//!
//! Every actor here is driven the same way, from the loop that owns the engine:
//!
//! ```text
//! actor.handle_event(&mut engine, &spine_event, now)   // one engine event
//! actor.handle_timeout(&mut engine, now)               // a deadline came due
//! actor.poll_timeout()                                 // when the next one is
//! ```
//!
//! `poll_timeout` reports an **absolute** instant on the same monotonic scale as the `now`
//! passed in, not a delay, and its *type* says something worth reading:
//!
//! | | |
//! |---|---|
//! | `Duration` | a deadline is always pending, because this actor produces a heartbeat. `ControllableSystemActor`, `EnergyGuardActor`, the guard side of [`emobility::charging`] |
//! | `Option<Duration>` | there may be nothing to wait for. Every state machine — [`limitation::ControllableSystem`], [`cob::BatteryControl`] — and the actors that produce no heartbeat of their own |
//! | *absent* | this actor has no timers at all. [`monitoring::MonitoringApplianceActor`] reads descriptions and subscribes; the engine owns the retry ladder for both |
//!
//! That is a rule rather than an inconsistency, and it is the reason the types differ: an
//! actor that must beat has a deadline it cannot be without, and saying so in the type is
//! worth more than a uniform signature that would make every caller unwrap a `Some` that
//! is always there.
//!
//! A driver with several actors on one engine folds what they report and sleeps until the
//! earliest, then hands the same `now` to each `handle_timeout`. Order does not matter
//! between actors; within one, the engine is the only shared state and each actor touches
//! only its own features.
//!
//! [`emobility::charging`]: crate::usecases::emobility::charging

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
pub mod ohpcf;
pub mod signals;

pub use descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};
