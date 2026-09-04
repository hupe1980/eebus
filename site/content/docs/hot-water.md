+++
title = "The DHW trio — hot water"
description = "CDT, MDSF and MDT: which mode a hot water circuit is in, setting its temperature setpoint — one of the two levers here that can ask an appliance to use more — and what the water actually got to."
weight = 105
[extra]
group = "Use cases"
+++

Nearly every control use case here can only ask an appliance to do **less**. [LPC and
LPP](@/docs/limitation.md) set a ceiling; [OPEV](@/docs/e-mobility.md) sets a current a car
may not exceed; [COB](@/docs/use-cases.md) drives a battery between limits. **Configuration
of DHW Temperature** is one of the two exceptions: it sets a target, and a target can go up.

That is not a detail. A hot water tank is the cheapest thermal battery in most buildings,
and raising its setpoint by ten degrees while the roof is exporting stores a few
kilowatt-hours that would otherwise be sold at the feed-in tariff and bought back at the
retail one. No limit, however carefully written, can ask for that — a ceiling the appliance
is already under changes nothing.

```rust
use eebus::usecases::hvac::cdt::DhwSetpoints;

// Everything the circuit published, read once at commissioning.
let mut known = DhwSetpoints::new();
known.learn(&descriptions);
known.learn(&constraints);
known.learn(&relations);

let dhw = known.temperature_setpoints().next().expect("a DHW setpoint");
let write = known.write(dhw, 60.0)?;    // refused here if the circuit would refuse it
```

## Three use cases, in the order a manager uses them

| | | |
|---|---|---|
| **MDSF** — Monitoring of DHW System Function | DHW Circuit, Monitoring Appliance | whether a write will reach anything |
| **CDT** — Configuration of DHW Temperature | DHW Circuit, Configuration Appliance | the write itself |
| **MDT** — Monitoring of DHW Temperature | DHW Circuit, Monitoring Appliance | whether it worked |

MDSF and CDT are the mandatory pair; MDT is what makes the answer checkable.

CDT §2.4.2–2.4.3 is unusually firm: a DHW Circuit that does not serve "Monitoring of DHW
System Function" **SHALL** serve "Configuration of DHW System Function" [CDT-005]. One of
the two is mandatory, and it is not paperwork. CDT's setpoints are addressed *through* the
operation modes — Table 10 relates each mode to the setpoints it reads — so "write 60 °C" is
only a complete instruction once you know which mode the circuit is in.

A setpoint the circuit is not currently reading can be written, acknowledged, and change
nothing anybody can measure. Nothing on the wire says so.

```rust
// Which setpoint would a write actually reach, right now?
let reachable = state.current_setpoints(&known);
```

So there is a gate on the write, the same shape as the limitation actor refusing a limit
with no recent heartbeat: the message would be accepted and mean nothing.

```rust
use eebus::usecases::hvac::cdt::WriteRefused;

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
| `OverriddenByOverrun` | a one-time heating is running over the top of it (MDSF Table 12). The write **lands**; it takes effect when the overrun ends |
| `Unknown` | no mode has been reported, or CDT's relations have not arrived |

Only `NotInCurrentMode` and `Unknown` are refusals — an overrun is "later", not "never".
The unconditional `write` remains, for a manager pre-loading the setpoint of a mode it is
about to ask for.

Both specifications also give an entity **one** `HVAC` feature (§3.2.2.2.1), so a circuit
serving both puts MDSF's six functions and CDT's relations on the same one —
`mdsf::with_cdt` is that feature.

## And whether it worked

A setpoint is a request. What the tank is actually at depends on the mode, on the circuit's
own step size, and on whether somebody has just had a shower. MDT is the thermometer, and it
needs no reader of its own — it is a `Measurement`, so the machinery that already resolves
MPC and MGCP readings resolves this one:

```rust
use eebus::usecases::hvac::mdt;
use eebus::usecases::monitoring::Readings;

let mut readings = Readings::new();
readings.describe(&mdt::temperature_description());
readings.apply(&mdt::temperature(58.5));

