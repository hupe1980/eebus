+++
title = "E-mobility"
description = "The six e-mobility use cases: who is on the end of the cable, what it may take, and what it actually did — EVSECC, EVCC, OPEV, OSCEV, EVCEM and EVSOC."
weight = 100
[extra]
group = "Use cases"
+++

The same machinery runs the e-mobility family: a wallbox, the car plugged into it, and the
energy manager that has to keep the house fuse intact.

A car is the one appliance in a building that arrives, says what it is, and leaves again.
Half of this family is therefore about finding out what is on the end of the cable before
anything can be asked of it, and the six use cases layer:

| | Use case | What it answers |
|---|---|---|
| **Who is there** | EVSECC | what the wallbox is |
| | EVCC | what the *car* is, and what it can be asked |
| **What it may take** | OPEV | the ceiling the fuse imposes — an *obligation* |
| | OSCEV | the surplus the roof is producing — a *recommendation* |
| **What it did** | EVCEM | the current, power and energy that really flowed |
| | EVSOC | how full the battery is, and how big |

## EVSECC — commissioning the wallbox

**EVSE Commissioning and Configuration** is how a wallbox introduces itself to an energy
manager — manufacturer, model, firmware — and how it reports being broken. Everything else
in the family assumes this has happened.

## EVCC — commissioning the car

**EV Commissioning and Configuration** is the same question about the thing on the end of
the cable, in eight scenarios. Two of them have no message at all: scenario 1 is an `EV`
entity *appearing* underneath the `EVSE` entity, and scenario 8 is it going away again.
Detailed discovery carries both.

Scenario 2 is the one that decides what the rest of the family can do:

```rust
use eebus::usecases::emobility::evcc::{CommunicationStandard, EvProfile};

let car = EvProfile::new()
    .communication_standard(CommunicationStandard::Iso15118Ed2)
    .asymmetric_charging(true)
    .charging_power(1_400.0, 11_000.0);

car.supports_data_exchange();   // true — so EVSOC can be asked
```

A car on **IEC 61851** has a pilot wire and nothing else. It can be offered a current and
it cannot be asked anything: no state of charge, no charging plan, however willing the
energy manager is. A car on **ISO 15118** has a data link. Reading that one field before
sending the first request is the difference between a working integration and a queue of
reads nothing will ever answer.

Two more fields decide real behaviour. `asymmetric_charging` is what OPEV needs before it
writes three different currents. And the charging power band's *minimum* is the number that
matters more than the maximum: below it a car does not charge slowly, it stops — so an
energy manager that offers less than the minimum has switched the charge off rather than
slowed it down.

## OPEV — keeping the fuse intact

**Overload Protection by EV Charging Current Curtailment** is LPC's counterpart on the
other side of the house, and the differences between them are exactly why it is a separate
use case rather than a parameter:

| | LPC | OPEV |
|---|---|---|
| What is limited | total active power, in watts | charging current, **per phase**, in amperes |
| Heartbeat timeout | 120 s | **4 s** |
| On losing the peer | the failsafe power limit, for hours | a safe current, immediately |

**Four seconds**, because a car's current is set by a pilot signal it follows at once, and
the fuse it protects does not wait — where a heat pump's compressor cannot be asked for a
change in less than minutes.

**Per phase**, because a car that can charge asymmetrically takes 16 A on the phase with
room and 6 A on the one already loaded. The specification's own example is 690 W against
460 W.

```rust
use eebus::model::ElectricalConnectionPhaseName as Phase;

guard.require(&car, ChargingCurrents::new(16.0, 10.0, 6.0));   // asymmetric [OPEV-002]

// …and on the car:
ev.charging().effective(Phase::A);   // Some(16.0) while the manager is heard from
ev.charging().source();              // Curtailed | GuardSilent | GuardFailed
```

A manager that goes quiet and a manager that announces a failure mean the same thing to the
car: fall back to a current that cannot overload anything, now.

### Which `limitId` is which phase is the car's to say

