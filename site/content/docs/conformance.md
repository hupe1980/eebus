+++
title = "Conformance"
description = "How eebus tracks the EEBUS specifications: certification test identifiers in the test names, deliberate departures from the reference implementations, and how to verify a claim."
weight = 140
[extra]
group = "Project"
+++

## Tests carry the certification's identifiers

`TC_SHIP_HELLO_002`, `TC_SPINE_COMP_006`, `TC_SPINE_RTS_005` and their siblings name the
tests that cover them. `cargo test` is therefore a pre-check for the laboratory rather than
a separate exercise, and a failing conformance test says which published test case it
corresponds to.

For the four certifiable use cases this goes further: all 203 abstract test cases of the
LPC, LPP, MPC and MGCP High-Level Test Specifications are carried as data **and driven**,
`cargo test` prints 189/203, and the fourteen that are not covered are exactly the
device-level ones — listed with the reason, and held to that by a test. See
[Certification](@/docs/certification.md).

## The fourteen the library cannot answer

A factory reset, a power cut, a start-up duration, what the appliance actually draws. No
library can answer those for the device it is linked into. `eebus::conformance::harness` is
the other half: seven procedures — each covering the LPC case and its LPP twin — carrying the
High-Level Test Specification's own steps, driven against a consumer's running binary.

```rust
use eebus::conformance::harness::{DeviceObservation, DeviceParameters, DeviceRun};

// §6.11.8: the start-up duration is not a number the specification fixes. It is one the
// manufacturer commits to in the parameter sheet, and is then held to.
let mut run = DeviceRun::new(
    DeviceParameters::new(Duration::from_secs(45))
        .failsafe(4_200.0, Duration::from_secs(2 * 3_600)),
);
run.observe(DeviceObservation::FactoryReset {
    limit_active: false,
    failsafe_watts: 4_200.0,
    failsafe_duration: Duration::from_secs(2 * 3_600),
});
println!("{}", run.report());
```

It **judges** against what the device declared — the failsafe defaults a reset must restore,
the `StartUpDur` a black start must return inside, the sixty seconds a rebooted Energy Guard
has to send a heartbeat and the limit after it. What it does not do is press the button.

`Inconclusive` is the verdict that earns its place: a persistence test that wrote back the
value the device already held proves nothing, and counting it as either a pass or a failure
would be a lie. `Skipped` is allowed only where the case is not mandatory for this device —
the two black starts are `r`, and mandatory only for one that declares itself black-start
capable.

`report().coverage()` gives the fourteen as a `Coverage`, to add to the protocol-level number
rather than quoting 189 of 203 for ever.

## Every claim points at a sentence

Citations live next to the code that satisfies them, not in a separate document that drifts.
To see what a given rule looks like in practice:

```sh
rg 'SHIP §|SPINE §|\[LPC-|\[OPEV-' src/
```

## Deliberate departures

Each of these differs from what a reference implementation does, and each is driven by the
specification text or the 2026 implementation guides.

**No "auto accept" mode, and `register` is never `true`.** SHIP IG §2.3 forbids both.

