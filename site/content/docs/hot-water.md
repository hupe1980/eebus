+++
title = "Heat pumps — the HVAC family"
description = "All twelve HVAC use cases and OHPCF: the hot water's mode, overrun and temperature, a room's heating and cooling modes and setpoints, the room and outdoor thermometers, and the compressor process a CEM can start."
weight = 105
[extra]
group = "Use cases"
+++

Nearly every control use case here can only ask an appliance to do **less**. [LPC and
LPP](@/docs/limitation.md) set a ceiling; [OPEV](@/docs/e-mobility.md) sets a current a car
may not exceed; [COB](@/docs/storage.md) drives a battery between limits. None of them
can make an appliance spend a surplus.

This family sets targets, and a target can go up. All twelve HVAC use cases — the complete
contents of both specification bundles — are implemented as three exchanges:

| | | |
|---|---|---|
| `hvac::system_function` | the operation mode | MDSF/CDSF, MRHSF/CRHSF, MRCSF/CRCSF |
| `hvac::setpoint` | the temperature setpoint | CDT, CRHT, CRCT |
| `hvac::temperature` | the thermometer | MDT, MRT, MOT |

## The hot water, in the order a manager uses them

| | | |
|---|---|---|
| **MDSF** — Monitoring of DHW System Function | DHW Circuit, Monitoring Appliance | which mode it is in |
| **CDSF** — Configuration of DHW System Function | DHW Circuit, Configuration Appliance | change the mode, or start a one-time loading |
| **CDT** — Configuration of DHW Temperature | DHW Circuit, Configuration Appliance | the setpoint |
| **MDT** — Monitoring of DHW Temperature | DHW Circuit, Monitoring Appliance | what the water got to |

CDT §2.4.2–2.4.3: a DHW Circuit that does not serve MDSF **SHALL** serve CDSF [CDT-005].
CDT's setpoints are addressed *through* the operation modes — Table 10 relates each mode to
the setpoints it reads — so a setpoint write is complete only once the current mode is
known.

A setpoint the circuit is not currently reading can be written and acknowledged without
effect, so the write is gated:

```rust
use eebus::usecases::hvac::setpoint::WriteRefused;

match known.write_effective(dhw, 60.0, &state) {
    Ok(write)                           => { /* send it */ }
    Err(WriteRefused::NotInCurrentMode) => { /* ask for the mode first */ }
    Err(WriteRefused::ModeUnknown)      => { /* MDSF has not spoken yet */ }
    Err(other)                          => { /* the circuit would refuse it anyway */ }
}
```

`effect_of` asks the same question without the write, and has four answers:

| | |
|---|---|
| `Effective` | the circuit is in a mode that reads this setpoint |
| `NotInCurrentMode` | it reads some other setpoint, or none — `off` may relate to none [CDT-003/3] |
| `OverriddenByOverrun` | a one-time heating is running over the top of it. The write **lands**; it takes effect when the overrun ends |
| `Unknown` | no mode has been reported, or the relations have not arrived |

Only `NotInCurrentMode` and `Unknown` are refusals. The unconditional `write` is for
pre-loading the setpoint of a mode the manager is about to ask for.

### Changing the mode, and pressing the button

CDSF has three scenarios:

| | | |
|---|---|---|
| 1 | Set DHW operation mode | `eco` while the grid is expensive, `on` while it is not |
| 2 | Start one-time DHW loading | the button in the bathroom, pressed over the wire |
| 3 | Stop one-time DHW loading | give it back when a cloud arrives |

Scenario 2 is a direct instruction to heat now — unlike a setpoint, which the circuit's
controller may decline to act on, and unlike OHPCF's process, which the compressor must have
announced first.

```rust
use eebus::model::HvacOperationModeType;

circuit.start_overrun()?;                               // [CDSF-002]
circuit.set_mode_named(&HvacOperationModeType::Eco)?;   // [CDSF-001]
```

`set_mode_named` resolves the mode through what the circuit published and refuses what the
circuit would: a mode this system function does not relate to (modes are described once per
device and a subset is related to each function), or a circuit that said
`isOperationModeIdChangeable: false`. The server
side is `SystemFunction::apply`, which turns an incoming write into a `Request` or a
`ModeRefused` **before** the acknowledgement.

### The overrun is an announcement, not a state

`finished` **may only be a notification, sent once**; the status becomes `inactive`
afterwards and should never appear in a reply (Table 14). `OverrunReport` models the three
resting states and hands the announcement back separately:

```rust
state.overrun();               // Active | Running | Inactive
state.overrun_just_finished(); // the one-shot, not stored
```

### And whether it worked

