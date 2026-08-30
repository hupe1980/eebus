//! Limitation of Power Consumption (LPC).
//!
//! An *Energy Guard* — a grid operator's control box, or an energy manager acting for
//! one — tells a *Controllable System* how much power it may draw. The use case is the
//! technical basis for §14a EnWG in Germany, and the first one the EEBUS certification
//! covers.
//!
//! Four scenarios make it up:
//!
//! 1. **Control active power consumption limit** — the limit itself, with an optional
//!    duration, acknowledged or refused by the Controllable System.
//! 2. **Failsafe values** — what applies when the Energy Guard falls silent, and for how
//!    long at least.
//! 3. **Heartbeat** — a message every sixty seconds in each direction, whose absence is
//!    what triggers the failsafe.
//! 4. **Constraints** — the nominal maxima, so the Energy Guard knows what it is
//!    limiting.
//!
//! [`ControllableSystem`] implements the state machine of §2.3 together with the rules
//! the 2026 implementation guide added.

mod descriptor;
pub use descriptor::*;

mod state;
pub use state::*;
