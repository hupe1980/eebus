+++
title = "MPC and MGCP"
description = "Monitoring of Power Consumption and of the Grid Connection Point: reading measurements, resolving what they mean, and the PV curtailment factor."
weight = 90
[extra]
group = "Use cases"
+++

**Monitoring of Power Consumption** reports what an appliance is drawing. **Monitoring of
the Grid Connection Point** reports the same quantities at the building's boundary with the
grid. They are the same measurements named from two sides, and they share their
implementation here.

## A measurement without its description means nothing

This is the whole difficulty of the use case. A notification looks like this:

```json
{"measurementId": 3, "value": {"number": 2300}}
```

Which is 2300 of *what*, on *which phase*, *scaled by* how much? None of that is in the
message. It is in two other functions — the measurement descriptions and the electrical
connection descriptions — which a Monitoring Appliance reads once at commissioning and
keeps.

So the appliance reads them, subscribes, and resolves everything afterwards:

```rust
use eebus::model::ElectricalConnectionPhaseName as Phase;

readings.total_power();                                        // Some(2300.0) watts
readings.value(&Measurand::on(Quantity::Current, Phase::A));   // Some(3.5) amperes
```

Two rules the crate enforces rather than leaving to the caller:

* A value whose description never arrived is **dropped**, not guessed at.
* A value flagged `error` is **not** handed back as a number — [MPC-003] says its content
  is to be ignored.

## Curtailment

MGCP carries the photovoltaic curtailment factor: how much of what the plant could produce
it is currently allowed to. It is the measurement that closes the loop with
[LPP](@/docs/limitation.md) — a limit was sent, and this is what actually happened.