A setpoint is a request. MDT is the thermometer; it is a `Measurement`, so the machinery
that resolves MPC and MGCP readings resolves it:

```rust
use eebus::usecases::hvac::mdt;
use eebus::usecases::monitoring::Readings;

let mut readings = Readings::new();
readings.describe(&mdt::temperature_description());
readings.apply(&mdt::temperature(58.5));

readings.value(&mdt::MEASURAND);   // Some(58.5)
```

CDT's setpoint description carries a `measurementId` that §3.2.1.2.2.1 says **SHALL** be the
one MDT publishes, so an appliance can tie the reading to the setpoint that governs it:

```rust
cdt::setpoint_description_measuring(UnitOfMeasurement::DegC, mdt::MEASUREMENT_ID)
```

`outOfRange` and `error` **SHALL be ignored** (MDT §2.5.1). `Reading::usable()` returns
`None` for both; the raw `value` is still there for a display.

## The room: the same exchanges, twice over

| | monitor | configure |
|---|---|---|
| room heating mode | `mrhsf` | `crhsf` |
| room cooling mode | `mrcsf` | `crcsf` |
| room temperature | `mrt` | `crht` (heating), `crct` (cooling) |

§3.2.2.2.1 gives an entity **at most one** feature of a type. So a room that both heats and
cools publishes both system functions in the same four lists on the same `HVAC` feature, and
**both setpoints on the same `Setpoint` feature under the same
`scopeType: roomAirTemperature`**, in the same unit, described identically. Nothing in the
descriptions tells the heating setpoint from the cooling one.

The relation does. `hvacSystemFunctionSetpointRelationData` is keyed by `systemFunctionId`
(PRIMARY) **and** `operationModeId` (SUB), and the same `auto` appears under both functions:

```rust
setpoints.for_mode(heating, auto());   // [3] — the heating setpoint
setpoints.for_mode(cooling, auto());   // [4] — the cooling one
```

A reader keyed by the mode alone would keep one entry for `auto` and could direct a heating
write to the cooling setpoint.

### A building is several rooms

§7.5 keys use-case information by address, so a gateway announces
`monitoringOfRoomTemperature` once per `HVACRoom` entity. `locate_all` returns them all, and
a `MonitoringApplianceActor` is keyed by `UnitId` — a device **and** an entity — so they sit
side by side:

```rust
for room in mrt::locate_all(remote) {
    appliance.attach(&mut engine, room, now);   // each keeps its own Readings
}
```

MRT and MOT give the inside and outside temperatures a thermal model of the building is
fitted against.

## Which setpoint, though

A `Setpoint` feature carries every setpoint the device has, and Table 7 spells the
identifier `<st1#(1..4)>`: a server publishes one to four and numbers them itself. Three
functions together say which to write:

* `setpointDescriptionListData` — which identifiers carry this temperature, and in which
  unit. `degC`, `degF` **and** `K` are permitted.
* `setpointConstraintsListData` — the range and step size of each.
* `hvacSystemFunctionSetpointRelationListData` — which of them each mode of each system
  function reads.

`Setpoints` holds all three together, and `write` refuses a temperature outside the
published range rather than letting the server answer with a bare error number. A value off
the *step size* is not refused — rounding is the server's job — but `Constraints::rounded`
says what it will become.

§2.3.1.1 puts three rules on how many setpoints a mode may relate to;
`relation_is_valid` enforces them:

| Mode | Setpoints | |
|---|---|---|
| `auto` | one to four | [CDT-003/1] — which applies, and when, is the server's own business |
| `on`, `eco` | exactly one | [CDT-003/2] |
| `off` | none, or exactly one | [CDT-003/3] |

