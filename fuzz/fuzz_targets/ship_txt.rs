//! The `_ship._tcp` TXT record, as multicast DNS delivers it.
//!
//! Unauthenticated by construction: anything on the subnet can announce anything.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    // Split into key/value pairs the way a TXT record arrives.
    let pairs: Vec<(&str, &str)> = text
        .split('\u{1}')
        .filter_map(|entry| entry.split_once('='))
        .collect();
    if let Ok(record) = eebus::ship::ShipTxtRecord::from_pairs(&pairs) {
        let Ok(written) = record.to_pairs() else {
            // A record that parsed but cannot be written back is a bug worth knowing
            // about, but not one this target can distinguish from a peer sending a
            // value we tolerate on the way in and refuse on the way out.
            return;
        };
        let borrowed: Vec<(&str, &str)> = written
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let back = eebus::ship::ShipTxtRecord::from_pairs(&borrowed)
            .expect("what we wrote does not parse");
        assert_eq!(back.ski, record.ski, "the SKI changed on the way out");
    }
});
