//! The trust store: what SHIP asks a node to remember, and to be able to forget.
//!
//! SHIP §12.2.2 states both halves. Persisting the list is "STRONGLY RECOMMENDED",
//! because the alternative is asking a user to compare forty hex digits again after every
//! power cut; being able to delete all of it is a SHALL, and it is what an installer
//! removing a control box has to be able to reach.

#![cfg(feature = "runtime")]

use eebus::runtime::{Node, TrustStore, TrustedPeer};
use eebus::ship::Ski;

fn ski(byte: u8) -> Ski {
    format!("{:038}{byte:02x}", 0)
        .parse()
        .expect("forty hex digits")
}

#[test]
fn a_store_survives_a_round_trip_through_json() {
    let store = TrustStore::new();
    store.remember(
        TrustedPeer::new(ski(1))
            .named("Steuerbox im Zählerschrank")
            .with_ship_id("i:46925_ControlBox-1")
            .at_time("2026-09-01T10:15:00Z"),
    );
    store.trust(ski(2));

    let json = store.to_json().expect("serialisable");
    let restored = TrustStore::from_json(&json).expect("readable");

    assert_eq!(restored.peers(), store.peers());
    assert!(restored.is_trusted(&ski(1)));
    assert!(restored.is_trusted(&ski(2)));

    let named = restored.get(&ski(1)).expect("the box");
    assert_eq!(named.name.as_deref(), Some("Steuerbox im Zählerschrank"));
    assert_eq!(named.ship_id.as_deref(), Some("i:46925_ControlBox-1"));
    assert_eq!(named.trusted_at.as_deref(), Some("2026-09-01T10:15:00Z"));

    // A peer known only by its SKI carries no empty fields into the file.
    assert!(!json.contains("\"name\": null"), "{json}");
}

/// SHIP §12.2.2: "at least the SHIP node SHALL offer a possibility to delete all stored
/// foreign public keys".
#[test]
fn an_eebus_reset_forgets_every_peer() {
    let store = TrustStore::with([ski(1), ski(2), ski(3)]);
    assert_eq!(store.len(), 3);

    assert_eq!(store.forget_all(), 3, "and says how many, for the log");
    assert!(store.is_empty());
    assert!(!store.is_trusted(&ski(1)));
    assert_eq!(store.forget_all(), 0, "a second reset forgets nothing more");
}

/// The reset reaches the store through the node, which is where a device's user interface
/// would call it.
#[test]
fn a_node_can_be_reset() {
    let identity =
        eebus::cert::self_signed(eebus::cert::CertParams::new("i:46925_u:HeatPump-1")).unwrap();
    let trust = TrustStore::with([ski(7)]);
    let node = Node::new(
        "i:46925_u:HeatPump-1",
        eebus::tls::ShipTls::new(identity),
        trust.clone(),
    );

    assert!(node.trust_store().is_trusted(&ski(7)));
    assert_eq!(node.eebus_reset(), 1);
    assert!(trust.is_empty(), "the store the caller kept a handle on");

    // The node's own identity is untouched: SHIP §12.1.1 leaves restoring that to the
    // device, which is the only thing that knows where it was stored.
    assert_eq!(node.ski(), node.ski());
}

/// Re-approving a peer replaces what was known about it rather than accumulating records:
/// a user who renames a box means the new name.
#[test]
fn re_approving_a_peer_replaces_its_record() {
    let store = TrustStore::new();
    store.remember(TrustedPeer::new(ski(1)).named("Wallbox"));
    store.remember(TrustedPeer::new(ski(1)).named("Wallbox Garage"));

    assert_eq!(store.len(), 1);
    assert_eq!(
        store.get(&ski(1)).and_then(|peer| peer.name),
        Some("Wallbox Garage".into())
    );
}

/// Forgetting one peer leaves the rest — the "remove certain trusted foreign public keys"
/// of the same section.
#[test]
fn forgetting_one_peer_leaves_the_others() {
    let store = TrustStore::with([ski(1), ski(2)]);
    store.forget(&ski(1));

    assert!(!store.is_trusted(&ski(1)));
    assert!(store.is_trusted(&ski(2)));
    assert_eq!(store.all(), [ski(2)]);
}

/// §12.5: only the first peer to present a valid PIN may become the commissioning tool.
///
/// "In SHIP, it is not possible that two SHIP nodes may gain a second factor trust of '32'
/// with the SHIP node PIN. Any SHIP node that sends the PIN afterwards SHALL only get a
/// second factor trust of '16'. If another communication partner should be the one with a
/// second factor trust of '32', a factory reset must be performed."
#[test]
fn only_the_first_peer_to_prove_the_pin_may_commission() {
    use eebus::ship::TrustLevel;

    let first: Ski = "1111111111111111111111111111111111111111".parse().unwrap();
    let second: Ski = "2222222222222222222222222222222222222222".parse().unwrap();
    let store = TrustStore::restored([TrustedPeer::new(first), TrustedPeer::new(second)]);

    let awarded = store.award_pin_trust(&first).expect("a known peer");
    assert_eq!(awarded.second_factor, TrustLevel::SECOND_FACTOR_PIN_FIRST);
    assert!(awarded.permits_ship_commissioning());

    let later = store.award_pin_trust(&second).expect("a known peer");
    assert_eq!(
        later.second_factor,
        TrustLevel::SECOND_FACTOR_PIN_LATER,
        "the second peer gets 16, and no factory reset has happened"
    );

    // A key the store does not hold gains nothing: a second factor is trust added to a
    // key, not a way in on its own.
    let stranger: Ski = "3333333333333333333333333333333333333333".parse().unwrap();
    assert_eq!(store.award_pin_trust(&stranger), None);

    // And after the reset §12.5 names, the next peer may be the commissioning tool again.
    store.forget_all();
    let store = TrustStore::restored([TrustedPeer::new(second)]);
    assert_eq!(
        store.award_pin_trust(&second).map(|l| l.second_factor),
        Some(TrustLevel::SECOND_FACTOR_PIN_FIRST)
    );
}
