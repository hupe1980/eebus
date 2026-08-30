+++
title = "Security model"
description = "How eebus establishes trust: self-signed certificates identified by Subject Key Identifier, TLS 1.2 with mutual authentication, certificate rotation, and the threat model."
weight = 130
[extra]
group = "Deployment"
+++

## Trust is a list of SKIs

There is no certificate authority in EEBUS and no central directory. Each node holds a
self-signed ECDSA P-256 certificate, and its identity is the **Subject Key Identifier** —
twenty bytes derived from the public key. Pairing means each side records the other's SKI.

That is the whole model, and it is a good fit for a building: nobody has to be online, and
no vendor can revoke a heat pump's ability to talk to its own energy manager.

The crate enforces it with a custom `rustls` verifier on both sides. The certificate chain
is not validated against a root store — there is no root — but the presented certificate
must carry the SKI that was approved, and the SKI must be derived from the key that
actually completed the handshake.

## TLS 1.2, mutual, no resumption

* **1.2 only**, because SHIP §9 says 1.3 "is not considered in this version".
* **Mutual authentication** always: both sides present certificates.
* **Session resumption is off.** §9.6 makes it a SHOULD, but a resumed session skips the
  certificate exchange, and the certificate is what identifies a SHIP peer. The saving is
  not worth losing the identity check.
* The cipher suites are those §9.1 mandates; a peer offering only CBC suites will not
  connect.

## Rotating a certificate

Replacing a certificate changes the node's name, so SHIP §12.1.3 defines a procedure rather
than leaving it to chance: the new material is announced with an `updateCounter`, the peer
records it, and the relationship survives. The crate implements it end to end, and the
`Hub` follows the update so an application does not have to.

## Pairing

Two paths, both deliberate acts:

* **Manual** — the user is shown the peer's SKI and approves it. The handshake stays
  `pending` meanwhile, prolonging its wait up to the two-minute cap.
* **Pairing Service** — an installer scans a QR code carrying a shared secret, and the
  digest exchange (Pairing Service 1.0.0 §7) establishes trust automatically. A replay
  guard (§11) prevents a captured exchange being reused.

There is no third path. `register` is never advertised as `true` and there is no
"auto accept" mode, both of which SHIP IG §2.3 forbids.

## Threat model

The attacker is on the building's LAN. They can send SHIP frames, malformed JSON, oversized
datagrams and bad certificates at any node.

* **Parsing** — the framing, the codec, the QR payload, the TXT record and a whole datagram
  through the engine are each a `cargo fuzz` target, run nightly in CI.
* **Impersonation** — requires the peer's private key. A SKI is a hash of the public key;
  presenting the SKI without the key fails the handshake.
* **Replay** — SPINE message counters are monotonic per source; the Pairing Service has its
  own guard.
* **Resource exhaustion** — the connection table is bounded, as is the peer table in the
  engine.

What is *not* defended: an attacker with the private key is the node, and an attacker who
can make the user approve their SKI is paired. Both are the intended semantics of the
standard.
