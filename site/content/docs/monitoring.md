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
back" serves eleven use cases: those two, [EVCEM and EVSOC](@/docs/e-mobility.md) for a car,
[MOI, MPS, MOB](@/docs/storage.md) and COB for an inverter, a PV string and a battery, and
[MDT, MRT and MOT](@/docs/hot-water.md) for the hot water, a room and the outdoors. Each
adds vocabulary — charged energy, state of charge, DC power, yields, insulation resistance,
three different temperatures — and none adds mechanism.

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
* A value flagged `error` **or** `outOfRange` is not handed back as a number. §2.5.2 lists
  the three states together and says the same of both abnormal ones: they "SHALL be ignored
  by the Monitoring Appliance" ([MPC-003]). Reading `outOfRange` as "high but real" is
  tempting — a meter reporting 14 kW against an 11 kW constraint looks like a number worth
  having — and the specification does not permit it. `Reading::usable()` returns `None` for
  both; the raw `value` field is still there for a display, which is a different thing from
  acting on it.

### When the reading was taken, not when it arrived

[MPC-002], [MDT-002], [MRT-002] and [MOT-002] all permit a `timestamp` on the value and
forbid the history that would otherwise justify one. `Reading::timestamp` carries it
verbatim, and `Readings::read_at` hands it back beside the value:

```rust
let (degrees, taken_at) = readings.read_at(&mrt::MEASURAND)?;
```

The obvious substitute is wrong. An appliance subscribes rather than polls, so a
notification arrives when something **changes** — and a room holding its temperature sends
nothing at all. Age the readings by arrival and you discard the ones most likely to still be
true, and cannot tell that room from a sensor that died an hour ago.

Most peers send none, so `taken_at` is usually `None`; what that means is the application's
call. A Monitored Unit on this side stamps with `MonitoredUnit::set_at`, or
`hvac::mrt::temperature_at` and its two siblings. `set` sends no timestamp: the engine's
clock is monotonic uptime, not a time of day.

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

### The factor's `keyId` is the connection point's, not yours

MGCP Table 23 spells the identifier `<k1#(1..1)>`: the device picks the number and fixes
the **name**, `pvCurtailmentLimitFactor`. A `DeviceConfiguration` feature carries every
configuration key the device has, so key `1` on a real connection point is as likely to be
its installed peak power in watts — a number that reads as a perfectly plausible percentage
and becomes a wrong export ceiling with nothing on the wire to say so.

`Curtailment` is the reader: hand it both functions, in whatever order they arrive, and it
holds the description beside the value.

```rust
use eebus::usecases::mgcp::{self, Curtailment};

let mut factor = Curtailment::new();
factor.describe(&mgcp::curtailment_description());
assert_eq!(factor.apply(&mgcp::curtailment_value(70.0)), Some(70.0));
assert_eq!(factor.limit(12_000.0).map(|l| l.watts()), Some(8_400.0));
```

`factor_percent` is `None` until both halves are in, and `None` is not zero — an unread
message is not a curtailment, and treating it as one stops a roof exporting.

### Or let the actor do it

`MonitoringApplianceActor` serves the whole use case. `monitoring::locate` finds the
connection point's `DeviceConfiguration` feature next to its measurements, `attach` reads
the description and subscribes, and the factor arrives as an event:

```rust,ignore
match appliance.handle_event(&event) {
    Some(MonitoringEvent::CurtailmentChanged { unit, factor_percent }) => {
        let ceiling = appliance.feed_in_limit(&unit, 12_000.0);
    }
    _ => {}
}
```

`unit` is a `UnitId` — a device **and** an entity — because one device is regularly several
units: a heat-pump gateway announces one `HVACRoom` per room, each with its own
`Measurement` feature. `attach` takes one peer per entity, `MonitoredUnitPeer::id()`
produces the key, and `readings`, `curtailment`, `feed_in_limit` and `detach` all take one.

There is no `mgcp::curtailment_client_feature` to go with `curtailment_feature`, and the
asymmetry is the specification's: LPC IG §3.3 asks each actor to hold **one** client feature
with featureType `Generic` for all of its client functionality. A Monitoring Appliance reads
a `Measurement` server for scenarios 2 to 7 and a `DeviceConfiguration` server for scenario 1
from the same one — `limitation::client_feature`, the same constructor an Energy Guard uses.

## Every feature a peer might not have

MGCP §3.2.2.2.1 ties each feature type to the scenarios that use it, and then says the
presence indications are "meant relative to the ones of the according Scenario stated in
Table 1". `DeviceConfiguration` belongs to scenario 1; `Measurement` and
`ElectricalConnection` belong to scenarios 2 to 7. So which features a peer serves depends
on which scenarios it implements, and `MonitoredUnitPeer` carries all three as `Option`:

```rust
let peer = monitoring::locate(remote, mgcp::NAME, actors::GRID_CONNECTION_POINT)?;
peer.measurement;            // Option: absent on a scenario-1-only connection point
peer.electrical_connection;  // Option: absent wherever there are no phases to describe
peer.curtailment;            // Option: MGCP scenario 1, and absent from MPC entirely
```

`attach` reads only what is there. What `locate` *does* require is that the peer serve at
least one of them: a peer announcing the use case and none of its features would otherwise
sit in the actor's list for the life of the connection, waiting for notifications that
cannot come.

That is also what lets a hot water tank in through the same door. A DHW circuit has no
`ElectricalConnection` at all — MDT Table 6 gives the use case one feature — so it has a
`locate` of its own, and what it returns goes into the same actor:

```rust
use eebus::usecases::hvac::mdt;

let tank = mdt::locate(remote)?;          // measurement only; no phases to describe
appliance.attach(&mut engine, tank, now); // the same actor as a grid connection point
```

So do the two temperatures a building is planned against — `hvac::mrt` for a room and
`hvac::mot` for outdoors — which are the same shape again. A room comes in the plural:

```rust
use eebus::usecases::hvac::mrt;

for room in mrt::locate_all(remote) {     // one HVACRoom entity per room
    appliance.attach(&mut engine, room, now);
}
```
