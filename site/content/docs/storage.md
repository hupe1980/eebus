+++
title = "Inverters, PV and batteries"
description = "MOI, MPS, MOB and COB: what an inverter, a string and a battery report, and the only control use case outside the grid pair."
weight = 95
[extra]
group = "Use cases"
+++

Four use cases about the machines on the generation side of a building, and they nest the
way the hardware does: an **inverter** with **PV strings** on one DC bus and a **battery**
on another, presenting one AC connection to the house.

| Use case | Actor | What it answers |
|---|---|---|
| **MOI** — Monitoring of Inverter | Inverter | apparent and reactive power, yields, cos φ |
| **MPS** — Monitoring of PV String | PVString | what each string is producing, on its DC side |
| **MOB** — Monitoring of Battery | Battery | power, current, voltage, energy, state of charge |
| **COB** — Control of Battery | Inverter | what the battery *should* do |

## Layering, not repetition

MOI is explicit about it (§2.5.1): an inverter **SHALL** also support
[MPC](@/docs/monitoring.md), which carries the ordinary AC values — active power, current,
voltage, frequency. MOI adds only what is specific to a machine converting DC into AC:
apparent and reactive power, the power factor, the four yield counters, the nameplate, and
a component temperature.

The same nesting appears in the entity tree. A `PVString` and a `Battery` are sub-entities
of an `Inverter`, and an installation with two roofs has two `PVString` entities under one
inverter — which is the point of MPS rather than MGCP. A shaded or failing string is
invisible in a building's total and obvious in its own measurement.

## One measurement layer, nine use cases

MOI, MPS and MOB are built on the same `usecases::monitoring` machinery as MPC, MGCP,
EVCEM, EVSOC, COB and MDT. What they add is vocabulary, not mechanism:

```rust
use eebus::usecases::mob;
use eebus::usecases::monitoring::{Measurand, Quantity};

let mut battery = mob::monitored_unit(1)
    .with(Measurand::unphased(Quantity::DcPower))
    .with(Measurand::unphased(Quantity::StateOfCharge));
```

`Measurand::unphased` is the DC counterpart of `total` and `on`. A direct-current
measurement has no phases, and the parameter description **omits** `acMeasuredPhases`
rather than defaulting it to `abc` — a client waiting for phase information on a DC
measurement would wait forever.

Three of a battery's values are easy to confuse and are separate quantities here for that
reason:

* `stateOfCharge` is a **percentage** — how full.
* `stateOfEnergy` is **watt-hours** — how much is in there.
* `useableCapacity` is **watt-hours** — how much it could hold, after ageing.

A client that read `stateOfEnergy` as a percentage would see a 12 kWh battery as 12 000 per
cent full.

## COB — the other control use case

Everything above reads. COB writes, and it is the only control use case in the crate
outside the [LPC/LPP pair](@/docs/limitation.md) — with, unsurprisingly, the same safety
shape: a heartbeat, a failsafe state, a Failsafe Duration Minimum between two and
twenty-four hours, and a state machine that fails *towards* a known-safe number rather than
towards the last thing it was told.

Its own idea is that a manager may control a battery in either of two frames:

```rust
use eebus::usecases::cob::{ControlMode, SetpointWrite};

// "Charge at 3 kW."
inverter.on_control_mode(ControlMode::Power, true, now);
inverter.on_setpoint(&SetpointWrite::active(3_000.0), true, now);

// Or: "hold the grid connection at zero, and work the rest out yourself."
inverter.on_control_mode(ControlMode::Pcc, true, now);
inverter.on_setpoint(&SetpointWrite::active(0.0), true, now);
```

`pcc` is the mode that self-consumption optimisation actually wants, and the reason the
inverter rather than the manager does the arithmetic: an inverter reacts to the grid
connection in milliseconds, and a manager on a network cannot.

Two details bite:

**The sign convention is passive** ([COB-001/1]): positive is consumption, negative is
production. A setpoint of −3 000 W is discharging at three kilowatts, and a caller that
gets the sign wrong charges when it meant to discharge.

**Which setpoint `power` mode uses depends on the machine.** A *battery inverter* has only
an AC side, so [COB-011] sets AC power. A *hybrid inverter* also has PV strings on its DC
bus, so [COB-012] sets DC power and the AC side is whatever the sun leaves over. Telling a
hybrid inverter an AC number would not say what the battery should do, which is why
`InverterKind` is a constructor argument rather than something to remember.

## Deactivated is not zero

[COB-917] is the rule most easily got wrong. A deactivated setpoint hands the inverter to
its **default setpoint**, which is a configured number — not nought, and not the last thing
the manager said. `EffectiveControl` names which of the four is driving the machine:

```rust
EffectiveControl::Setpoint(3_000.0)   // the manager's number
EffectiveControl::Default(-500.0)     // deactivated or expired  [COB-917]
EffectiveControl::Failsafe(0.0)       // the manager went quiet  [COB-913]
EffectiveControl::Autonomous          // the inverter's own logic
```

In a household that is being billed for what the battery does, which of those four applied
is the whole question — and it is not visible on the wire.
