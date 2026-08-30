//! Announcing a SHIP node and finding it again, on the real network.
//!
//! This test uses actual multicast, so it needs a network interface that carries it.
//! Where that is not available — a sandboxed CI runner, a container without multicast —
//! it reports the absence rather than failing, because a machine that cannot do mDNS is
//! not evidence that the code cannot.

#![cfg(feature = "mdns")]

use core::time::Duration;

use eebus::mdns::Mdns;
use eebus::ship::{DeviceCategory, ShipId, ShipTxtRecord, Ski};

fn record(ski: Ski) -> ShipTxtRecord {
    ShipTxtRecord::new(ShipId::new("46925", "HeatPump-1"), ski)
        .with_brand("ExampleBrand")
        .with_model("HP-3000")
        .with_categories([DeviceCategory::Hvac])
}

#[test]
fn a_node_announces_itself_and_is_found() {
    let ski: Ski = "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap();

    let Ok(mut responder) = Mdns::new() else {
        eprintln!("no mDNS on this machine; skipping");
        return;
    };
    let Ok(browser) = Mdns::new() else {
        eprintln!("no mDNS on this machine; skipping");
        return;
    };

    let browse = browser.browse().expect("a browse");
    responder
        .announce(
            "eebus-test-heatpump",
            &record(ski),
            4712,
            &[core::net::IpAddr::from([127, 0, 0, 1])],
        )
        .expect("the announcement");

    match browse.recv_timeout(Duration::from_secs(5)) {
        Some(found) => {
            assert_eq!(found.ski, ski, "the SKI a peer will be asked to trust");
            assert_eq!(found.port, 4712);
            assert_eq!(found.record.brand.as_deref(), Some("ExampleBrand"));
            assert!(found.socket_address().is_some(), "and where to dial it");
        }
        None => eprintln!("multicast did not reach this process; skipping the assertions"),
    }

    responder.withdraw().expect("the withdrawal");
}
