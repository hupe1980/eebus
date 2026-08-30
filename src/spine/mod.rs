//! SPINE: the information model and the rules for exchanging it.
//!
//! [`crate::model`] holds the data types; this module holds the protocol around them —
//! how messages are numbered and correlated, when an acknowledgement is owed, and which
//! error number says what.
//!
//! The datagram-level rules live here because they are small, exact, and easy to get
//! subtly wrong: acknowledging an acknowledgement produces an unbounded exchange,
//! answering a successful `read` with a result confuses peers, and a receiver that
//! insists message counters advance by exactly one will drop traffic from compliant
//! devices that restart.

mod ack;
pub use ack::*;

mod address;
pub use address::*;

mod device;
pub use device::*;

mod discovery;
pub use discovery::*;

mod engine;
pub use engine::*;

mod relations;
pub use relations::*;

mod counter;
pub use counter::*;
