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

## The cryptography provider is the consumer's choice

`rustls` 0.23 can be backed by `ring` or by `aws-lc-rs`, and this crate names neither.
`cert`, `tls` and `runtime` require exactly one of the `ring` and `aws-lc-rs` features, and
a build that names both — or neither — stops with a `compile_error!` that says why.

The reason is not tidiness. `rustls`' provider is *process-global*: a binary that links both
panics the first time anything asks for the default. A library that quietly pulled one in
would be making that choice for every consumer downstream of it, including one whose
`deny.toml` bans it — and the result is not a compile error but a household box that panics
the first time a control box connects.

```toml
eebus = { version = "0.3", features = ["runtime", "ring"] }
# or, for a build that must not contain `ring`:
eebus = { version = "0.3", default-features = false, features = ["std", "runtime", "aws-lc-rs"] }
```

Nothing here ever reads the process default. The provider is built explicitly, with exactly
the three cipher suites and the one key-exchange group §9 permits — the post-quantum hybrids
`aws-lc-rs` offers by default are dropped, because a SHIP node may not negotiate one — so a
consumer that has installed its own default keeps it. `eebus::tls::CRYPTO_PROVIDER` says
which backend a running binary got, which is worth a line in the start-up log.

## Forgetting a peer

SHIP §12.2.2 states two things about the trust list, and this crate serves both.
Persisting it is "STRONGLY RECOMMENDED", because the alternative is asking a user to compare
forty hex digits again after every power cut — `TrustStore::to_json` and `from_json` are the
two calls that make saving it a one-liner, and `TrustedPeer` carries the name and SHIP ID a
person needs to tell one line of the store from the next.

Being able to delete all of it is a SHALL: *"at least the SHIP node SHALL offer a possibility
to delete all stored foreign public keys (e.g. via factory reset)."* That is
`Node::eebus_reset`, and it is what an "EEBUS reset" on a device's user interface has to
reach. It returns how many peers were forgotten, and it deliberately does not restore the
node's own certificate — §12.1.1 asks a factory reset to bring back the identity printed on
the label, and only the device knows where that was stored.

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
* **A timing oracle on a short secret** — the two comparisons that matter, a SHIP PIN and a
  pairing secret, go through one constant-time equality rather than stopping at the first
  differing byte. A PIN's whole defence is the escalating penalty of SHIP §13.4.4.3.4, and
  that penalty counts *attempts*: an equality that leaks a byte at a time turns eight hex
  digits into thirty-two guesses, and the penalty never notices.
* **Resource exhaustion** — every table a peer can grow is bounded. The hub holds at most
  `DEFAULT_MAX_CONNECTIONS` connections and two per peer, refusing the rest with a
  `connectionClose`; the engine tracks at most `MAX_PEERS` device addresses, evicting the one
  that has told it least, and at most `MAX_REMOTE_FUNCTIONS` of each peer's functions; at
  most `MAX_DEFERRED_WRITES` writes may be waiting on the application, and one function holds
  at most `MAX_LIST_ENTRIES` — after any of which a peer is answered `errorNumber` 3,
  overload, rather than served. A device address is bounded and printable or the datagram is
  discarded. SHIP itself caps none of this, and the omission is exploitable: a device that
  dials in a thousand times takes the memory of every node that answers.

What is *not* defended: an attacker with the private key is the node, and an attacker who
can make the user approve their SKI is paired. Both are the intended semantics of the
standard.