See [Addressing a peer's data](@/docs/use-cases.md).

## Finding one, and driving it

Nine of the twelve are served from an `HVAC` feature on the entity that **announced the
actor** (implementation guide §3.3), not whichever entity carries an `HVAC` server. A heat
pump that heats water and two rooms has three of them.

Each use case has a `locate`, and — where a device can hold several — a `locate_all`:

```rust
let circuit = cdsf::locate(remote)?;          // the DHW circuit's `HVAC` feature
let warm    = crht::locate(remote)?;          // `HVAC` *and* `Setpoint`, on one entity
for room in crhsf::locate_all(remote) { … }   // one per `HVACRoom`
```

A located peer is only an address: a `SystemFunction` answers nothing until six payloads
have arrived. `HvacPeer::follow` subscribes and reads the use case's whole scenario table in
one call. The subscription goes first, so no change is missed; the reads are filtered by
what discovery said the peer serves, so a circuit without the one-time loading is asked for
four functions rather than six. `Following::function_of` names the
function behind a refusal.

### Or let the actor do it

`HvacApplianceActor` is `follow` plus the bookkeeping — a reader per system function, a
reader per setpoint scope, routing by feature address, and change detection:

```rust
use eebus::usecases::hvac::{self, HvacApplianceActor, cdsf, cdt};

let mut appliance = HvacApplianceActor::new(client.clone());
appliance.attach(&mut engine, cdsf::locate(remote)?, now);   // the mode and the overrun
appliance.attach(&mut engine, cdt::locate(remote)?, now);    // the temperature

// …once the replies have gone through `appliance.handle_event(&event)`:
appliance.set_temperature(&mut engine, &unit, &hvac::DHW, 60.0, now)?;
```

That last call joins the circuit's `systemFunctionId`, its *current* mode, the relation
keyed by both, and the constraints for the setpoint that names. Two answers are refusals — `NotInCurrentMode` where the mode reads
no setpoint, and `SeveralSetpoints` where it reads more than one and which applies is the
circuit's own business. With no system-function use case attached it is `ModeUnknown`, which
is [CDT-005] as an API; `preload_setpoint` is the exception, for loading the setpoint of a
mode you are about to ask for.

Use cases attached against one entity gather on one **unit**, keyed by `UnitId` (device and
entity), the same key a `MonitoringApplianceActor` uses. A room that
heats and cools is one unit with two system functions and one setpoint reader, and
`set_temperature(&unit, &hvac::HEATING, …)` and the same call with `&hvac::COOLING` reach
different setpoints out of the same `roomAirTemperature` list.

`handle_event` returns a **list**: one `hvacSystemFunctionListData` carries every system
function the appliance has, so one notification can move the heating and the cooling at
once. Only changes are reported.

| | |
|---|---|
| `FunctionDescribed` | an identifier, its modes and a current one: scenario 1 can be reported |
| `ModeChanged` | including when the *appliance* changed it — the wall panel, or its own scheduler |
| `OverrunChanged` | somebody pressed the one-time loading button |
| `OverrunFinished` | the one-shot [MDSF-002] announcement, which is not a state |
| `SetpointChanged` | a setpoint moved, whoever moved it |

## Nothing here binds

All twelve say the same sentence, including the six that write: **"Binding SHOULD NOT be
used for this Scenario"**. Every writeable feature here defers its writes to the
application instead. See
[Who may write, and who may not](@/docs/use-cases.md).

## The other lever: OHPCF

**Optimization of Self-Consumption by Heat Pump Compressor Flexibility** is a different
mechanism from a setpoint: the compressor announces that
it *could* run and how much it would draw, and the CEM **starts that process** at a time it
names, then stops, pauses or resumes it.

It is also the one heat-pump use case that binds. §3.4.1.1 says of scenario 1 that "Binding
SHOULD NOT be used"; §3.4.2 says of scenario 2 that an actor writing part of the feature
"needs to create a binding [...] Only one binding partner is allowed". Without a binding
`activate` comes back `BindingRequired`. `follow` sends all three requests in the order the
two scenarios put them:

```rust
use core::time::Duration;
use eebus::usecases::ohpcf::{self, CompressorOffer};

let compressor = ohpcf::locate(remote)?;
let pending = compressor.follow(&mut engine, &client, now);   // bind, subscribe, read

let read = CompressorOffer::read(&payload)?;
if read.is_available() {
    // The roof is exporting. Run it now.
    let write = ohpcf::activate(read.sequence, Duration::ZERO);
    engine.write(&compressor.flexibility, &client, write, true, now);
}
```

**The start time is a span, not an instant**: `Duration::ZERO` is "now" and two hours is
"in two hours", and the compressor side refuses any other `startTime` as
`UnreadableStartTime`. See
[Where a use case is narrower than the schema](@/docs/wire-format.md#where-a-use-case-is-narrower-than-the-schema-the-use-case-wins).

One function on one `SmartEnergyManagementPs` feature, and what a payload *means* is which
of four phases the compressor is in: an offer (`inactive`, no schedule), a process
(`scheduled`, `running` or `paused`), an ending (`completed` or `invalid`), or nothing (no
`alternatives`). It also tells a planner [OHPCF-008], the least time the compressor must
run once started, and [OHPCF-009], the least time it must rest before starting again.

The compressor refuses what the specification does not allow — a pause on something that is
not running, a stop it never announced as stoppable — before acknowledging it, as LPC's
Controllable System does. [OHPCF-011/7]
guarantees it always announces at least one way out, so `Interrupt` has no empty case.
