+++
title = "Regenerating the model"
description = "How the 830 SPINE types and 47 SHIP messages are generated from the specifications' XML Schemas, and how to regenerate them yourself."
weight = 150
[extra]
group = "Project"
+++

The model is generated, not transcribed. `cargo xtask codegen` reads the XML Schemas the
SPINE and SHIP specifications ship and emits 830 SPINE types and 47 SHIP messages,
together with the table that links each data type to its Restricted Function Exchange
selectors and elements filters — 141 functions' worth of a relationship the schemas leave
as a naming convention.

**The generated Rust is committed.** Building the crate never needs the specifications, and
`cargo test` does not depend on a download.

## Two things the schemas do not contain

The XML Schemas describe *shapes*. Two relationships the protocol depends on are stated only
in prose, and the generator resolves both into tables.

**Which selectors and elements filter address which function.** Every function that supports
a partial exchange has three related types — the data, its selectors, its elements filter —
and nothing links them but a naming convention. The table covers 141 functions and is what
`possibleOperations` is derived from, so the crate announces a partial read exactly where it
can serve one.

**Which elements identify a list entry.** Merging "entry by entry" needs to know which
elements *are* the entry, and a wrong answer is silent in both directions: too wide makes
every change look like a new entry, too narrow refuses a conformant message. So the
generator uses the Resource Specification's own answer — **Annex B.7, Table 358**, "all
identifiers that belong to the identifiers concept" — with §3.4.2.1's distinction between an
identifier that *addresses* an entry (PRIMARY, SUB) and one that refers to another feature
and "is **not** used to create further dimensions of list entries" (FOREIGN).

`tests/list_identifiers.rs` writes the resulting answers down, each with the specification
table that governs it.

## Getting the specifications

They are free but **not redistributable**, so they are not in the repository.

1. Register at [eebus.org/specifications-media](https://www.eebus.org/specifications-media/)
   — free, no membership required.
2. Download `EEBus_SPINE_V1.3.0.zip` and
   `EEBus_SHIP_TS_Specification_v1.1.0_public.zip`, and unpack both into `specs/`.

## Running the generators

```sh
cargo run -p xtask -- codegen    # regenerate src/model/generated and src/ship/generated
cargo run -p xtask -- fixtures   # regenerate tests/fixtures from the specification's examples
```

A third generator needs no specifications at all:

```sh
git clone --depth=1 https://github.com/enbility/devices /tmp/devices
cargo run -p xtask -- devices /tmp/devices   # tests/fixtures/devices
```

That corpus is captures from real hardware, MIT-licensed and stored as ordinary JSON, so
the converter projects it into the JSON-UTF8 of SHIP §11.4. Re-running it when the corpus
has not changed produces no diff.

`specs/` is git-ignored; only generated Rust is committed. The generator formats its output
through rustfmt, so re-running it when nothing has changed produces no diff — which makes
"is the checked-in model current?" a question CI can answer.

## Fuzzing

The fuzz targets are a workspace of their own, so `libfuzzer-sys` does not end up in an
ordinary build. They need a nightly toolchain and `cargo-fuzz`, and the corpora are seeded
from the specification's own example datagrams:

```sh
mkdir -p fuzz/corpus/spine_datagram
cp tests/fixtures/spine/*.json fuzz/corpus/spine_datagram/
cargo +nightly fuzz run spine_datagram
```