Table 6 spells the three identifiers `<x1>`, `<x2>`, `<x3>` — placeholders. Nothing on the
wire ties a `limitId` to a phase directly: the limit description points at a
`measurementId`, and the *parameter* descriptions give that `measurementId` its phase. A
manager has to read both and compose them, which is what `PhaseLimits` does and what
`OverloadGuardActor::attach` now waits for before it writes anything.

Getting it wrong is quiet. Curtailing "phase A" by writing to `limitId` 1 on a car that
numbers them the other way limits a phase the supply was not worried about, leaves the one
that was at full current, and is acknowledged — which is the fuse this use case exists to
protect. A car that serves OPEV *and* OSCEV makes it worse: the same feature then carries
two limits per phase, an `obligation` and a `recommendation`, so `PhaseLimits` matches on
the category and scope too. Writing the fuse's ceiling into the recommendation tells the car
it may ignore it.

## Saying "no curtailment needed"

An empty limit list says nothing — it cannot be distinguished from a message that lost its
contents. When no curtailment is required the guard sends limits that name every phase
explicitly with `isLimitActive: false`, so the car knows the difference between "you are
unconstrained" and "I have not spoken yet".

## OSCEV — the same message, the opposite meaning

**Optimization of Self-Consumption During EV Charging** is OPEV's mirror image. The energy
manager tells the car how much *self-produced* current is going spare, and the car may take
it. The wire is nearly identical: the same per-phase ceilings in amperes on the same
`LoadControl` feature, the same four-second heartbeat, the same fallback.

Two elements differ, and they are the whole use case:

| | OPEV | OSCEV |
|---|---|---|
| `limitCategory` | `obligation` — a fuse | `recommendation` — an offer |
| `scopeType` | `overloadProtection` | `selfConsumption` |

A car that reads a recommendation as an obligation will throttle itself to the solar
surplus and stop charging when a cloud passes — having been *offered* free power, not
*limited* to it. The two usually run over the same connection at the same time, which is
the ordinary installation: the fuse says what the car may never exceed, the roof says what
is cheap right now, and the obligation wins.

The two are one implementation, `usecases::emobility::charging`, pointed by a `Purpose` —
the same shape as `Direction` for [LPC and LPP](@/docs/limitation.md).

## EVCEM — what actually flowed

**EV Charging Electricity Measurement** is what makes a curtailment checkable rather than
asserted. A manager that limits a car to 6 A on phase B and never reads back what it drew is
working from a number it has only ever written down.

```rust
use eebus::usecases::emobility::evcem;
use eebus::usecases::monitoring::{Measurand, Quantity};

let mut car = evcem::monitored_unit(1)
    .with(Measurand::on(Quantity::Current, Phase::B))
    .with(Measurand::unphased(Quantity::EnergyCharged));
```

`EnergyCharged` has scope `charge`: this session, into the battery. A wallbox's lifetime
import is a different number that happens to share a unit.

## EVSOC — how full, and how big

**EV State of Charge** is four scenarios, one mandatory and three optional, and the first
two are worth having together and nearly useless apart. A percentage says nothing about how
long charging will take; a capacity says nothing about how much is needed.

```rust
use eebus::usecases::emobility::evsoc::Battery;

battery.energy_to_full();     // Some(46_200.0) Wh — the number a planner uses
battery.usable_capacity();    // nominal × state of health
```

`energy_to_full` answers `None` rather than guessing when only one of the two has arrived: a
manager that assumed a capacity would plan a charge for a battery it invented.

Scenario 2 is not a measurement. The nominal capacity is a *characteristic* on the
`ElectricalConnection` feature — `energyCapacityNominalMax` — because the size of a battery
is a property of the car, not something read off it moment by moment. And scenario 4's
travel range is in **metres**, which is what the specification fixes and not what a
dashboard shows.

EVCEM and EVSOC run on the [shared measurement layer](@/docs/monitoring.md); what they add
is vocabulary, not mechanism.

## Not here: CEVC

**Coordinated EV Charging** is a different shape from everything above: three actors, power
sequences, incentive tables, and a charging plan negotiated between the car and the energy
broker rather than a ceiling imposed on it. It is not implemented.
