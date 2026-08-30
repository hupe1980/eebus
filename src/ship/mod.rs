//! SHIP: Smart Home IP, the EEBUS transport.
//!
//! SHIP carries SPINE between two devices on a local network. A connection is
//! discovered over mDNS-SD, secured with mutual TLS 1.2 against self-signed ECDSA
//! certificates, framed inside a WebSocket sub-protocol, and opened with a five-phase
//! handshake that establishes which peer trusts which key.
//!
//! This module implements SHIP 1.0.1 with the 1.1.0 extensions, and follows the SHIP
//! implementation guide 1.0.0 where it overrides the base specification.

mod generated;
pub use generated::*;

mod frame;
pub use frame::*;

mod handshake;
pub use handshake::*;

mod ski;
pub use ski::*;

mod discovery;
pub use discovery::*;

mod qr;
pub use qr::*;

#[cfg(feature = "pairing")]
pub mod pairing;
