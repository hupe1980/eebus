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
initiator's connection instead.

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
appended as a new entry, which is what the reference implementations do.

**The binding and subscription tables are served from the live relations**, so a peer reading
`nodeManagementBindingData` sees the bindings that are actually held (§7.3.2, §7.4.2).

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

## How the crate is checked

| | |
|---|---|
| Unit and integration tests | `cargo test --workspace --all-features` |
| Both examples, end to end | `cargo run --example grid_limit`, `--example networked` |
| Property tests | round trips, the RFE merge laws and the wire arithmetic, via `proptest` |
| Fuzzing | five `cargo fuzz` targets, run nightly; their compilation checked on every push |
| Lints | `cargo clippy --workspace --all-targets --all-features` with `-D warnings` |
| Documentation | `cargo doc` with `RUSTDOCFLAGS=-D warnings` |
| Documented counts | the generated model's size, held to the number the docs quote |
| Minimum Rust version | `cargo +1.88.0 check` |
| Embedded | `cargo build --no-default-features --target thumbv7em-none-eabihf` |

`--workspace` covers the code generator in `xtask/`; the fuzz targets are a workspace of
their own and are compiled separately.

All of it runs in CI on every push.
