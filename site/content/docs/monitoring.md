+++
title = "MPC and MGCP"
description = "Monitoring of Power Consumption and of the Grid Connection Point: reading measurements, resolving what they mean, and the PV curtailment factor."
weight = 90
[extra]
group = "Use cases"
+++

**Monitoring of Power Consumption** reports what an appliance is drawing. **Monitoring of
the Grid Connection Point** reports the same quantities at the building's boundary with the
grid. They share one implementation.

The same machinery serves eleven use cases: MPC and MGCP,
[EVCEM and EVSOC](@/docs/e-mobility.md) for a car,
[MOI, MPS, MOB](@/docs/storage.md) and COB for an inverter, a PV string and a battery, and
[MDT, MRT and MOT](@/docs/hot-water.md) for the hot water, a room and the outdoors. Each
adds vocabulary (charged energy, state of charge, DC power, yields, insulation resistance,
temperatures), not mechanism.

## A measurement without its description means nothing

A notification looks like this:

```json
{"measurementId": 3, "value": {"number": 2300}}
```

Quantity, phase and scale are not in the message. They are in the measurement
descriptions and the electrical connection descriptions, which a Monitoring Appliance reads
once, keeps, and resolves every value against:

```rust
use eebus::model::ElectricalConnectionPhaseName as Phase;

readings.total_power();                                        // Some(2300.0) watts
readings.value(&Measurand::on(Quantity::Current, Phase::A));   // Some(3.5) amperes
```

Two rules the crate enforces:

* A value whose description never arrived is **dropped**, not guessed at.
* A value flagged `error` **or** `outOfRange` is not handed back as a number: §2.5.2 says
  both "SHALL be ignored by the Monitoring Appliance" ([MPC-003]). `Reading::usable()`
  returns `None` for both; the raw `value` field is still there for a display.

### When the reading was taken, not when it arrived

[MPC-002], [MDT-002], [MRT-002] and [MOT-002] all permit a `timestamp` on the value and
forbid the history that would otherwise justify one. `Reading::timestamp` carries it
verbatim, and `Readings::read_at` hands it back beside the value:

```rust
let (degrees, taken_at) = readings.read_at(&mrt::MEASURAND)?;
```

Arrival time is no substitute: a notification arrives when something **changes**, so a room
holding its temperature sends nothing.

Most peers send no timestamp, so `taken_at` is usually `None`; how to treat that is up to
the application. A Monitored Unit on this side stamps with `MonitoredUnit::set_at`, or
`hvac::mrt::temperature_at` and its two siblings. `set` sends no timestamp: the engine's
clock is monotonic uptime, not a time of day.

### One element, two things

`timestamp` is `AbsoluteOrRelativeTimeType`: an instant or a **span**, and nothing in the
element says which. `taken_at()?.parse()` reads whichever arrived:

```rust
use eebus::model::TimeValue;

let taken_at = match reading.taken_at()?.parse()? {
    // An instant on the peer's own calendar.
    TimeValue::Absolute(stamp) => stamp.unix_seconds(),
    // An age: the unit measured this long before it sent the message.
    TimeValue::Relative(_) => reading.taken_at_relative_to(arrived).map(|d| d.as_secs() as i64),
};
```

A relative timestamp is an age, so `Reading::taken_at_relative_to` subtracts it from the
arrival time, saturating at zero. An absolute one is `xs:dateTime`, not RFC 3339, and
`unix_seconds()` is `None` when it carries no offset; `as_timestamp()` says whether it could
be read at all. How far to trust a peer's clock is the application's policy. See
[Times](@/docs/wire-format.md#times).

### What silence means

A value that has not arrived for a while comes from a peer that stopped answering or from
one with nothing to say. The descriptors say which; see
[Who has to subscribe, and what silence means](@/docs/use-cases.md#who-has-to-subscribe-and-what-silence-means).

## Curtailment

MGCP scenario 1 carries the photovoltaic curtailment factor: how much of what the plant
could produce it is currently allowed to feed in. It closes the loop with
[LPP](@/docs/limitation.md): what actually happened after a limit was sent.

[MGCP-011] states it as an equation rather than a value:

```text
P_PV,feed-in  ≤  PLF_PV,feed-in,max,pct  ×  Σ P_PV,AC,nom
```

Only the factor crosses the wire. The sum of the installed nominal peak power is a property
of the building that no EEBUS message carries; together they give the watts an energy
manager acts on — in Germany, the export ceiling of EEG §9. `FeedInLimit` keeps both terms:

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
configuration key the device has, so key `1` may be something else entirely, such as its
installed peak power.

`Curtailment` is the reader: hand it both functions, in either order, and it holds the
description beside the value.

```rust
use eebus::usecases::mgcp::{self, Curtailment};

let mut factor = Curtailment::new();
factor.describe(&mgcp::curtailment_description());
assert_eq!(factor.apply(&mgcp::curtailment_value(70.0)), Some(70.0));
assert_eq!(factor.limit(12_000.0).map(|l| l.watts()), Some(8_400.0));
```

`factor_percent` is `None` until both halves are in; `None` is not zero.

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

`unit` is a `UnitId` — a device **and** an entity — because one device is often several
units: a heat-pump gateway announces one `HVACRoom` per room, each with its own
`Measurement` feature. `attach` takes one peer per entity, `MonitoredUnitPeer::id()`
produces the key, and `readings`, `curtailment`, `feed_in_limit` and `detach` all take one.

There is no client counterpart to `mgcp::curtailment_feature`: LPC IG §3.3 asks each actor
to hold **one** client feature with featureType `Generic` for all of its client
functionality. A Monitoring Appliance reads the `Measurement` server (scenarios 2 to 7) and
the `DeviceConfiguration` server (scenario 1) from `limitation::client_feature`, the same
constructor an Energy Guard uses.

## Every feature a peer might not have

MGCP §3.2.2.2.1 ties each feature type to the scenarios that use it, and then says the
presence indications are "meant relative to the ones of the according Scenario stated in
Table 1". `DeviceConfiguration` belongs to scenario 1; `Measurement` and
`ElectricalConnection` belong to scenarios 2 to 7, so `MonitoredUnitPeer` carries all three
as `Option`:

```rust
let peer = monitoring::locate(remote, mgcp::NAME, actors::GRID_CONNECTION_POINT)?;
peer.measurement;            // Option: absent on a scenario-1-only connection point
peer.electrical_connection;  // Option: absent wherever there are no phases to describe
peer.curtailment;            // Option: MGCP scenario 1, and absent from MPC entirely
```

`attach` reads only what is there; `locate` requires the peer to serve at least one of
them.

A DHW circuit has no `ElectricalConnection` (MDT Table 6 gives the use case one feature), so
it has a `locate` of its own, and what it returns goes into the same actor:

```rust
use eebus::usecases::hvac::mdt;

let tank = mdt::locate(remote)?;          // measurement only; no phases to describe
appliance.attach(&mut engine, tank, now); // the same actor as a grid connection point
```

So do `hvac::mrt` for a room and `hvac::mot` for outdoors. Rooms come in the plural:

```rust
use eebus::usecases::hvac::mrt;

for room in mrt::locate_all(remote) {     // one HVACRoom entity per room
    appliance.attach(&mut engine, room, now);
}
```
