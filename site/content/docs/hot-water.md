+++
title = "Heat pumps — the HVAC family"
description = "All twelve HVAC use cases and OHPCF: the hot water's mode, overrun and temperature, a room's heating and cooling modes and setpoints, the room and outdoor thermometers, and the compressor process a CEM can start."
weight = 105
[extra]
group = "Use cases"
+++

Nearly every control use case here can only ask an appliance to do **less**. [LPC and
LPP](@/docs/limitation.md) set a ceiling; [OPEV](@/docs/e-mobility.md) sets a current a car
may not exceed; [COB](@/docs/use-cases.md) drives a battery between limits. A ceiling an
appliance is already under changes nothing, so a manager holding only those can never spend
a surplus.

This family sets targets, and a target can go up. All twelve HVAC use cases are here — the
complete contents of both specification bundles — as three exchanges:

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

CDT §2.4.2–2.4.3 is firm about the first two: a DHW Circuit that does not serve MDSF
**SHALL** serve CDSF [CDT-005]. One of them is mandatory because CDT's setpoints are
addressed *through* the operation modes — Table 10 relates each mode to the setpoints it
reads — so "write 60 °C" is only a complete instruction once you know which mode the circuit
is in.

A setpoint the circuit is not currently reading can be written, acknowledged, and change
nothing anybody can measure. Nothing on the wire says so, so there is a gate on the write —
the same shape as the limitation actor refusing a limit with no recent heartbeat:

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

Only `NotInCurrentMode` and `Unknown` are refusals — an overrun is "later", not "never". The
unconditional `write` remains, for a manager pre-loading the setpoint of a mode it is about
to ask for.

### Changing the mode, and pressing the button

CDSF has three scenarios, and the second is the one an energy manager reaches for:

| | | |
|---|---|---|
| 1 | Set DHW operation mode | `eco` while the grid is expensive, `on` while it is not |
| 2 | Start one-time DHW loading | the button in the bathroom, pressed over the wire |
| 3 | Stop one-time DHW loading | give it back when a cloud arrives |

Scenario 2 is the shortest path from "the roof is exporting" to "the tank is absorbing it".
Unlike a setpoint, which the circuit's controller may decline to act on, and unlike OHPCF's
process, which the compressor must have announced first, it is a direct instruction to heat
now.

```rust
use eebus::model::HvacOperationModeType;

circuit.start_overrun()?;                               // [CDSF-002]
circuit.set_mode_named(&HvacOperationModeType::Eco)?;   // [CDSF-001]
```

`set_mode_named` resolves the mode through what the circuit published and refuses what the
circuit would: a mode this system function does not relate to — modes are described once for
the device and a subset is related to each function, so `eco` may be a hot water mode and
not a room one — or a circuit that said `isOperationModeIdChangeable: false`. The server
side is `SystemFunction::apply`, which turns an incoming write into a `Request` or a
`ModeRefused` **before** the acknowledgement.

### The overrun is an announcement, not a state

`finished` **may only be a notification, sent once**; the status becomes `inactive`
afterwards and should never appear in a reply (Table 14). A circuit that leaves `finished`
standing tells every appliance that reads it later that a heating has just completed —
repeatedly. `OverrunReport` models the three resting states and hands the announcement back
separately:

```rust
state.overrun();               // Active | Running | Inactive
state.overrun_just_finished(); // the one-shot, not stored
```

It matters defensively too: a tank drawing power while the mode says `off` is not a fault,
it is somebody pressing the button, and reporting it as an anomaly is how a manager loses a
user's trust.

### And whether it worked

A setpoint is a request. What the tank is at depends on the mode, on the circuit's step
size, and on whether somebody has just had a shower. MDT is the thermometer, and it needs no
reader of its own — it is a `Measurement`, so the machinery that resolves MPC and MGCP
readings resolves this one:

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
`None` for both; the raw `value` is still there for a display, which is a different thing
from acting on it.

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

A reader keyed by the mode alone stores one entry for `auto`, whichever arrived last, and
then tells a manager that heating to 21 °C means writing the *cooling* setpoint. The room
applies it, acknowledges it, and gets colder.

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

MRT and MOT are what a thermal model of the building is fitted against: the temperature
inside, the temperature outside, and the heat delivered in between. A heat pump measures
both anyway — its heating curve runs on nothing else — and its outdoor sensor is on this
building's wall rather than in a forecast for the grid square.

## Which setpoint, though

A `Setpoint` feature carries every setpoint the device has, and Table 7 spells the
identifier `<st1#(1..4)>`: a server publishes one to four and numbers them itself. Writing 60
to the wrong one heats a living room. Three functions say between them which to write, and
none says it alone:

* `setpointDescriptionListData` — which identifiers carry this temperature, and in which
  unit. `degC`, `degF` **and** `K` are permitted; writing 60 to a circuit working in
  Fahrenheit is a cold shower.