readings.value(&mdt::MEASURAND);   // Some(58.5)
```

It is the one measurement in this crate that is not electricity: MDT Table 7 fixes
`commodityType: domesticHotWater`, and a client filtering on the commodity would not find a
tank published as power.

The identifiers tie together too. CDT's setpoint description carries a `measurementId` that
§3.2.1.2.2.1 says **SHALL** be the one MDT publishes — so an appliance can tie the reading
to the setpoint that governs it, rather than guessing that the only temperature it can see
is the one it just wrote:

```rust
cdt::setpoint_description_measuring(UnitOfMeasurement::DegC, mdt::MEASUREMENT_ID)
```

### Flagged values are not readings

Both specifications give three value states, and say the same thing about both of the
abnormal ones: `outOfRange` and `error` **SHALL be ignored** by the appliance (MPC §2.5.2,
MDT §2.5.1). It is tempting to read `outOfRange` as "high but real" — a meter reporting
14 kW against an 11 kW constraint looks like a number worth having — and it is not
permitted. `Reading::usable()` returns `None` for both; the raw `value` field is still there
for a display, which is a different thing from acting on it.

## Which setpoint, though

A `Setpoint` feature carries every setpoint the device has. A heat pump's room air
temperature is the same `valueAbsolute` in the same `degC` as its hot water, and Table 7
spells the identifier `<st1#(1..4)>` — a circuit publishes **one to four** temperature
setpoints and numbers them itself. Writing 60 to the wrong one heats a living room.

Three functions say between them which one to write, and none of them says it alone:

* `setpointDescriptionListData` — which identifiers are DHW temperatures at all
  (`scopeType: dhwTemperature`), and in which unit. Table 7 permits `degC`, `degF` **and**
  `K`; writing 60 to a circuit working in Fahrenheit is a cold shower.
* `setpointConstraintsListData` — the range and step size of each.
* `hvacSystemFunctionSetpointRelationListData` — which of them the circuit uses in which
  operation mode.

`DhwSetpoints` is the reader that holds all three together, and `write` refuses a
temperature outside the published range rather than letting the circuit answer with a bare
error number. A value off the *step size* is not refused — Table 8 makes rounding the
server's job — but `Constraints::rounded` says what it will become.

See [Addressing a peer's data](@/docs/use-cases.md) for why none of this may be shortcut.

## The operation-mode rules

§2.3.1.1 puts three different rules on how many setpoints a mode may relate to, and
`relation_is_valid` enforces them rather than publishing a relation a Configuration
Appliance cannot act on:

| Mode | Setpoints | |
|---|---|---|
| `auto` | one to four | [CDT-003/1] — which one applies, and when, is the circuit's own business |
| `on`, `eco` | exactly one | [CDT-003/2] |
| `off` | none, or exactly one | [CDT-003/3] |

A circuit that told an appliance `on` maps to two setpoints has said nothing usable: the
appliance would have to guess which of them its write takes effect on.

## The overrun

MDSF scenario 2 is the "one-time DHW loading" button in the bathroom: it overrides the
current operation mode until it finishes. It matters to an energy manager for a defensive
reason — a tank drawing power while the mode says `off` is not a fault, and reporting it as
one is how a manager loses a user's trust.

Table 14 puts a rule on it that is easy to get wrong and impossible to see: `finished` **may
only be a notification, sent once**, the status becomes `inactive` afterwards, and it should
never appear in a reply. A circuit that leaves `finished` standing tells every appliance
that reads it later that a heating has just completed — repeatedly. `OverrunReport` models
the three resting states and hands the announcement back separately:

```rust
state.overrun();               // Active | Running | Inactive
state.overrun_just_finished(); // the one-shot, not stored
```

## The other lever: OHPCF

A setpoint is a request the circuit's own controller decides what to do about, and a tank
already at temperature will not run for a higher one. **Optimization of Self-Consumption by
Heat Pump Compressor Flexibility** is the other half of the same idea and a different
mechanism: the compressor announces that it *could* run and how much it would draw, and the
CEM **starts that process** at a time it names — then stops, pauses or resumes it.

```rust
use eebus::usecases::ohpcf::{self, CompressorOffer, Flexibility, Interrupt};

let read = CompressorOffer::read(&payload)?;
if read.is_available() {
    // The roof is exporting. Run it now.
    engine.write(&peer.flexibility, &client, ohpcf::activate(read.sequence, "PT0S"), true, now);
}
```

It is one function on one `SmartEnergyManagementPs` feature, and what a payload *means* is
which of four phases the compressor is in — an offer, a process, an ending, or nothing at
all. Two things it tells a planner that nothing else does: [OHPCF-008], the least time the
compressor must run once started, and [OHPCF-009], the least time it must rest before
starting again. Short-cycling a compressor is how they die, and the CEM is the only thing in
a position to avoid it.

The compressor refuses what the specification does not allow — a pause on something that is
not running, a stop it never announced as stoppable — before acknowledging it. [OHPCF-011/7]
guarantees it always announces at least one way out, so `Interrupt` has no empty case, and a
peer that announces neither is reported rather than assumed.

## What is not here

Nine further HVAC use cases — room and cooling temperature, their system functions, outdoor
temperature — are specified in the same two documents and are not implemented. Nor is
"Configuration of DHW System Function", the writeable counterpart of MDSF, which a circuit
may serve *instead* of it to satisfy [CDT-005].
