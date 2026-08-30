//! The EEBUS-JSON codec, over arbitrary bytes.
//!
//! The datagram decoder is the largest parser in the crate — 841 generated types reached
//! through one entry point — and the one an attacker has the most room in.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let Ok(datagram) = eebus::model::from_json_str(text) else {
        return;
    };

    // Anything that decoded must encode again, and decode to the same value: the codec is
    // meant to be a bijection, and a value that changes on the way back out would change
    // every time it crossed the network.
    let Ok(reencoded) = eebus::model::to_json(&datagram) else {
        panic!("a decoded datagram failed to encode");
    };
    let Ok(again) = eebus::model::from_json_str(&reencoded) else {
        panic!("re-encoded output failed to decode");
    };
    assert_eq!(again, datagram, "the codec is not stable");
});