* `setpointConstraintsListData` — the range and step size of each.
* `hvacSystemFunctionSetpointRelationListData` — which of them each mode of each system
  function reads.

`Setpoints` holds all three together, and `write` refuses a temperature outside the
published range rather than letting the server answer with a bare error number. A value off
the *step size* is not refused — rounding is the server's job — but `Constraints::rounded`
says what it will become.

§2.3.1.1 puts three rules on how many setpoints a mode may relate to, and
`relation_is_valid` enforces them rather than publishing a relation an appliance cannot act
on:

| Mode | Setpoints | |
|---|---|---|
| `auto` | one to four | [CDT-003/1] — which applies, and when, is the server's own business |
| `on`, `eco` | exactly one | [CDT-003/2] |
| `off` | none, or exactly one | [CDT-003/3] |

See [Addressing a peer's data](@/docs/use-cases.md) for why none of this may be shortcut.

## Finding one, and driving it

Nine of the twelve are served from an `HVAC` feature, and finding it is the lookup the
use-case implementation guide §3.3 makes easiest to get wrong: the feature is on the entity
that **announced the actor**, not whichever entity happens to carry an `HVAC` server. A heat
pump that heats water and two rooms has three of them.

Each use case has a `locate`, and — where a device can hold several — a `locate_all`:

```rust
let circuit = cdsf::locate(remote)?;          // the DHW circuit's `HVAC` feature
let warm    = crht::locate(remote)?;          // `HVAC` *and* `Setpoint`, on one entity
for room in crhsf::locate_all(remote) { … }   // one per `HVACRoom`
```

A located peer is an address, not a conversation: a `SystemFunction` answers nothing until
six payloads have arrived, and a client that read the descriptions and stopped holds a
reader that refuses every write it is asked to build. `HvacPeer::follow` subscribes and
reads the use case's whole scenario table in one call. The subscription goes first, so a
mode that changes between the reply and a later request is not missed; the reads are
filtered by what discovery said the peer serves, so a circuit without the one-time loading
is asked for four functions rather than six. `Following::function_of` names the
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

That last call is four joins deep: the circuit's own `systemFunctionId`, the mode it is
*currently* in, the relation keyed by both, and the constraints for the setpoint that names.
Two of its answers are refusals rather than guesses — `NotInCurrentMode` where the mode reads
no setpoint, and `SeveralSetpoints` where it reads more than one and which applies is the
circuit's own business. With no system-function use case attached it is `ModeUnknown`, which
is [CDT-005] as an API; `preload_setpoint` is the exception, for loading the setpoint of a
mode you are about to ask for.

Use cases attached against one entity gather on one **unit**, keyed by `UnitId` — device and
entity, the same key a `MonitoringApplianceActor` gives that room's thermometer. A room that
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
used for this Scenario"**. A server that insisted on one would refuse every conformant
Configuration Appliance. What replaces it is the application's own decision — every
writeable feature here defers its writes. See
[Who may write, and who may not](@/docs/use-cases.md).

## The other lever: OHPCF

A setpoint is a request the appliance's controller decides what to do about, and a tank
already at temperature will not run for a higher one. **Optimization of Self-Consumption by
Heat Pump Compressor Flexibility** is a different mechanism: the compressor announces that
it *could* run and how much it would draw, and the CEM **starts that process** at a time it
names, then stops, pauses or resumes it.

It is also the one heat-pump use case that binds. §3.4.1.1 says of scenario 1 that "Binding
SHOULD NOT be used"; §3.4.2 says of scenario 2 that an actor writing part of the feature
"needs to create a binding [...] Only one binding partner is allowed". A CEM that only
subscribed gets every notification and can act on none of them — `activate` comes back
`BindingRequired`, and nothing in the offer warned about it. `follow` sends all three
requests in the order the two scenarios put them:

```rust
use eebus::usecases::ohpcf::{self, CompressorOffer};

let compressor = ohpcf::locate(remote)?;
let pending = compressor.follow(&mut engine, &client, now);   // bind, subscribe, read

let read = CompressorOffer::read(&payload)?;
if read.is_available() {
    // The roof is exporting. Run it now.
    engine.write(&compressor.flexibility, &client, ohpcf::activate(read.sequence, "PT0S"), true, now);
}
```

One function on one `SmartEnergyManagementPs` feature, and what a payload *means* is which
of four phases the compressor is in: an offer, a process, an ending, or nothing at all. Two
things it tells a planner that nothing else does — [OHPCF-008], the least time the
compressor must run once started, and [OHPCF-009], the least time it must rest before
starting again.

The compressor refuses what the specification does not allow — a pause on something that is
not running, a stop it never announced as stoppable — before acknowledging it. [OHPCF-011/7]
guarantees it always announces at least one way out, so `Interrupt` has no empty case.
