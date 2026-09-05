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
//! them, which is usually a handful of constants:
//!
//! * [`limitation`] serves [`lpc`] and [`lpp`], the same state machine pointed in
//!   opposite directions.
//! * [`monitoring`] serves [`mpc`] and [`mgcp`], the same measurements named from the
//!   appliance's side or the grid's — and [`moi`], [`mob`], [`mps`], [`cob`], the three
//!   temperatures of [`hvac`] and two of the [`emobility`] family besides, which are the
//!   same "describe a measurement twice and read it back" with a wider vocabulary.
//! * [`emobility::charging`] serves [`emobility::opev`] and [`emobility::oscev`], the same
//!   per-phase current ceiling for opposite reasons.
//! * [`hvac`] is three exchanges serving **twelve** use cases: [`hvac::system_function`]
//!   for the six operation-mode ones, [`hvac::setpoint`] for the three temperature-setting
//!   ones, [`hvac::temperature`] for the three thermometers.
//!
//! # What is here
//!
//! | | Use case | Actors |
//! |---|---|---|
//! | **Grid** | [`lpc`], [`lpp`] | Controllable System, Energy Guard |
//! | | [`mpc`], [`mgcp`] | Monitored Unit / Grid Connection Point, Monitoring Appliance |
//! | **E-mobility** | [`emobility`] | seven use cases: the wallbox, the car, its ceiling, its surplus, what it measured, and what the session cost |
//! | **Generation and storage** | [`moi`] | Inverter, Monitoring Appliance |
//! | | [`mps`] | PV String, Monitoring Appliance |
//! | | [`mob`] | Battery, Monitoring Appliance |
//! | | [`cob`] | Inverter, CEM — the only *control* use case outside the grid pair |
//! | **Heating and cooling** | [`hvac`] | all twelve HVAC use cases: the hot water's mode, overrun and temperature; a room's heating and cooling modes and setpoints; the room and outdoor thermometers |
//! | | [`ohpcf`] | Compressor, CEM — the CEM *starts* the compressor, and stops, pauses and resumes it |
//!
//! Every use case in the grid, e-mobility and storage rows can only ask an appliance to do
//! **less**: a limit, a ceiling, a current not to exceed. [`hvac`] and [`ohpcf`] are the
//! four levers that can ask for more — a temperature setpoint, an operation mode, a
//! one-time hot water loading, and the compressor's own optional consumption.
//!
//! # Bindings, and where they are not wanted
//!
//! SPINE §7.3 makes a binding a property of the **feature**: "some feature types define
//! requirements for binding". The use cases say which, and they do not agree:
//!
//! | | |
//! |---|---|
//! | binds | [`lpc`]/[`lpp`], [`emobility::opev`]/[`emobility::oscev`], [`cob`], [`emobility::evcs`], [`ohpcf`] scenario 2 |
//! | does not | every use case in [`hvac`], including all six that write |
//!
//! So [`crate::spine::WriteBinding`] is per feature, and the constructors here set it.

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
//! | *absent* | this actor has no timers at all. [`monitoring::MonitoringApplianceActor`] and [`hvac::HvacApplianceActor`] read descriptions and subscribe; the engine owns the retry ladder for all three |
//!
//! That is a rule rather than an inconsistency, and it is the reason the types differ: an
//! actor that must beat has a deadline it cannot be without, and saying so in the type is
//! worth more than a uniform signature that would make every caller unwrap a `Some` that
//! is always there.
//!
//! `handle_event` returns what the actor made of one engine event: an `Option` almost
//! everywhere, and a `Vec` from [`hvac::HvacApplianceActor`] alone. That is the payloads
//! rather than a taste: one `hvacSystemFunctionListData` carries every system function the
//! appliance has, so a single notification legitimately moves a room's heating *and* its
//! cooling, and there is no one event to return.
//!
//! A driver with several actors on one engine folds what they report and sleeps until the
//! earliest, then hands the same `now` to each `handle_timeout`. Order does not matter
//! between actors; within one, the engine is the only shared state and each actor touches
//! only its own features.
//!
//! # What silence means
//!
//! Every scenario of every use case here is subscription-driven: each UC TS §3.4.n.1 says
//! "Actors SHALL create a subscription for each server Feature that is relevant for the
//! corresponding Actor within this Scenario", and §3.3.4 names polling only as the
//! fallback for a subscription that was *refused*. So an actor never learns that a peer is
//! healthy by hearing from it on schedule — with one exception, and the descriptors carry
//! which:
//!
//! | | |
//! |---|---|
//! | [`Delivery::OnChange`] | sent when the value changes and at no other time. A room holding its temperature, a heat pump staying in `auto`, a tank at its setpoint — each sends nothing for hours and each is working. **The age of the last value is not a health signal**, and a driver that times out on it drops the units that are behaving |
//! | [`Delivery::Periodic`] | sent at least this often whether it changed or not. The heartbeats, and only the heartbeats: 60 s for LPC, LPP and COB, 4 s for OPEV and OSCEV. Silence past it **is** a fault, which is what arms the failsafe |
//!
//! Ask [`UseCaseDescriptor::delivery_of`](descriptor::UseCaseDescriptor::delivery_of)
//! rather than keeping a list of which of your own drivers subscribe — the answer is the
//! specification's, and `tests/use_case_delivery.rs` holds every actor here to it.
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
mod unit;

pub use descriptor::{ActorRole, Delivery, FunctionUse, Scenario, Support, UseCaseDescriptor};
pub use unit::UnitId;
