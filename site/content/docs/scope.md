+++
title = "Scope and non-goals"
description = "What the eebus crate sets out to do, and what it deliberately does not: no JSON-UTF16, no Brainpool curves, no enhanced-mode routing, no persistence layer."
weight = 35
[extra]
group = "Foundations"
+++

## What it sets out to do

1. **Certifiable** implementations of LPC, LPP, MPC and MGCP on **both** sides of each —
   an energy manager needs the client actors as much as a heat pump needs the server ones.
   The same for every use case added afterwards.
2. **Correct where the reference implementations deviate**, and explicit about the
   difference. Each departure is listed under [Conformance](@/docs/conformance.md).
3. **Implementation-guide compliant by construction.** The 2026 guides are errata as much
   as guidance — mandatory `scale`, the heartbeat-then-limit sequence gate, no auto accept,
   the binding lock — and they belong in types and defaults rather than in an integrator's
   checklist.
4. **A sans-IO core**, so every timer in the standard is an ordinary unit test and the same
   code runs under Tokio, in a simulator, or on a microcontroller.
5. **`no_std + alloc`** for everything below the socket.
6. **No `unsafe`** anywhere (`#![forbid(unsafe_code)]`), and every parser the network can
   reach behind a fuzz target.

## What it deliberately does not do

Each of these is a decision, not an omission waiting to be filled in.

**Being an energy manager.** The crate provides actors and events. What limit the grid
needs, and whether a device can follow one, are the application's decisions — and the API
is shaped so they are the *only* decisions it has to make.

**The JSON-UTF16 message format.** Negotiated away during the handshake; UTF-8 only.

**SHIP commissioning** (`commissioningRequest`, `keyMaterialRequest`, …). The Installation
Requirements Annex A.6 does not require it.

**Brainpool curves.** secp256r1 only, unless a regulator demands otherwise. The API does
not hard-code the curve.

**Enhanced-mode SPINE routing** — destination lists, forwarding on another node's behalf.
A datagram addressed elsewhere is answered with `errorNumber` 5.

**A second TLS backend for the CBC suite.** SHIP marks
`TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA256` a SHALL, and `rustls` implements no CBC suite, so
a peer offering CBC alone cannot connect. Doubling the TLS surface for a suite the
Installation Requirements advise against, that the certification suite does not test, and
that no observed device requires, is not worth it. `ShipTls::mandatory_suite_available`
reports the gap rather than hiding it.

**Persistence.** Everything that must survive a restart is `serde`-serialisable and
reachable — `TrustStore::to_json`, `Hub::peer_keys`, `AuditLog::drain`, `CsConfig`. Where it
is kept, and how long a regulator requires it to be kept, only the application knows.

**A telemetry module, a CLI, C FFI, language bindings.** Each is a product of its own, and
none of them is this crate.
