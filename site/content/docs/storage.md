+++
title = "Inverters, PV and batteries"
description = "MOI, MPS and MOB: what an inverter, a PV string and a battery report. COB: controlling the battery."
weight = 95
[extra]
group = "Use cases"
+++

Four use cases cover the generation side of a building, nested the way the hardware is: an
**inverter** with **PV strings** on one DC bus and a **battery** on another, presenting one
AC connection to the house.

| Use case | Actor | What it answers |
|---|---|---|
| **MOI** — Monitoring of Inverter | Inverter | apparent and reactive power, yields, cos φ |
| **MPS** — Monitoring of PV String | PVString | what each string is producing, on its DC side |
| **MOB** — Monitoring of Battery | Battery | power, current, voltage, energy, state of charge |
| **COB** — Control of Battery | Inverter | what the battery *should* do |

## Layering, not repetition

MOI §2.5.1: an inverter **SHALL** also support
[MPC](@/docs/monitoring.md), which carries the ordinary AC values — active power, current,
voltage, frequency. MOI adds only what is specific to a machine converting DC into AC:
apparent and reactive power, the power factor, the four yield counters, the nameplate, and
a component temperature.

The same nesting appears in the entity tree. A `PVString` and a `Battery` are sub-entities
of an `Inverter`, and an installation with two roofs has two `PVString` entities under one
inverter, so MPS shows a shaded or failing string that a building's total hides.

## One measurement layer, eleven use cases

MOI, MPS and MOB are built on the same `usecases::monitoring` machinery as MPC, MGCP,
EVCEM, EVSOC, COB and the three HVAC thermometers:

```rust
use eebus::usecases::mob;
use eebus::usecases::monitoring::{Measurand, Quantity};

let mut battery = mob::monitored_unit(1)
    .with(Measurand::unphased(Quantity::DcPower))
    .with(Measurand::unphased(Quantity::StateOfCharge));
```

`Measurand::unphased` is the DC counterpart of `total` and `on`. A direct-current
measurement has no phases, and the parameter description **omits** `acMeasuredPhases`
rather than defaulting it to `abc`.

Three of a battery's values are easy to confuse and are separate quantities:

* `stateOfCharge` is a **percentage** — how full.
* `stateOfEnergy` is **watt-hours** — how much is in there.
* `useableCapacity` is **watt-hours** — how much it could hold, after ageing.

## COB — the other control use case

Everything above reads; COB writes. It has the same safety shape as the
[LPC/LPP pair](@/docs/limitation.md): a heartbeat, a failsafe state, a Failsafe Duration
Minimum between two and twenty-four hours, and a state machine that fails *towards* a
known-safe number rather than towards the last thing it was told.

A manager may control a battery in either of two frames:

```rust
use eebus::usecases::cob::{ControlMode, SetpointWrite};

// "Charge at 3 kW."
inverter.on_control_mode(ControlMode::Power, true, now);
inverter.on_setpoint(&SetpointWrite::active(3_000.0), true, now);

// Or: "hold the grid connection at zero, and work the rest out yourself."
inverter.on_control_mode(ControlMode::Pcc, true, now);
inverter.on_setpoint(&SetpointWrite::active(0.0), true, now);
```

`pcc` suits self-consumption optimisation: the inverter, which reacts to the grid
connection in milliseconds, does the arithmetic.

Two details:

**The sign convention is passive** ([COB-001/1]): positive is consumption, negative is
production. A setpoint of −3 000 W is discharging at three kilowatts.

**Which setpoint `power` mode uses depends on the machine.** A *battery inverter* has only
an AC side, so [COB-011] sets AC power. A *hybrid inverter* also has PV strings on its DC
bus, so [COB-012] sets DC power and the AC side is whatever the sun leaves over.
`InverterKind` is therefore a constructor argument.

## Deactivated is not zero

[COB-917]: a deactivated setpoint hands the inverter to its **default setpoint**, which is a configured number — not nought, and not the last thing
the manager said. `EffectiveControl` names which of the four is driving the machine:

```rust
EffectiveControl::Setpoint(3_000.0)   // the manager's number
EffectiveControl::Default(-500.0)     // deactivated or expired  [COB-917]
EffectiveControl::Failsafe(0.0)       // the manager went quiet  [COB-913]
EffectiveControl::Autonomous          // the inverter's own logic
```

Which of the four applies is not visible on the wire.
