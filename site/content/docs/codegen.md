+++
title = "Regenerating the model"
description = "How the 830 SPINE types and 47 SHIP messages are generated from the specifications' XML Schemas, and how to regenerate them yourself."
weight = 150
[extra]
group = "Project"
+++

`cargo xtask codegen` reads the XML Schemas the
SPINE and SHIP specifications ship and emits 830 SPINE types and 47 SHIP messages,
together with the table that links each data type to its Restricted Function Exchange
selectors and elements filters — 141 functions' worth of a relationship the schemas leave
as a naming convention.

**The generated Rust is committed.** Neither building nor `cargo test` needs the
specifications.

## Two things the schemas do not contain

The XML Schemas describe *shapes*. Two relationships the protocol depends on are stated only
in prose; the generator resolves both into tables.

**Which selectors and elements filter address which function.** Every function that supports
a partial exchange has three related types — the data, its selectors, its elements filter —
linked only by a naming convention. The table covers 141 functions; `possibleOperations` is
derived from it, so the crate announces a partial read exactly where it can serve one.

**Which elements identify a list entry.** Merging entry by entry needs to know which
elements *are* the entry: too wide makes every change a new entry, too narrow refuses a
conformant message. The generator uses the Resource Specification's answer — **Annex B.7, Table 358**, "all
identifiers that belong to the identifiers concept" — with §3.4.2.1's distinction between an
identifier that *addresses* an entry (PRIMARY, SUB) and one that refers to another feature
and "is **not** used to create further dimensions of list entries" (FOREIGN).

`tests/list_identifiers.rs` records the results, each with the specification table that
governs it.

## Getting the specifications

They are free but **not redistributable**, so the repository does not contain them.

1. Register at [eebus.org/specifications-media](https://www.eebus.org/specifications-media/)
   — free, no membership required.
2. Download `EEBus_SPINE_V1.3.0.zip` and
   `EEBus_SHIP_TS_Specification_v1.1.0_public.zip`.
3. Unpack the SPINE bundle into `<dir>/spine/` and the SHIP bundle into `<dir>/ship/` of
   a directory outside the repository, and point the generators at it with
   `EEBUS_REFERENCE=<dir>`.

## Running the generators

```sh
export EEBUS_REFERENCE=<dir>
cargo run -p xtask -- codegen    # regenerate src/model/generated and src/ship/generated
cargo run -p xtask -- fixtures   # regenerate tests/fixtures from the specification's examples
cargo run -p xtask -- check-schemas  # are these the schemas the committed model came from?
```

Beside each generated model, `codegen` writes `schemas.sha256`: every schema it read and
its SHA-256 (line endings normalised), in `sha256sum` format. `check-schemas` compares the
schemas under `EEBUS_REFERENCE` against those records and names every file that is
changed, missing or unexpected. Without the specifications it says so and succeeds.

A third generator, `devices`, needs no specifications at all:

```sh
git clone --depth=1 https://github.com/enbility/devices /tmp/devices
cargo run -p xtask -- devices /tmp/devices   # tests/fixtures/devices
```

The corpus is MIT-licensed captures from real hardware, stored as ordinary JSON; the
converter projects it into the JSON-UTF8 of SHIP §11.4.

`codegen` formats its output through rustfmt, so re-running any of the three generators on
unchanged input produces no diff: with the specifications present, a regeneration and
`git diff` show whether the committed model is current.

## Fuzzing

The fuzz targets are a separate workspace, so `libfuzzer-sys` stays out of an ordinary
build. They need nightly and `cargo-fuzz`; the corpora are seeded from the specification's
example datagrams:

```sh
mkdir -p fuzz/corpus/spine_datagram
cp tests/fixtures/spine/*.json fuzz/corpus/spine_datagram/
cargo +nightly fuzz run spine_datagram
```
