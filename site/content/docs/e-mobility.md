+++
title = "E-mobility"
description = "The seven e-mobility use cases: who is on the end of the cable, what it may take, and what it actually did — EVSECC, EVCC, OPEV, OSCEV, EVCEM, EVSOC and EVCS."
weight = 100
[extra]
group = "Use cases"
+++

The e-mobility family covers a wallbox, the car plugged into it, and the energy manager
that keeps the house fuse intact. A car arrives, says what it is, and leaves again, so half
of the family is about finding out what is on the end of the cable. The use cases layer:

| | Use case | What it answers |
|---|---|---|
| **Who is there** | EVSECC | what the wallbox is |
| | EVCC | what the *car* is, and what it can be asked |
| **What it may take** | OPEV | the ceiling the fuse imposes — an *obligation* |
| | OSCEV | the surplus the roof is producing — a *recommendation* |
| **What it did** | EVCEM | the current, power and energy that really flowed |
| | EVSOC | how full the battery is, and how big |
| | EVCS | what the finished session cost, and where its energy came from |

## EVSECC — commissioning the wallbox

**EVSE Commissioning and Configuration** is how a wallbox introduces itself to an energy
manager — manufacturer, model, firmware — and how it reports being broken.

## EVCC — commissioning the car

**EV Commissioning and Configuration** does the same for the car, in eight scenarios. Two of them have no message at all: scenario 1 is an `EV`
entity *appearing* underneath the `EVSE` entity, and scenario 8 is it going away again.
Detailed discovery carries both.

Scenario 2 decides what the rest of the family can do:

```rust
use eebus::usecases::emobility::evcc::{CommunicationStandard, EvProfile};

let car = EvProfile::new()
    .communication_standard(CommunicationStandard::Iso15118Ed2)
    .asymmetric_charging(true)
    .charging_power(1_400.0, 11_000.0);

car.supports_data_exchange();   // true — so EVSOC can be asked
```

A car on **IEC 61851** has only a pilot wire: it can be offered a current but not asked
anything (no state of charge, no charging plan). A car on **ISO 15118** has a data link.
Check this field before the first request; reads to an IEC 61851 car are never answered.

`asymmetric_charging` is what OPEV needs before it writes three different currents. The
charging power band's *minimum* matters: below it a car stops charging rather than charging
slowly.

## OPEV — keeping the fuse intact

**Overload Protection by EV Charging Current Curtailment** is LPC's counterpart for the
car:

| | LPC | OPEV |
|---|---|---|
| What is limited | total active power, in watts | charging current, **per phase**, in amperes |
| Heartbeat timeout | 120 s | **4 s** |
| On losing the peer | the failsafe power limit, for hours | a safe current, immediately |

**Four seconds**, because a car follows its pilot signal at once. **Per phase**, because a
car that can charge asymmetrically takes 16 A on the phase with room and 6 A on the one
already loaded; the specification's example is 690 W against 460 W.

```rust
use eebus::model::ElectricalConnectionPhaseName as Phase;

guard.require(&car, ChargingCurrents::new(16.0, 10.0, 6.0));   // asymmetric [OPEV-002]

// …and on the car:
ev.charging().effective(Phase::A);   // Some(16.0) while the manager is heard from
ev.charging().source();              // Curtailed | GuardSilent | GuardFailed
```

A manager that goes quiet and one that announces a failure mean the same to the car: fall
back to a safe current immediately.

### Which `limitId` is which phase is the car's to say

Table 6 spells the three identifiers `<x1>`, `<x2>`, `<x3>` — placeholders. Nothing on the
wire ties a `limitId` to a phase directly: the limit description points at a
`measurementId`, and the *parameter* descriptions give that `measurementId` its phase.
`PhaseLimits` composes both, and `OverloadGuardActor::attach` waits for it before writing.
A wrong mapping is acknowledged without error and curtails the wrong phase.

A car that serves OPEV *and* OSCEV carries two limits per phase on the same feature, an
`obligation` and a `recommendation`, so `PhaseLimits` also matches on category and scope.

