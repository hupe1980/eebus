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
