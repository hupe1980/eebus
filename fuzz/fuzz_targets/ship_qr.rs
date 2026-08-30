//! The installation QR payload, which an installer's phone camera supplies.
//!
//! The input is whatever a scanner read off a sticker, which is to say whatever anybody
//! printed on one.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    if let Ok(qr) = text.parse::<eebus::ship::ShipQr>() {
        // A payload that parsed must produce one that parses to the same thing.
        let written = qr.to_payload();
        let back = written
            .parse::<eebus::ship::ShipQr>()
            .expect("what we wrote does not parse");
        assert_eq!(back, qr, "the QR payload is not stable");
    }
});
