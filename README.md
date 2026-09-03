# eebus

An unofficial [EEBUS](https://www.eebus.org) implementation in Rust: the SHIP transport,
the SPINE information model, and the grid use cases that German §14a EnWG installations
are built on.

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Docs](https://img.shields.io/badge/docs-hupe1980.github.io%2Feebus-0b8f63.svg)](https://hupe1980.github.io/eebus/)

📖 **[Documentation](https://hupe1980.github.io/eebus/docs/)** — the standard explained
alongside the code. [What EEBUS is](https://hupe1980.github.io/eebus/docs/introduction/) ·
[Getting started](https://hupe1980.github.io/eebus/docs/getting-started/) ·
[LPC and LPP](https://hupe1980.github.io/eebus/docs/limitation/) ·
[Conformance](https://hupe1980.github.io/eebus/docs/conformance/)

> **Status: under construction.** The stack is complete from the socket to the use case,
> and all four use cases certifiable since July 2026 — LPC, LPP, MPC and MGCP — are
> implemented on **both** sides and measured against all 203 of their published abstract
> test cases, as are six of the e-mobility family and four more for inverters, PV strings
> and batteries. Not published to crates.io yet; the API will change.

## What EEBUS is, and why this exists

EEBUS is how a heat pump, a wallbox, a battery and an energy manager talk to each other
and to the grid operator's control box. Since July 2026 four of its use cases — limiting
and monitoring power consumption and production — are formally certifiable, and in Germany
they are the mechanism behind §14a EnWG and EEG §9: when the low-voltage grid is congested,
the distribution operator sends a limit and the building has to honour it, whether it is
drawing power or feeding it in.

The reference implementations are Go, Java and C. This crate is the Rust one, built to be
certifiable rather than merely interoperable.

## Design

**One crate, layered.** `codec` → `model` → `ship` → `spine` → `usecases`, with `runtime`
on top for the socket. Feature flags rather than a family of crates to keep in step.

**Generated from the schemas.** 830 SPINE types and 47 SHIP messages, emitted by
`cargo xtask codegen` from the XML Schemas the specifications ship, so the model cannot
drift from the standard. The generated Rust is committed: building never needs the specs.

**Sans-IO protocol core.** The SHIP handshake and the SPINE engine touch neither sockets
nor clocks:

```rust
engine.handle_datagram(&datagram, now);   // something arrived
engine.handle_timeout(now);               // a timer fired
engine.poll_transmit();                   // what to send
engine.poll_timeout();                    // when to come back
engine.poll_event();                      // what the application should know
```

Every timeout in the standard — the hello prolongation dance, LPC's two-hour failsafe — is
then an ordinary unit test against a virtual clock. Everything below `runtime` builds for
`no_std + alloc`, checked in CI against `thumbv7em-none-eabihf`.

The contract between the two halves is one line, and it is asserted rather than assumed:
**waiting until exactly `poll_timeout()` is enough.** One test drives every state machine
from nothing but its own deadlines.

**The awkward rules are handled once.** Restricted Function Exchange merges partial updates
element by element: an omitted element means *unchanged*, all the way down. Send a
`scaledNumber` with a bare `number` and the stored `scale` has to survive, or a 4.2 kW limit
becomes 42 MW. Nothing in the schemas links a data type to its selectors and elements
filters — the link is a naming convention — so the generator resolves it into a table
covering 141 functions, and merge, delete and partial read are written once. A command may
ask for more than one of them — LPC's own worked example carries two filters, one deleting a
limit's `endTime` and one writing its new value — so they are applied in order, and a
delete's `selectors` choose the entries while its `elements` choose the parts of them.

**Announce nothing you cannot do.** `possibleOperations` is a promise, so partial reads are
announced only for functions that table covers; a filtered request for anything else gets
`errorNumber` 8 rather than an approximate answer. The binding and subscription tables are
answered from the relations actually held. The table is
[published](https://hupe1980.github.io/eebus/docs/functions/), generated from the same
source the compiler reads, so "which SPINE variations does this implement" has an answer
you can read before committing.

**A partial write is decided on the merged state.** The same rule, a layer up: the engine
hands the application both — `data` for the entries a write addresses, `resolved` for what
they become — and the use cases decide on `resolved`, where an absent `isLimitActive` still
means *unchanged*. An entry without its identifiers is refused (UC IG §3.1), not appended.
A partial *notification* is the same rule pointed the other way: the engine holds the merged
state of every function a peer has sent, and hands over both payloads there too.

**The application decides, not the library.** A limit write is answered by the use case,
not a generic setter: under §14a EnWG the acknowledgement *is* the record that the limit was
applied, so it has to say what the appliance actually did.

```rust
if let Some(SpineEvent::WriteRequested(w)) = engine.poll_event() {
    let write = limitation::read_limit_write(&w.resolved)?;   // merged, not the fragment
    let outcome = system.on_limit_write(&write, decide(&write), now);
    if outcome.is_accepted() {
        engine.accept_write(w.token, now)?;                         // ACK — or what the peer was told instead
    } else {
        engine.reject_write(w.token, outcome.error_number(), now);  // NACK
    }
}
```

**Both sides of every use case.** A heat pump needs the Controllable System; an energy
manager needs the Energy Guard, and the 2026 implementation guides spend most of their pages
on that half. Here the guard holds it:

```rust
guard.require(&device, Some(LimitWrite::active(4_200.0)), now);
```

and nothing before it: the guard takes up a peer that announces the Controllable System by
itself, and a limit required before that peer is reachable is held rather than dropped.
Behind that line: a heartbeat immediately before the write and only once the peer has
subscribed to it (§2.11); never a deactivation as the first limit after a reconnection
(§2.13); never a zero duration on an activated limit (§2.2); a refusal retried a minute later
and a minute later again, with the device kept (§2.5); and no more than one write every five
minutes otherwise (§2.10).

**Two use cases, one implementation.** LPC and LPP are the same use case pointed in opposite
directions — the same four scenarios, table numbers and thirteen transitions — so the state
machine and both actors are written once and pointed by a `Direction`. OPEV and OSCEV are
the same pair on the e-mobility side, pointed by a `Purpose`; seven use cases share one
measurement layer. The reference implementations duplicate each; here a fix to one is a fix
to all.

**Illegal states are unrepresentable.** A SPINE command carries a payload *choice*, so "two
payloads in one command" cannot be built. Identifiers are distinct types, so a
`LoadControlLimitId` cannot be passed where a `MeasurementId` belongs — a mix-up that
matters, because LPC requires the limit's `measurementId` to match MPC's. A Controllable
System comes from a builder that ends in `install`, so an actor that never published its
limit description — and would therefore never be sent a limit — cannot be constructed.

**Secrets redact themselves.** A PIN and a pairing secret print as `<redacted>` from the
`Debug` of every type that holds one, and compare in constant time. A PIN's whole defence is
SHIP's escalating penalty, and a PIN in a support bundle has none.

**Every number off the wire is treated as hostile.** `scale` is signed 16-bit, so a `number`
near `i64::MAX` reaches infinity in one well-formed message; `to_f64` answers `None`, and a
use case handed a present-but-unreadable value NACKs the write rather than substituting one.
A duration pointing backwards reads as zero, not as its magnitude.

**And converted exactly.** A scale is applied in steps of at most `10^22`, the largest power
of ten an `f64` holds exactly, so a negative scale *divides* by an exact constant rather
than multiplying by an inexact one, and a scale past the table is still that scale. A value
too large for an `i64` raises the scale instead of saturating. `cargo xtask check-floats`
keeps hand-rolled powers of ten and float casts out of the two modules where the wire meets
a number.

**Nothing a peer sends can grow without bound.** SHIP and SPINE cap neither connections nor
stored state. A hub holds sixteen connections and two per peer, handshakes in progress
included; the engine tracks thirty-two device addresses, thirty-two remembered functions
and thirty-two bindings and subscriptions per peer, sixteen undecided writes and 128
entries per function, and answers `errorNumber` 3 beyond any of them. A device address — a
routing key, a stored identity, a line in the §14a record — is bounded and printable or the
datagram is discarded. A write nobody decides is abandoned once the peer has stopped
waiting.

**And nothing a peer sends speaks for another peer.** A binding or subscription call names
its client in the payload; the engine honours it only from the device SHIP authenticated as
the sender, so one paired peer cannot release another's binding. Discovery is filed under
the header's source, not the payload's claim. A heartbeat keeps a Controllable System out of
its failsafe state only when it comes from the Energy Guard that holds its bindings.

**Fuzzed and property-tested.** Five `cargo fuzz` targets cover everything the network can
reach — SHIP framing, the JSON codec, the QR payload, the TXT record, a whole datagram
through the engine — and property tests cover the round trips, the merge laws, and the
arithmetic above. On a heat-pump controller a panic is a reboot.

**Tested against implementations it did not write.** Fifteen datagrams captured from eight
real devices by seven manufacturers ([`enbility/devices`](https://github.com/enbility/devices))
are parsed, **driven through an engine**, resolved into a device model, and used to seed the
fuzzers — as are all twenty-nine Restricted Function Exchange examples from the
specification's own annex, which are served rather than only decoded. And
`cargo test --features interop` runs `eebus-go`'s own examples in a container at a pinned
revision — their control box against this crate's Controllable System, their EVSE against
its Energy Guard — driving the whole §14a exchange each way. Opt-in and its own CI job, so
an ordinary `cargo test` needs no Docker.

**Tests carry the certification's identifiers.** `TC_SHIP_HELLO_002`, `TC_SPINE_COMP_006`
and their siblings name the tests that cover them. For the four certifiable use cases,
`eebus::conformance` carries all 203 abstract test cases of the High-Level Test
Specifications as data, the suite **drives every one of them**, and `cargo test` prints
189/203. The fourteen missing are exactly the seven device-level cases — a factory reset, a
power cut, a start-up duration — counted for LPC and LPP, each listed with its reason, and
held to that by a test.

## The wire format

SPINE over SHIP is "JSON-UTF8": a JSON projection of the XML Schema in which every sequence
becomes an **array of single-key objects** (SHIP §11.4). A heartbeat looks like this:

```json
{"datagram":[
  {"header":[{"specificationVersion":"1.3.0"},{"msgCounter":7},{"cmdClassifier":"notify"}]},
  {"payload":[{"cmd":[[
    {"function":"deviceDiagnosisHeartbeatData"},
    {"deviceDiagnosisHeartbeatData":[{"heartbeatCounter":12},{"heartbeatTimeout":"PT1M"}]}
  ]]}]}
]}
```

Other implementations serialise to ordinary JSON and rewrite the tree afterwards. This one
encodes the format directly through `serde` in a single streaming pass, with no intermediate
`Value` and no allocation for field names — so `to_json(&from_json_str(m)?)? == m`, byte for
byte, **for a schema-valid message**. A peer that sends a field the schema does not define
in that position loses it, which is the interoperable answer.
[More on the wire format](https://hupe1980.github.io/eebus/docs/wire-format/).

## What is implemented

| Layer | | Specification |
|---|---|---|
| **Wire format** | EEBUS JSON codec; `ScaledNumber`, ISO 8601 durations | SHIP §11.4, SPINE IG §3.2 |
| **Model** | 830 SPINE types, 47 SHIP messages, generated | SPINE 1.3.0, SHIP 1.1.0 |
| **SHIP** | framing; the five-phase handshake with PIN penalties and the close handshake | §10.3, §13.4.3–13.4.8 |
| | the hello prolongation dance, driven by the side that is `pending` | §13.4.4.1.3 |
| | double-connection resolution, both halves | §12.2.3 |
| | certificate updates, end to end | §12.1.3 |
| | SKI, SHIP ID, `_ship._tcp` TXT record, installation QR code | §5.4, installation requirements 1.1.0 |
| | the Pairing Service end to end: both roles, the TXT record, the fingerprint trust, the §4.3 policy | Pairing Service 1.0.0 |
| **SPINE** | device model, NodeManagement, detailed and use-case discovery | §5.1, §7.1–7.3 |
| | Restricted Function Exchange: partial read and write, filtered delete, several filters per command — sent as well as served | §5.3.4, SPINE IG §3.3, LPC UC TS §3.4.1.4 |
| | a peer's partial notifications and replies merged into the state they update | §7.4, SPINE IG §3.2.2, §3.3 |
| | acknowledgements, error numbers, counters, `specificationVersion` | §5.2.4–5.2.5, SPINE IG §2.5 |
| | `maxResponseDelay`: honoured on what a peer announced, announced for what a feature needs | §5.2.5.3 |
| | bindings and subscriptions, single-writer policy, group lock | §5.3.5–5.3.6, LPC IG §3.5 |
| | the binding and subscription tables, served from the live relations | §7.3.2, §7.4.2 |
| | heartbeat producer and monitor; deferred writes on the merged state | LPC/LPP scenario 3, LPC IG §4.1.5 |
| **Use cases** | 14 use cases, both actors of each | see [Use cases](https://hupe1980.github.io/eebus/docs/use-cases/) |
| | LPC and LPP: state machine, both actors, the §14a record | UC TS §2.3, §3.3 + the 2026 IGs |
| | scenario 4 constraints: the nameplate for a device, the contract for a CEM | [LPC/LPP-041], [LPC/LPP-042], UC TS §2.6.4.1 |
| | MPC and MGCP: both actors, incl. the PV curtailment factor as a ceiling in watts | MPC/MGCP UC TS §3.2.2, [MGCP-011] |
| | six e-mobility use cases; OPEV and OSCEV as one machine, told apart by `limitCategory` | EVSECC/EVCC/OPEV/OSCEV/EVCEM 1.0.1, EVSOC 1.0.0 |
| | inverter, PV string and battery monitoring | MOI/MPS/MOB 1.0.0 |
| | Control of Battery: six states, twenty transitions, both control modes | COB 1.0.0 §2.4 |
| **Certification** | the 203 abstract test cases of the four HLTS as data, all driven: 189/203 | LPC/LPP/MPC/MGCP HLTS 1.0.2 |
| | runtime signals for a laboratory's debug interface | the HLTS "e.g. via debug interface" footnote |
| **Transport** | node certificates, TLS 1.2 with mutual auth | SHIP §9, §12 (`cert`, `tls`) |
| | mDNS-SD announce and browse | SHIP §5 (`mdns`) |
| | connection table, keep-alive, reconnection with spread backoff | SHIP §10 (`runtime`) |
| | persistent trust store, and the "delete all foreign keys" reset | SHIP §12.2.2 (`runtime`) |
| | interactive pairing: the pending peer reported, approved or refused in place, and bounded in number | SHIP §13.4.4.1 (`runtime`) |
| | automatic pairing: `_shippairing._tcp` requests evaluated, and announced, from the hub | Pairing Service §4, §9 (`runtime`, `mdns`) |

Where the crate deliberately departs from the reference implementations — the §12.2.3
double-connection rule, no session resumption, refusing unsupported selectors rather than
approximating them, and seven more — each departure and the sentence driving it is listed
under [Conformance](https://hupe1980.github.io/eebus/docs/conformance/). Where the
*specification* is the thing at fault — §12.2.3 can starve both peers of connections, which
`enbility`'s 2025 analysis sets out — the rule is followed and the hazard is bounded, rather
than either being ignored. What is
deliberately *not* here — JSON-UTF16, Brainpool curves, enhanced-mode routing, a
persistence layer — is listed under
[Scope and non-goals](https://hupe1980.github.io/eebus/docs/scope/).

## Runnable examples

```sh
cargo run --example grid_limit                          # against a virtual clock
cargo run --example networked --features runtime,ring   # over a real socket
```

Both play out the §14a exchange: handshake, discovery, binding, a 3 kW limit, its
acknowledgement, and the failsafe taking over when the control box goes quiet. `grid_limit`
does it without a socket; `networked` generates certificates, approves SKIs and completes
TLS 1.2 with mutual authentication over loopback first.

### The two simulators

A control box and a household appliance, on a real network — one on each side of the §14a
exchange, so either can be tested without the other's hardware on the desk.

```sh
# In one terminal: the household end. Prints its SKI and its QR payload.
cargo run --example heat_pump --features full

# In another: the grid end. Finds the household over mDNS and offers to pair with it.
cargo run --example steuerbox --features full -- --limit 4200
```

Answer `y` on each side — the terminal stands in for the button on a real device — or pass
`--trust <SKI>` to skip the question. They persist their identity and their trust store
between runs, and print the `lpc:` runtime signals a certification laboratory reads.
`--reset` is the EEBUS reset of SHIP §12.2.2: forget every peer, and the identity with it.

On a network, [`Hub`](https://hupe1980.github.io/eebus/docs/networking/) owns the sockets
and the clock. It listens, dials and browses **in the background** — a peer that is slow,
unreachable or waiting for a user holds nothing else up — routes each datagram to the peer
it names, keeps idle connections alive, resolves double connections, and redials with a
backoff spread by the peer's SKI so a building coming back from a power cut does not dial
all at once. `hub.run(handler)` is the loop, because `hub.next()` is not cancel-safe and a
write cancelled anyway is closed with `Disconnect::InterruptedWrite` rather than left to
corrupt the peer's stream.

**Pairing happens while the peer waits.** An unapproved peer completes TLS, is held in the
SHIP `hello: pending` state and reported as `TrustRequested`; `hub.approve(ski)` completes
the handshake it is waiting in and `hub.refuse(ski)` answers `hello: aborted`. The
[SHIP Pairing Service](https://hupe1980.github.io/eebus/docs/networking/#or-nobody-is-asked-at-all)
is the other path, where a control unit proves it knows a printed secret and nobody is
asked at all. There is no auto accept, and no third path.

## Building

```sh
cargo build
cargo test --workspace --features eebus/full
```

The cross-implementation suite is opt-in, because it needs Docker and pulls a Go toolchain
to build the peer:

```sh
cargo test --features interop,full --test interop -- --nocapture
```

`--workspace` includes the code generator in `xtask/`, which is what produces the committed
model. The fuzz targets are a workspace of their own and are compiled separately —
`cargo check --manifest-path fuzz/Cargo.toml --all-targets`, which CI runs on every push so
that a moved API cannot break them unnoticed.

The documentation site is [Zola](https://www.getzola.org):

```sh
zola --root site serve     # http://127.0.0.1:1111
zola --root site check     # follows every internal link
```

The fuzz targets need a nightly toolchain and `cargo-fuzz`; the corpora are seeded from the
specification's own example datagrams:

```sh
mkdir -p fuzz/corpus/spine_datagram
cp tests/fixtures/spine/*.json fuzz/corpus/spine_datagram/
cargo +nightly fuzz run spine_datagram
```

The specifications are free but not redistributable, so they are not in this repository. To
regenerate the model you need them locally — register at
[eebus.org/specifications-media](https://www.eebus.org/specifications-media/), unpack
`EEBus_SPINE_V1.3.0.zip` and `EEBus_SHIP_TS_Specification_v1.1.0_public.zip` into `specs/`,
and run:

```sh
cargo run -p xtask -- codegen    # regenerate src/model/generated and src/ship/generated
cargo run -p xtask -- fixtures   # regenerate tests/fixtures from the specification's examples
```

A third generator needs no specifications — it converts MIT-licensed captures from real
hardware into wire-format fixtures:

```sh
git clone --depth=1 https://github.com/enbility/devices /tmp/devices
cargo run -p xtask -- devices /tmp/devices   # tests/fixtures/devices
```

`specs/` is git-ignored; only generated Rust is committed. The generator formats its output,
so re-running it when nothing has changed produces no diff.

## Features

| Feature | Default | Effect |
|---|---|---|
| `std` | ✅ | Standard library. Without it the crate builds `no_std + alloc`. |
| `pairing` | ✅ | SHIP Pairing Service, both roles (`hmac`, `sha2`); with `runtime` and `mdns`, the hub's end of it too. |
| `conformance` | ✅ | The 203 abstract test cases of the four HLTS as data. Tens of kilobytes of static strings a device in the field never reads; `--no-default-features` leaves it out. |
| `cert` | — | Generating and reading node certificates (`rcgen`, `x509-parser`). |
| `tls` | — | TLS 1.2 as SHIP §9 requires it (`rustls`). |
| `runtime` | — | Sockets on Tokio: TCP, TLS, WebSocket, the SHIP handshake, the connection table. |
| `mdns` | — | `_ship._tcp` announcement and discovery, and `_shippairing._tcp` with `pairing` (`mdns-sd`). |
| `ring` | — | `rustls` and `rcgen` backed by `ring`. |
| `aws-lc-rs` | — | The same, backed by `aws-lc-rs`. |
| `interop` | — | The cross-implementation test suite. Needs Docker; nothing in the library depends on it, and `full` deliberately excludes it. |
| `full` | — | Everything above, on `ring`. |

### The cryptography provider is yours to pick

`cert`, `tls` and `runtime` need one of `ring` or `aws-lc-rs`, and this crate names neither
by default: `rustls`' provider is process-global, so a library that pulled one in would
choose for every consumer downstream of it.

```toml
eebus = { version = "0.4", features = ["runtime", "ring"] }
# or, for a build that must not contain `ring`:
eebus = { version = "0.4", default-features = false, features = ["std", "runtime", "aws-lc-rs"] }
```

Naming both, or neither, is a `compile_error!` rather than a device that panics on its first
connection. `eebus::tls::CRYPTO_PROVIDER` says which one a binary got. Nothing here reads
`rustls`' process default — the provider is built explicitly, with exactly the suites SHIP §9
permits — so a consumer that installed its own keeps it.

`--all-features` is therefore not a configuration that can exist; `--features full` is
everything, on `ring`.

Minimum supported Rust version: 1.88.

## License

MIT or Apache-2.0, at your option.

EEBUS® is a trademark of EEBus Initiative e.V. This project is not affiliated with or
endorsed by the EEBus Initiative.