**Double connections follow §12.2.3 as written**: the larger SKI keeps the *most recent*
connection, and the smaller one waits three seconds and then pings. `ship-go` keeps the
initiator's connection instead — a deliberate deviation, because "most recent" is a local
judgement two nodes can disagree about, and disagreeing can leave both with nothing. The
rule is followed here and the hazard bounded instead: one ping round, then a decision, and a
redial if it does go wrong. [More](@/docs/ship.md#two-nodes-that-dial-each-other-at-once).

**Every table a peer can grow is capped.** Connections — handshakes in progress included —
peers waiting on an approval, tracked device addresses, bindings and subscriptions per
peer, remembered functions per peer, entries per function, and writes waiting on the
application. SHIP and SPINE cap none
of them; a peer that reaches a cap is told so — `errorNumber` 3 for a write or a relation,
a dropped socket or a `connectionClose` for a connection — rather than served until the
memory runs out.

**A binding or subscription call acts only in the sender's name.** The client address in
the payload has to belong to the device SHIP authenticated, or the call is refused with
`errorNumber` 7; a call that leaves the device part out is completed from the header. The
reference implementation fills the address in and checks nothing, which lets one paired
peer release another's binding.

**Discovery is filed under the header's source, not the payload's device.** A peer's
detailed discovery describes the device the *header* says sent it; a payload naming another
device cannot file itself under that device, nor rename a record already held.

**A heartbeat counts only from the Energy Guard that holds the bindings.** A Controllable
System subscribes to the diagnosis feature of the entity that bound both its features
(LPC IG §3.8), and a heartbeat from any other peer on the network does not keep its
failsafe at bay.

**A pairing is decided while the peer waits.** An unapproved peer is held in the SHIP
`hello: pending` state and reported; approving its SKI completes the handshake it is
waiting in, refusing it answers `hello: aborted`. There is no auto accept and no third
path.

**The SHIP Pairing Service is implemented, and trusts a certificate.** No reference
implementation has it. Its request names no SKI, so the trust store holds a `PairedUnit`
beside its SKI records and admits a peer whose fingerprint matches — which §10.2 says
"SHALL be seen like a successful trust in an SKI".

**A write the application never decides is abandoned** once §5.2.5's maximum response delay
has passed, and reported as `SpineEvent::WriteAbandoned`. An answer after that would reach a
peer that has already given up, and under §14a a limit that was never decided is worth a
log line rather than a silent disappearance.

**`accessMethods.id` is populated**, which SHIP 1.1.0 makes mandatory and which a peer needs
in order to dial back in the other direction.

**TLS 1.2 only**, because §9 says 1.3 "is not considered in this version".

**Session resumption is off.** §9.6 makes it a SHOULD, but a resumed session skips the
certificate exchange, and the certificate is what identifies a SHIP peer.

**`scale` is always sent**, per the SPINE IG §3.2.1 erratum, and a partial update merges into
the stored value rather than replacing it.

**Unsupported selectors are refused, not approximated** — `errorNumber` 8 rather than the
wrong entries.

**A deferred write is decided on the merged state.** The application is handed both: `data`
for the entries the write addresses, `resolved` for what they become (SPINE IG §3.3). The
reference implementations pass the fragment alone.

**A list entry without its identifiers is refused** — use-case IG §3.1 — rather than
appended as a new entry, which is what the reference implementations do. Which elements
*are* the identifiers comes from the Resource Specification's own Annex B.7, not from a
naming convention.

**The binding and subscription tables are served from the live relations** (§7.3.2, §7.4.2)
and **tailored to their recipient** (§7.4.3) — absent rather than empty when nothing concerns
them (Table 18). Serving the whole table tells every peer which other peers this device is
talking to.

**Discovery is subscribed to** (§7.5.4), which is the only way a peer's *departing* entity
is ever heard about.

**A `scaledNumber` that overflows `f64` reads as nothing, not as infinity**, and a value
that is present but unrepresentable makes the whole write unusable rather than approximate.

**A negative ISO 8601 duration reads as zero**, not as its magnitude; a duration with no
components — bare `P` or `PT` — is refused.

**A SPINE device address belongs to one connection.** Routing is by that address, so a peer
that restates a different one, or claims one another connection holds, is disconnected.

**The `hello` prolongation is driven by the `pending` node** (§13.4.4.1.3): it is the side
holding the connection up, and granting a request restarts the granter's own
Wait-For-Ready-Timer.

**A malformed `specificationVersion` is refused**, which `TC_SPINE_COMP_006` calls the
recommended behaviour, while tolerating the one deviation it permits: a leading `v`.

**Unchanged data is not notified** (SPINE IG §2.4), which is what makes a dozen
subscriptions affordable.

**The Subject Key Identifier extension is written by hand.** `rcgen` emits it only for
certificate authorities and a SHIP node certificate is a leaf, so without this the extension
§12.2 expects is absent and a peer cannot identify the node.

## Interoperability, measured

Conformance to a specification and interoperability with the implementations that also
claim it are different properties, and only one of them can be checked by reading. Round
trips over fixtures a crate produced itself prove that its encoder agrees with its decoder,
which is not the question.

**Recorded traffic.** `tests/fixtures/devices` carries fifteen datagrams captured from
eight devices by seven manufacturers — Elli, evcc, Kostal, Porsche, SMA, Spelsberg,
Vaillant and Viessmann — recorded with `eebus-go` and published as MIT-licensed
[`enbility/devices`](https://github.com/enbility/devices). Every one must parse, carry a
header this crate would accept off a socket, and resolve into a device model; between them
they name a dozen use cases and exercise eight feature types. They seed the fuzz corpora
too, so the fuzzers explore shapes real hardware produces rather than shapes this crate
produces.

**A live peer, in both directions.** An opt-in suite runs `eebus-go`'s own examples in a
container at a **pinned** revision and drives the whole §14a exchange against them:

| Their example | Their role | This crate's role | Who dials |
|---|---|---|---|
| `examples/controlbox` | Energy Guard | Controllable System | this crate |
| `examples/evse` | Controllable System | Energy Guard | this crate |
| `examples/controlbox` | Energy Guard | Controllable System | **`eebus-go`** |

Each run asserts the lot — TLS 1.2 with mutual authentication, the five-phase SHIP
handshake, SPINE discovery, the binding and the subscription, and a limit written, accepted
and recorded.

The third row is the direction §14a describes — the control box dials, the household
appliance listens — and it is the one that exercises this crate's accept path against
something it did not write. `eebus-go` discovers its peer over mDNS rather than taking an
address, so it needs the host's own network stack; the test skips with the reason where the
Docker daemon cannot give it that, and `tests/interop/Dockerfile` carries the recipe.

```sh
cargo test --features interop,full --test interop -- --nocapture
```

It is behind `required-features` and runs as its own CI job, so an ordinary `cargo test`
stays hermetic, needs no Docker and finishes in seconds. Pinning the peer is what makes a
failure legible: it means *this crate* changed, not that the other one did.

## How the crate is checked

| | |
|---|---|
| Unit and integration tests | `cargo test --workspace --features eebus/full` |
| Examples, end to end | `cargo run --example grid_limit`, `--example networked` |
| Both cryptography providers | the whole suite runs on `ring` and on `aws-lc-rs` |
| Property tests | round trips, the RFE merge laws and the wire arithmetic, via `proptest` |
| Real devices | fifteen datagrams from eight devices by seven manufacturers, parsed and resolved |
| A live peer, both ways | `eebus-go`'s own control box and EVSE in a container, at a pinned revision, doing the §14a exchange in each direction, and once with the peer dialling *in* to this crate's listener — opt-in, `--features interop` |
| Device-level conformance | `eebus::conformance::harness` — the fourteen cases a library cannot answer, as procedures a consumer runs against its own binary |
| What identifies a list entry | `tests/list_identifiers.rs` — derived from the Resource Spec's Annex B.7 and pinned per entry type |
| Fuzzing | five `cargo fuzz` targets, run nightly, seeded from the specification's examples **and** the device captures; their compilation checked on every push |
| Lints | `cargo clippy --workspace --all-targets --features eebus/full` with `-D warnings` |
| Documentation | `cargo doc` with `RUSTDOCFLAGS=-D warnings` |
| Documented counts | the generated model's size, held to the number the docs quote |
| Wire arithmetic | `cargo xtask check-floats` — no hand-rolled powers of ten or float casts where the wire meets a number |
| Minimum Rust version | `cargo +1.88.0 check` |
| Embedded | `cargo build --no-default-features --target thumbv7em-none-eabihf` |

`--workspace` covers the code generator in `xtask/`; the fuzz targets are a workspace of
their own and are compiled separately. `--all-features` is not used because it cannot be:
the two cryptography providers are mutually exclusive, and `full` is everything on `ring`.

All of it runs in CI on every push.
