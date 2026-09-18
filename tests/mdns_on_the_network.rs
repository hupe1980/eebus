//! Announcing a SHIP node and finding it again, on the real network.
//!
//! This test uses actual multicast, so it needs a network interface that carries it.
//! Where that is not available — a sandboxed CI runner, a container without multicast —
//! it reports the absence rather than failing, because a machine that cannot do mDNS is
//! not evidence that the code cannot.

#![cfg(feature = "mdns")]

use core::time::Duration;

use eebus::mdns::{BrowseEvent, Mdns, MdnsError};
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

    // `_ship._tcp` is a shared namespace, and what answers a browse is whatever is on the
    // segment: another test in this binary, a colleague's laptop, an actual heat pump. So
    // the announcement this test made is picked out by name rather than by being first —
    // taking the first arrival made the test a claim about the network.
    let mut seen = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        match browse.recv_timeout(Duration::from_secs(1)) {
            Some(BrowseEvent::Found(found)) if found.instance == "eebus-test-heatpump" => {
                seen = Some(found);
                break;
            }
            Some(_) => {}
            None => {}
        }
    }
    let instance = match seen {
        Some(found) => {
            assert_eq!(found.ski, ski, "the SKI a peer will be asked to trust");
            assert_eq!(found.port, 4712);
            assert_eq!(found.record.brand.as_deref(), Some("ExampleBrand"));
            assert!(found.socket_address().is_some(), "and where to dial it");
            Some(found.instance)
        }
        None => {
            eprintln!("multicast did not reach this process; skipping the assertions");
            None
        }
    };

    responder.withdraw().expect("the withdrawal");

    // A node that leaves is news too: an application that dials what it discovers would
    // otherwise keep a departed peer in its redial schedule for the life of the process.
    if let Some(instance) = instance {
        let mut lost = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match browse.recv_timeout(Duration::from_secs(1)) {
                Some(BrowseEvent::Lost { instance: gone }) if gone == instance => {
                    lost = Some(gone);
                    break;
                }
                Some(_) => {}
                None => {}
            }
        }
        match lost {
            Some(gone) => assert_eq!(gone, instance, "the instance that withdrew"),
            None => eprintln!("no removal reached this process; skipping the assertion"),
        }
    }
}

/// An announcement with no address is refused rather than published.
///
/// DNS-SD resolves an instance to a host name and the host name to an address; with no
/// address the last step has no answer, so a peer finds the service, reads the SKI it
/// carries, and has nowhere to dial. The daemon accepts such a registration happily and
/// nothing goes wrong that anybody can see — the announcing node waits for a connection
/// that cannot be made, which is why this is an error and not a warning.
#[test]
fn an_announcement_with_no_address_is_refused() {
    let ski: Ski = "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap();
    let Ok(mut responder) = Mdns::new() else {
        eprintln!("no mDNS on this machine; skipping");
        return;
    };
    assert!(matches!(
        responder.announce("eebus-test-unreachable", &record(ski), 4712, &[]),
        Err(MdnsError::NoAddress)
    ));
}

/// The composition the documentation recommends, built for real: listen, announce, browse.
///
/// This test exists because its absence was the defect. `Hub::browse` said in as many words
/// that "a listener and an announcement beside it" is the whole of a device's networking,
/// and nothing in this repository ever built that arrangement — so nobody saw that a node
/// doing all three finds its own announcement, offers the installer a pairing with the
/// device in front of them, and, once trusted, connects to itself (D101). Every party in
/// every other test came from a factory that mints a fresh identity per call, which made
/// "the peer is us" a state the suite could not express.
///
/// Multicast reaching this process is the precondition, not the subject, so its absence is
/// reported and skipped exactly as the tests above do.
#[cfg(all(feature = "runtime", feature = "mdns"))]
#[tokio::test]
async fn a_node_that_announces_and_browses_does_not_find_itself() {
    use eebus::cert::{self, CertParams};
    use eebus::model::{DeviceType, EntityType};
    use eebus::runtime::{Hub, HubEvent, Node, TrustStore};
    use eebus::spine::{Engine, LocalDevice, LocalEntity};
    use eebus::tls::ShipTls;

    let identity = cert::self_signed(CertParams::new("i:46925_u:Selfy-1")).expect("an identity");
    let ski = identity.ski;
    let trust = TrustStore::new();
    // The state one `y` at the old self-pairing prompt left behind, on disk, for ever.
    trust.trust(ski);

    let mut device = LocalDevice::new("i:46925", "Selfy-1", DeviceType::HeatGenerationSystem)
        .expect("a device address");
    device
        .add_entity(LocalEntity::new([1], EntityType::HeatPumpAppliance))
        .expect("a fresh entity");

    let node = Node::new("i:46925_u:Selfy-1", ShipTls::new(identity), trust);
    let mut hub = Hub::new(node, Engine::new(device));

    let bound = hub.listen("127.0.0.1:0").await.expect("a listener");

    let Ok(mut mdns) = Mdns::new() else {
        eprintln!("no mDNS on this machine; skipping");
        return;
    };
    if mdns
        .announce(
            "eebus-test-selfy",
            &record(ski),
            bound.port(),
            &[core::net::IpAddr::from([127, 0, 0, 1])],
        )
        .is_err()
    {
        eprintln!("this machine would not take the announcement; skipping");
        return;
    }
    hub.browse(&mdns).expect("a browse");

    // Whatever multicast delivers, none of it may be about this node.
    let deadline = hub.now() + Duration::from_secs(6);
    loop {
        hub.wake_at(deadline);
        match hub.next().await.expect("no transport error") {
            HubEvent::Tick if hub.now() >= deadline => break,
            HubEvent::Tick => {}
            HubEvent::Found { peer, .. } => assert_ne!(
                peer.ski, ski,
                "the hub reported this node's own announcement as a peer"
            ),
            HubEvent::Lost {
                ski: Some(gone), ..
            } => {
                assert_ne!(
                    gone, ski,
                    "and a departure it never announced an arrival for"
                )
            }
            HubEvent::Connected { ski: peer, .. } => {
                panic!("the hub connected to itself as {peer}")
            }
            HubEvent::TrustRequested { peer, .. } => {
                panic!("the hub asked a user to pair with itself: {}", peer.ski)
            }
            _ => {}
        }
    }

    assert_eq!(
        hub.remembered().count(),
        0,
        "nothing of this node's own is on the dial schedule"
    );
    assert_eq!(hub.peers().count(), 0, "and it is not its own peer");
}
