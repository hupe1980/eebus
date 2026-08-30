+++
title = "The wire format"
description = "EEBUS JSON-UTF8 explained: why every sequence is an array of single-key objects, and how the codec encodes it directly through serde in one pass."
weight = 40
[extra]
group = "Foundations"
+++

SPINE is specified in XML Schema, but on the wire over SHIP it is JSON. The projection
between them (SHIP §11.4) is unusual enough to be worth understanding before anything else
makes sense.

## Arrays of single-key objects

XML sequences are ordered and may repeat an element name; JSON objects are neither. Rather
than lose that, EEBUS represents **every sequence as an array of single-key objects**:

```json
{"datagram":[
  {"header":[{"specificationVersion":"1.3.0"},{"msgCounter":7},{"cmdClassifier":"notify"}]},
  {"payload":[{"cmd":[[
    {"function":"deviceDiagnosisHeartbeatData"},
    {"deviceDiagnosisHeartbeatData":[{"heartbeatCounter":12},{"heartbeatTimeout":"PT1M"}]}
  ]]}]}
]}
```

Note the double bracket after `"cmd"`: `cmd` is a list, and each command is itself a
sequence, so an array of arrays.

## Encoded directly

Most implementations serialise to ordinary JSON and rewrite the tree afterwards. This one
encodes the format directly through `serde` in a single streaming pass — no intermediate
`Value`, no allocation for field names. Round trips are byte for byte, which matters
because the fixtures are the specification's own example datagrams.

```rust
use eebus::model::{from_json_str, to_json, CmdData, Function};

let datagram = from_json_str(message)?;
let cmd = &datagram.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];

if let Some(CmdData::DeviceDiagnosisHeartbeatData(hb)) = &cmd.data {
    println!("heartbeat #{:?}", hb.heartbeat_counter);
}

assert_eq!(to_json(&datagram)?, message);   // byte for byte
```

## Numbers that are not floats

SPINE carries physical quantities as a `ScaledNumber`: an integer `number` and a power-of-ten
`scale`. 4200 W is `{"number": 42, "scale": 2}`. There is no floating point on the wire,
which is deliberate — a limit is a legal quantity and must round-trip exactly.

Three rules follow, and all three are easy to get wrong:

* **`scale` is always sent.** The SPINE implementation guide §3.2.1 erratum says so, and
  a peer that assumes a missing scale means zero will be wrong by orders of magnitude.
* **A partial update merges into the stored value.** Send a `scaledNumber` carrying only
  `number` and the stored `scale` must survive. Replace instead of merge and a 4.2 kW limit
  becomes 42 MW. See [Restricted Function Exchange](@/docs/spine.md#restricted-function-exchange).
* **A value that overflows `f64` is not a value.** `scale` is signed 16-bit, so a `number`
  near `i64::MAX` reaches infinity in one well-formed message. `to_f64` answers `None`, and
  a use case handed a present-but-unreadable value NACKs the write rather than substituting
  a number the peer never sent.

## Durations

Times are ISO 8601 durations — `PT1M`, `PT2H`, `P1DT30M` — parsed and emitted without a
calendar library, since only the fixed-length part of the grammar can occur in SPINE.

Months and years are refused rather than approximated: LPC's failsafe duration is a
safety-relevant timer, and guessing at 30-day months would make it wrong. So are durations
with no components at all (`P`, `PT`). A duration pointing backwards reads as zero, not as
its magnitude — `Duration` is unsigned, and a span that has elapsed has nothing left to run.
