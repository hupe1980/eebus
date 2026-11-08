+++
title = "Use cases"
description = "What an EEBUS use case is, how a device publishes the ones it plays, and how a peer discovers them before exchanging anything."
weight = 70
[extra]
group = "Use cases"
+++

A use case is the application layer of EEBUS. It names the **actors** involved, the
numbered **scenarios** they run, and the SPINE features and functions each scenario needs.
A peer expects every scenario a device advertises.

## Publishing what you play

A device publishes its use cases in `nodeManagementUseCaseData`, one descriptor per actor:

```rust
use eebus::usecases::{lpc, UseCaseDescriptor};

lpc::CONTROLLABLE_SYSTEM;   // the appliance's side
lpc::ENERGY_GUARD;          // the controlling side
```

Each descriptor carries the use case name, its version, the actor, the scenarios supported
and the entity types it is valid for — taken from the use-case technical specification's
own tables and, where the 2026 implementation guides revise them, from those.

## Discovering what a peer plays

```rust
match engine.poll_event() {
    Some(SpineEvent::UseCasesUpdated { device }) => { /* ask what it plays */ }
    _ => {}
}
```

The answer says which use cases the peer plays, in which actor role, and which scenarios of
each.

## Who may write, and who may not

Whether a write needs a binding is a property of the **feature**, not a rule of the protocol
(SPINE §7.3), and the use cases do not agree about their own:

| | |
|---|---|
| **binds** | LPC/LPP, OPEV/OSCEV, COB, EVCS, OHPCF scenario 2 — "Actors that write parts of a Feature within this Scenario need to create a binding [...] Only one binding partner is allowed" |
| **does not** | every HVAC use case, including all six that write — "Binding SHOULD NOT be used for this Scenario" |

The feature constructors carry it; the default is `Required`:

```rust
limitation::load_control_feature(1);   // WriteBinding::Required
cdt::setpoint_feature(1);              // WriteBinding::NotRequired — CDT §3.4.1.1
```

HVAC writes are deferred, so a product that accepts writes only from its commissioned
manager enforces that where it sees who is asking. See
[Bindings and subscriptions](@/docs/spine.md).

## Who has to subscribe, and what silence means

On subscription the use cases agree: every scenario says "Actors SHALL create a
subscription for each server Feature that is relevant for the corresponding Actor within
this Scenario" (§3.4.n.1), and §3.3.4 names polling only as the fallback for a refused
subscription. The descriptor derives the list:

```rust
mpc::MONITORING_APPLIANCE.features_needing_subscription();
// [ElectricalConnection, Measurement] — five scenarios, two features, each named once
```

`tests/use_case_delivery.rs` checks that every actor's `attach` or `follow` subscribes to
exactly what its descriptor names, including the Monitoring Appliance's
`ElectricalConnection` (`acMeasuredPhases` gives a per-phase value its meaning), the
Energy Guard's `DeviceConfiguration` and `ElectricalConnection` (the failsafe pair is
writable at the appliance too; the contractual maximum is what a §14a agreement sets), and
the EV guard's `permittedValueSet`, which changes mid-session when a car raises its minimum
current.

A value that has not arrived for a while comes either from a peer that stopped answering or
from one with nothing to say. What tells them apart is whether the notification comes on a
**clock**, and the descriptors carry that as data:

```rust
use eebus::model::{FeatureType, Function};
use eebus::usecases::descriptor::Delivery;
use eebus::usecases::{lpc, mpc};

mpc::MONITORING_APPLIANCE.delivery_of(&FeatureType::Measurement, &Function::MeasurementListData);
// Some(Delivery::OnChange) — a room holding its temperature sends nothing, and is fine

lpc::CONTROLLABLE_SYSTEM.delivery_of(&FeatureType::DeviceDiagnosis, &Function::DeviceDiagnosisHeartbeatData);
// Some(Delivery::Periodic(60 s)) — [LPC-005]; silence past this arms the failsafe
```

Heartbeats are the only functions these specifications put a clock on;
`periodic_functions()` lists them: 60 s for LPC, LPP and COB, 4 s for OPEV and OSCEV. The
period is the specification's "at least every", not a tolerance: how many missed beats to
allow is the consumer's choice (LPC allows two, OPEV none). Everything else is `OnChange`,
so its age says nothing about the peer.

## Addressing a peer's data

Almost every list in SPINE is keyed by an identifier the **device** chooses; the
specifications write them as placeholders: `<l1#(1..1)>` for a load-control limit,
`<k1#(1..1)>` for a configuration key, `<p1#(1..1)>` for an electrical-connection parameter.
"SHALL be used as the primary identifier" means the device keeps *its own* number stable,
not that a peer may assume one.

