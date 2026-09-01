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

This machinery is not only MPC and MGCP. The same "describe a measurement twice and read it
back" serves seven use cases: those two, [EVCEM and EVSOC](@/docs/e-mobility.md) for a car,
and [MOI, MPS and MOB](@/docs/storage.md) for an inverter, a PV string and a battery. Each
added vocabulary — charged energy, state of charge, DC power, yields, insulation
resistance — and none of them added mechanism.

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

MGCP scenario 1 carries the photovoltaic curtailment factor: how much of what the plant
could produce it is currently allowed to. It is the measurement that closes the loop with
[LPP](@/docs/limitation.md) — a limit was sent, and this is what actually happened.

[MGCP-011] states it as an equation rather than a value:

```text
P_PV,feed-in  ≤  PLF_PV,feed-in,max,pct  ×  Σ P_PV,AC,nom
```

The factor is what crosses the wire. The sum of the installed systems' nominal peak power
is a property of the building that no EEBUS message carries, so only the two together are a
number of watts — and it is the watts an energy manager acts on. In Germany that number is
the export ceiling of EEG §9.

`FeedInLimit` keeps both terms, which is what stops a caller acting on a percentage as
though it were a power:

```rust
use eebus::usecases::mgcp::FeedInLimit;

// 70 % of a 12 kWp array: the classic §9 configuration.
let limit = FeedInLimit::new(70.0, 12_000.0);
assert_eq!(limit.watts(), 8_400.0);
assert!(!limit.permits(9_000.0));

// And the value LPP is written with, which is how an inverter hears about it.
let write = limit.as_production_limit();
```

`FeedInLimit::from_data` reads the factor straight off a `deviceConfigurationKeyValueListData`
and returns `None` when the payload carries none — which is not the same as a factor of
zero, and must not be treated as one. An unread message is not a curtailment.
