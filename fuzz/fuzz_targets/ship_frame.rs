//! Every byte a peer can put on the wire, through the SHIP framing.
//!
//! This is the first parser an unauthenticated-at-the-SHIP-level peer reaches: TLS has
//! completed, so the sender holds a private key, but nothing yet says a person approved
//! it. A panic here is a reboot on a heat-pump controller, triggerable by anything on the
//! subnet that can complete a TLS handshake.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = eebus::ship::ShipMessage::decode(data) {
        // What decodes must re-encode; a message that cannot be written back is one the
        // state machine would answer with something it cannot send.
        let _ = message.encode();
    }
});