What the specifications fix is what each entry **describes** — a key's `keyName`, a
parameter's `scopeType` and phases, a limit's type, category and direction — and every such
list has a description function beside it. Address a peer's entries by reading that
description:

```rust
use eebus::model::DeviceConfigurationKeyName;
use eebus::usecases::addressing::KeyIds;

let mut keys = KeyIds::new();
keys.learn(&description_payload);              // keyId ⇄ keyName

let failsafe = keys.get(&DeviceConfigurationKeyName::FailsafeDurationMinimum);
let what_is_it = keys.name_of(some_key_from_a_value_list);
```

A wrong identifier produces no error: the write is well-formed, names a real entry, and is
applied and acknowledged to the wrong value.

Each use case wraps the resolver its data needs — `limitation::PeerIds`,
`mgcp::Curtailment`, `charging::PhaseLimits`, `evcc::EvReader` — and the actors use it. An
application only has to let the actor read the descriptions before expecting it to write;
`EnergyGuardActor::is_ready` reports when it has.

## What is implemented

| Use case | Actors | Specification |
|---|---|---|
| **LPC** — Limitation of Power Consumption | Controllable System, Energy Guard | UC TS 1.0.0 + IG 1.1.0 |
| **LPP** — Limitation of Power Production | Controllable System, Energy Guard | UC TS 1.0.0 + IG 1.1.0 |
| **MPC** — Monitoring of Power Consumption | Monitored Unit, Monitoring Appliance | UC TS 1.0.0 |
| **MGCP** — Monitoring of Grid Connection Point | Monitored Unit, Monitoring Appliance | UC TS 1.0.0 |
| **EVSECC** — EVSE Commissioning and Configuration | EVSE, CEM | EVSECC 1.0.1 |
| **EVCC** — EV Commissioning and Configuration | EV, CEM | EVCC 1.0.1 |
| **OPEV** — Overload Protection by EV Charging Current Curtailment | EV, CEM | OPEV 1.0.1b |
| **OSCEV** — Optimization of Self-Consumption During EV Charging | EV, CEM | OSCEV 1.0.1b |
| **EVCEM** — EV Charging Electricity Measurement | EV, Energy Guard | EVCEM 1.0.1 |
| **EVSOC** — EV State of Charge | EV, Monitoring Appliance | EVSOC 1.0.0 |
| **EVCS** — EV Charging Summary | EVSE, Energy Broker (or CEM) | EVCS 1.0.1 |
| **MOI** — Monitoring of Inverter | Inverter, Monitoring Appliance | MOI 1.0.0 |
| **MPS** — Monitoring of PV String | PVString, Monitoring Appliance | MPS 1.0.0 |
| **MOB** — Monitoring of Battery | Battery, Monitoring Appliance | MOB 1.0.0 |
| **COB** — Control of Battery | Inverter, CEM | COB 1.0.0 |
| **MDSF** — Monitoring of DHW System Function | DHW Circuit, Monitoring Appliance | MDSF 1.0.0 |
| **CDSF** — Configuration of DHW System Function | DHW Circuit, Configuration Appliance | CDSF 1.0.0 |
| **MDT** — Monitoring of DHW Temperature | DHW Circuit, Monitoring Appliance | MDT 1.0.0 |
| **CDT** — Configuration of DHW Temperature | DHW Circuit, Configuration Appliance | CDT 1.0.0 |
| **MRHSF** — Monitoring of Room Heating System Function | HVAC Room, Monitoring Appliance | MRHSF 1.0.0 |
| **CRHSF** — Configuration of Room Heating System Function | HVAC Room, Configuration Appliance | CRHSF 1.0.0 |
| **MRCSF** — Monitoring of Room Cooling System Function | HVAC Room, Monitoring Appliance | MRCSF 1.0.0 |
| **CRCSF** — Configuration of Room Cooling System Function | HVAC Room, Configuration Appliance | CRCSF 1.0.0 |
| **MRT** — Monitoring of Room Temperature | HVAC Room, Monitoring Appliance | MRT 1.0.0 |
| **CRHT** — Configuration of Room Heating Temperature | HVAC Room, Configuration Appliance | CRHT 1.0.0 |
| **CRCT** — Configuration of Room Cooling Temperature | HVAC Room, Configuration Appliance | CRCT 1.0.0 |
| **MOT** — Monitoring of Outdoor Temperature | Outdoor Temperature Sensor, Monitoring Appliance | MOT 1.0.0 |
| **OHPCF** — Optimization of Self-Consumption by Heat Pump Compressor Flexibility | Compressor, CEM | OHPCF 1.0.0 |

