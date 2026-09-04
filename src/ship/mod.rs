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

mod fingerprint;
pub use fingerprint::*;

mod discovery;
pub use discovery::*;

mod qr;
pub use qr::*;

mod keys;
pub use keys::*;

mod policy;
pub use policy::*;

mod trust;
pub use trust::*;

mod spine;
pub use spine::*;

#[cfg(feature = "pairing")]
pub mod pairing;

/// Compares two byte strings without an early exit.
///
/// Two secrets are compared here: a PIN, whose whole defence is the escalating penalty of
/// SHIP §13.4.4.3.4, and a pairing secret. A comparison that stops at the first differing
/// byte turns either into an oracle that gives up one byte at a time, and the penalty is
/// counted per *attempt* rather than per byte, so it does not help.
///
/// Unequal lengths are reported at once. A length is not the secret: a PIN's is announced
/// in the handshake and a pairing secret's is fixed by whoever printed the sticker.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
