+++
title = "eebus"
template = "index.html"
[extra]
landing = true
lede = """An unofficial EEBUS implementation in Rust: the SHIP transport, the SPINE information model, and the four grid use cases certifiable since July 2026 — built to pass certification, not merely to interoperate."""
status = "Under construction. The stack is complete from the socket to the use case; the API will change and nothing is published to crates.io yet."

hero_code = """
```rust
// A heat pump answers the grid operator's limit itself —
// under §14a EnWG the acknowledgement *is* the record.
match engine.poll_event() {
    Some(SpineEvent::WriteRequested(w)) => {
        let write = limitation::read_limit_write(&w.resolved)?;
        match system.on_limit_write(&write, decide(&write), now) {
            WriteOutcome::Accepted => engine.accept_write(w.token, now),
            outcome => engine.reject_write(w.token, outcome.error_number(), now),
        }
    }
    _ => {}
}
```
"""

[[extra.stats]]
value = "830"
label = "SPINE types, generated"
[[extra.stats]]
value = "6"
label = "use cases, both actors"
[[extra.stats]]
value = "no_std"
label = "below the runtime"
[[extra.stats]]
value = "TLS 1.2"
label = "mutual auth, SKI-pinned"

[[extra.features]]
title = "Sans-IO protocol core"
body = "The SHIP handshake and the SPINE engine touch neither sockets nor clocks. Time is a parameter, so the two-minute hello, the SPINE response deadline and LPC's two-hour failsafe are ordinary unit tests against a virtual clock."

[[extra.features]]
title = "Certifiable, not just interoperable"
body = "Tests carry the certification's own identifiers — TC_SHIP_HELLO_002, TC_SPINE_COMP_006 and their siblings — so cargo test is a pre-check for the laboratory rather than a separate exercise."

[[extra.features]]
title = "Illegal states are unrepresentable"
body = "A command carries a payload choice, so two payloads in one command cannot be built. A LoadControlLimitId cannot be passed where a MeasurementId belongs — a mix-up that matters, because LPC requires the two to match."

[[extra.features]]
title = "One implementation per pair"
body = "LPC and LPP are the same use case pointed in opposite directions; MPC and MGCP share their measurements. Each pair is written once, so a fix to one is a fix to both."

[[extra.features]]
title = "Runs on the controller"
body = "Everything below the runtime builds for no_std + alloc, checked in CI against thumbv7em-none-eabihf. Five fuzz targets cover everything the network can reach, because on a heat-pump controller a panic is a reboot."

[[extra.features]]
title = "The wire format, directly"
body = "EEBUS JSON-UTF8 is encoded through serde in a single streaming pass — no intermediate tree to rewrite, no allocation for field names. Round trips are byte for byte."
+++

## The exchange, end to end

A distribution grid operator's control box tells a heat pump how much power it may draw.
Under §14a EnWG the building has to honour that limit, and the acknowledgement it sends
back is the record that it did. Every layer below is in this crate.

```text
mDNS-SD  ──▶  TCP  ──▶  TLS 1.2  ──▶  WebSocket  ──▶  SHIP  ──▶  SPINE  ──▶  use case
 find it     connect    mutual auth    framing      handshake    model      the decision
```

## Both sides, not just the appliance

A heat pump needs the Controllable System. An energy manager needs the Energy Guard, and
the 2026 implementation guides spend most of their pages on that half. Both are here, and
the guard's rules — heartbeat before write, no deactivation as the first limit after a
reconnection, a refusal retried a minute later, one write per five minutes otherwise —
live behind a single call.

```rust
guard.require(&device, Some(LimitWrite::active(4_200.0)), now);
```

## Generated from the schemas, not from a reading of them

830 SPINE types and 47 SHIP messages are emitted from the XML Schemas the specifications
ship, so the model cannot drift from the standard. The generated Rust is committed:
building the crate never needs the specifications.

## One rule you cannot get wrong twice

Restricted Function Exchange merges partial updates element by element: an omitted element
means *unchanged*, all the way down. Send a `scaledNumber` carrying a bare `number` and
the stored `scale` has to survive — otherwise a 4.2 kW limit silently becomes 42 MW. The
generator resolves the type-to-selector link the schemas leave implicit into a table over
141 functions, and the merge is written once.
