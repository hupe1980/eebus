+++
title = "E-mobility"
description = "EVSECC and OPEV: how a wallbox commissions itself, and how an energy manager holds the charging current below what the house supply can carry — per phase, in four seconds."
weight = 100
[extra]
group = "Use cases"
+++

The same machinery runs the e-mobility family: a wallbox, the car plugged into it, and the
energy manager that has to keep the house fuse intact.

## EVSECC — commissioning the wallbox

**EVSE Commissioning and Configuration** is how a wallbox introduces itself to an energy
manager — manufacturer, model, firmware — and how it reports being broken. Everything else
in the family assumes this has happened.

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

## Saying "no curtailment needed"

An empty limit list says nothing — it cannot be distinguished from a message that lost its
contents. When no curtailment is required the guard sends limits that name every phase
explicitly with `isLimitActive: false`, so the car knows the difference between "you are
unconstrained" and "I have not spoken yet".
