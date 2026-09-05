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

**Byte for byte for a schema-valid message.** The model is generated from the XSDs, so a
field a peer sends where the schema defines none has nowhere to go and is dropped. That is
not hypothetical: SPINE's detailed discovery uses a *restricted* address type carrying only
the entity path, and `eebus-go` puts a `device` in it anyway. Dropping it is the
interoperable answer — the enclosing `deviceInformation` already names the device — and the
device captures in the test suite hold the crate to exactly that one exception.

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
* **A negative scale divides; it does not multiply.** No negative power of ten is
  representable in binary, so `number × 10⁻⁴` rounds twice — once building the constant,
  once in the product — and `12345 × 10⁻⁴` comes out as `1.2345000000000002`. Dividing by
  the exact `1e4` rounds once. The two spellings of a quantity have to agree, or a limit
  changes when a peer picks a different scale for it.
* **And a scale past `10²²` is still that scale.** Only the first twenty-three powers of ten
  are `f64` values, so a bigger scale is applied *in steps* of the largest exact one rather
  than through a magnitude built up first — which rounds once per step instead of once per
  decade, and gets `10²³` exactly right. `scale` is a signed 16-bit integer, so a peer
  reaches that seam in one schema-valid message.
* **A value too large for an `i64` raises the scale.** `1e30 as i64` is `i64::MAX`, a
  different number by a factor of nine; `ScaledNumber::from_f64` answers `number = 1,
  scale = 30` instead, which is what the scale is for.

## Times

`AbsoluteOrRelativeTimeType` is defined as `xs:union memberTypes="xs:duration xs:dateTime"`,
so one element carries either a span or an instant and nothing in the element says which.
`AbsoluteOrRelativeTime::parse` returns whichever arrived; `is_absolute()` says which form
the peer *meant*, which stays answerable even when the value cannot be read.

**Durations** — `PT1M`, `PT2H`, `P1DT30M` — are parsed and emitted without a calendar
library, since only the fixed-length part of the grammar can occur in SPINE.

Months and years are refused rather than approximated: LPC's failsafe duration is a
safety-relevant timer, and guessing at 30-day months would make it wrong. So are durations
with no components at all (`P`, `PT`). A duration pointing backwards reads as zero, not as
its magnitude — `Duration` is unsigned, and a span that has elapsed has nothing left to run.

On a measurement a duration is an **age**: the value was taken that long before the message
carrying it, so the instant is `arrived − duration`. That subtraction is
`Reading::taken_at_relative_to`, against the same monotonic clock the engine is given.

**Timestamps** are `xs:dateTime`, which is not RFC 3339 and where reaching for an RFC 3339
parser is a bug that looks like a peer sending nothing. Three differences carry meaning:

* **The offset is optional.** `2026-09-05T08:15:00` is well formed and names a wall-clock
  reading whose zone the protocol never states, so it fixes no instant at all. `DateTime`
  keeps that distinction: `unix_seconds()` is `None`, and `unix_seconds_at(offset)` makes
  the caller name the zone it means to assume rather than defaulting to UTC and landing an
  hour or two out of place.
* **`24:00:00` is legal**, and denotes midnight ending that day (XML Schema Part 2 §3.2.7).
  It is normalised to `00:00:00` of the next.
* **The year may be negative and may exceed four digits.** Neither reaches a household
  message; both are cheap to accept and expensive to refuse.

Everything that carries meaning is refused instead: a thirteenth month, a 29 February in a
common year, a sixtieth second, an offset past ±14:00, a `+0200` without its colon. A
timestamp that is wrong is worse than one that is absent — the second is visibly missing and
the first is silently believed.

What the crate does **not** decide is the policy. A household device sets its clock from NTP
or from nothing, and how far a peer's clock may be wrong before its timestamps are worth
less than the arrival time is not a decision a protocol library can make for a product.

### Where a use case is narrower than the schema, the use case wins

The union is what the *schema* permits, and it is regularly wider than what a given element
may actually carry. Two cases here, and both are places an implementer reading only the
schema goes wrong in good faith:

* **LPC, LPP and COB fix their durations as relative.** §3.1.8.2 of each: "Durations used
  within this Use Case SHALL be presented as relative times. The same holds for the
  `endTime` Element used for the duration of validity." So a limit's `endTime` is a span,
  and one that cannot be read as a span makes the write unusable — reading it as *absent*
  would mean "no expiry", which is the wrong way for the mistake to fall.
* **`SmartEnergyManagementPs` restricts `schedule.startTime` to `xs:duration`** by
  `xs:restriction` on the `PowerSequences` type that uses the union. So a heat-pump
  compressor's start time is "in this long", never "at this instant", and `ohpcf::activate`
  takes a `Duration` so the wider spelling cannot be built.

Both are enforced rather than documented: the value is refused, and the refusal reaches the
peer as a NACK.
