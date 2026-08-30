//! The machinery shared by Monitoring of Power Consumption and of the Grid Connection
//! Point.
//!
//! MPC and MGCP are the same exchange seen from two places. A *Monitored Unit* or a *Grid
//! Connection Point* publishes measurements — power, energy, current, voltage,
//! frequency — and a *Monitoring Appliance* reads them and subscribes for the rest. The
//! data model is identical: an `ElectricalConnection` feature binding each measurement to
//! its phases, and a `Measurement` feature saying what each one is and what it currently
//! reads.
//!
//! The two differ in what the scopes are called — MGCP names the energies from the grid's
//! side, `gridConsumption` and `gridFeedIn`, where MPC names them from the appliance's —
//! and in which scenarios each actor must implement. That is [`Naming`], and everything
//! else is shared.
//!
//! Use it through [`crate::usecases::mpc`] or [`crate::usecases::mgcp`], which carry the
//! descriptors for their use case.
//!
//! ```
//! use core::time::Duration;
//! use eebus::model::ElectricalConnectionPhaseName as Phase;
//! use eebus::usecases::monitoring::{Measurand, MonitoredUnit, Quantity, Readings};
//!
//! // A heat pump publishing its total draw and the current on each phase.
//! let mut unit = MonitoredUnit::new(1)
//!     .with(Measurand::total_power())
//!     .with(Measurand::on(Quantity::Current, Phase::A));
//! unit.set(&Measurand::total_power(), 2_300.0, Duration::ZERO);
//!
//! // What an energy manager makes of it, having read the descriptions first.
//! let mut readings = Readings::new();
//! readings.describe(&unit.measurement_descriptions());
//! readings.describe(&unit.parameter_descriptions());
//! readings.apply(&unit.measurements());
//!
//! assert_eq!(readings.total_power(), Some(2_300.0));
//! ```

mod appliance;
pub use appliance::*;

mod measurand;
pub use measurand::*;

mod unit;
pub use unit::*;