The first four are certifiable since July 2026. Every use case is implemented on **both**
sides, the appliance's and the manager's.

The twelve HVAC rows are the complete contents of the two HVAC specification bundles,
`EEBUS_HVAC_SystemFunction_UseCases_V1.0.0` and `EEBUS_HVAC_Temperature_UseCases_V1.0.0`.

Not implemented: **CEVC** (Coordinated EV Charging — see
[Not here: CEVC](@/docs/e-mobility.md#not-here-cevc)) and **ITPCM** (Incentive Table Based
Power Consumption Management), both a price signal over `incentiveTable` that an appliance
plans against. No device in the eight-device corpus announces either; the SMA Home Manager 2
announces [OHPCF](https://docs.rs/eebus/latest/eebus/usecases/ohpcf/index.html), the
direct-control alternative.

## The four levers that can ask for *more*

The grid, e-mobility and storage use cases can only ask an appliance to do less (see
[Heat pumps](@/docs/hot-water.md)). Four levers ask for more:

* **CDT / CRHT / CRCT** raise a **setpoint** and leave the appliance's own controller to
  decide. A tank already at temperature does not run for a higher setpoint.
* **CDSF / CRHSF / CRCSF** change the **operation mode** — `eco` while the grid is
  expensive, `on` while it is not. Slower, and available only where the appliance says
  `isOperationModeIdChangeable`.
* **CDSF scenario 2** starts a **one-time hot water loading** outright; scenario 3 stops it.
  See
  [Changing the mode, and pressing the button](@/docs/hot-water.md#changing-the-mode-and-pressing-the-button).
* **OHPCF** **starts a process**: the compressor's optional power consumption, at a time
  the CEM names, with stop, pause and resume afterwards — e.g. pre-heating while PV is
  exporting. See
  [The other lever: OHPCF](@/docs/hot-water.md#the-other-lever-ohpcf).

## Two use cases, one implementation

LPC and LPP are the same use case in opposite directions: the same four scenarios, table
numbers and thirteen state transitions, with requirement identifiers differing only in the
prefix. The state machine and both actors are written once and pointed by a `Direction`:

```rust
ControllableSystemActor::builder(system, lpc::DIRECTION, features).install(&mut engine, now)
ControllableSystemActor::builder(system, lpp::DIRECTION, features).install(&mut engine, now)
```

`features` is a [`CsFeatures`](https://docs.rs/eebus/latest/eebus/usecases/limitation/struct.CsFeatures.html)
with named fields rather than positional `FeatureAddress` arguments, so features cannot be
swapped by order.

`install` publishes the limit description and locks the two features to one binding partner,
and it is the only way to obtain an actor. Without it, a device looks healthy but its empty
limit list gives an Energy Guard no `limitId` to write to.

The same sharing runs elsewhere:

* **MPC and MGCP** are the same measurements named from the appliance's side or the grid's.
* **OPEV and OSCEV** are the same per-phase current ceiling for opposite reasons, pointed
  by a `Purpose`: `obligation`/`overloadProtection` against
  `recommendation`/`selfConsumption`.
* **EVCEM, EVSOC, MOI, MPS, MOB, COB, MDT, MRT and MOT** are built on the same
  `monitoring` machinery as MPC and MGCP, serving eleven use cases; each adds vocabulary,
  not mechanism.
* **The twelve HVAC use cases are three exchanges.** `hvac::system_function` is the
  operation-mode one and serves six — MDSF/CDSF, MRHSF/CRHSF, MRCSF/CRCSF — told apart by
  `systemFunctionType` (`dhw`, `heating`, `cooling`) and by whether the actor may write.
  `hvac::setpoint` is the temperature-setting one and serves three — CDT, CRHT, CRCT.
  `hvac::temperature` is the measurement one and serves three — MDT, MRT, MOT. Twelve
  modules of constants and descriptors sit on top.
* **MDT, MRT and MOT** share a `locate` of their own for the one thing they do not share
  with MPC: a tank, a room and the outdoors have no `ElectricalConnection`.
* **The other nine share `hvac::peer`.** One `locate`, one `locate_all` and one `follow`,
  told apart by the use-case descriptor each passes in, so `follow` reads the
  specification's scenario tables. See
  [Heat pumps](@/docs/hot-water.md).
* **And one actor for all nine.** `HvacApplianceActor` serves both client actors of the
  family — the Monitoring Appliance reads, the Configuration Appliance also writes — keyed
  by `UnitId`, device **and** entity — the same key `MonitoringApplianceActor` uses,
  because a room's thermometer and its setpoint are the same entity.