## Saying "no curtailment needed"

An empty limit list cannot be distinguished from a message that lost its contents. When no
curtailment is required the guard sends limits naming every phase with
`isLimitActive: false`, so the car can tell "unconstrained" from "not spoken yet".

## OSCEV — the same message, the opposite meaning

**Optimization of Self-Consumption During EV Charging** is OPEV's mirror image. The energy
manager tells the car how much *self-produced* current is going spare, and the car may take
it: the same per-phase ceilings in amperes on the same `LoadControl` feature, the same
four-second heartbeat, the same fallback. Two elements differ:

| | OPEV | OSCEV |
|---|---|---|
| `limitCategory` | `obligation` — a fuse | `recommendation` — an offer |
| `scopeType` | `overloadProtection` | `selfConsumption` |

A car that reads the recommendation as an obligation throttles itself to the solar surplus.
The two usually run over the same connection at the same time; the obligation wins.

They are one implementation, `usecases::emobility::charging`, pointed by a `Purpose` —
the same shape as `Direction` for [LPC and LPP](@/docs/limitation.md).

## EVCEM — what actually flowed

**EV Charging Electricity Measurement** reports what the car actually drew, so a
curtailment can be checked.

```rust
use eebus::usecases::emobility::evcem;
use eebus::usecases::monitoring::{Measurand, Quantity};

let mut car = evcem::monitored_unit(1)
    .with(Measurand::on(Quantity::Current, Phase::B))
    .with(Measurand::unphased(Quantity::EnergyCharged));
```

`EnergyCharged` has scope `charge`: this session, into the battery — not a wallbox's
lifetime import.

## EVSOC — how full, and how big

**EV State of Charge** has four scenarios, one mandatory and three optional. The first two,
percentage and capacity, are needed together to plan a charge.

```rust
use eebus::usecases::emobility::evsoc::Battery;

battery.energy_to_full();     // Some(46_200.0) Wh — the number a planner uses
battery.usable_capacity();    // nominal × state of health
```

`energy_to_full` answers `None` when only one of the two has arrived.

Scenario 2 is not a measurement: the nominal capacity is a *characteristic* on the
`ElectricalConnection` feature, `energyCapacityNominalMax`. Scenario 4's travel range is in
**metres**.

EVCEM and EVSOC run on the [shared measurement layer](@/docs/monitoring.md).

## EVCS — what the session cost, and where the energy came from

**EV Charging Summary** goes the other way from everything above. The EVSE serves a
**writeable** `Bill` and the energy manager writes into it: only the manager knows the PV
production and tariff, and the wallbox is where a driver looks.

```rust
use eebus::usecases::emobility::evcs::{ChargingSummary, Share};
use eebus::model::Currency;

// 18 kWh at €4.20 — a quarter of the energy off the grid, and nearly all of the bill.
let summary = ChargingSummary::new(18_000.0, 4.20, Currency::EUR)
    .from_grid(Share::new(25.0, 90.0))
    .self_produced(Share::new(75.0, 10.0));

summary.grid_energy_wh();       // Some(4_500.0)
summary.self_produced_cost();   // Some(0.42)
```

The energy split and the cost split are different numbers and are kept apart.

Before matching on an actor name:

* **The client has two conformant names.** §3.2.2.1: `CEM` where the Energy Guard and the
  Energy Broker are the same CEM entity, `EnergyBroker` where they are different ones.
* **`evcs::locate` matches on the role, not the name.** §3.2.2 gives the Energy Broker "only
  client functionality", so the peer serving a `Bill` is the EVSE whatever it calls itself
  (the Porsche Mobile Charger Connect announces this use case under actor `EV`).

The wallbox asks for a summary by setting `updateRequired` [EVCS-009], and clears it once the
write arrives. Tables 6 and 8 both warn the result "SHOULD NOT be used for actual billing as
it may also contain approximated values".

## Not here: CEVC

**Coordinated EV Charging** is not implemented. It has three actors, power sequences,
incentive tables, and a charging plan negotiated between the car and the energy broker
rather than a ceiling imposed on it.
