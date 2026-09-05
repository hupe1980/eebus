+++
title = "Use cases"
description = "What an EEBUS use case is, how a device publishes the ones it plays, and how a peer discovers them before exchanging anything."
weight = 70
[extra]
group = "Use cases"
+++

A use case is the application layer of EEBUS. It names the **actors** involved, the
numbered **scenarios** they run, and the SPINE features and functions each scenario needs.
Claiming to play one is a commitment: a peer will expect every scenario you advertise.

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
each. Only then does an application know whether the peer is a heat pump it may limit or a
display that merely watches.

## Who may write, and who may not

Whether a write needs a binding is a property of the **feature**, not a rule of the protocol
(SPINE §7.3), and the use cases do not agree about their own:

| | |
|---|---|
| **binds** | LPC/LPP, OPEV/OSCEV, COB, EVCS, OHPCF scenario 2 — "Actors that write parts of a Feature within this Scenario need to create a binding [...] Only one binding partner is allowed" |
| **does not** | every HVAC use case, including all six that write — "Binding SHOULD NOT be used for this Scenario" |

So the feature constructors carry it, and the default is `Required`:

```rust
limitation::load_control_feature(1);   // WriteBinding::Required
cdt::setpoint_feature(1);              // WriteBinding::NotRequired — CDT §3.4.1.1
```

What replaces it on an HVAC feature is the application's own decision: those writes are
deferred, so a product that wants "only the manager I was commissioned with" enforces it
where it can see who is asking. See [Bindings and subscriptions](@/docs/spine.md).

## Who has to subscribe, and what silence means

Subscription, unlike binding, the use cases *do* agree about. Every scenario of every one
of them says the same sentence — "Actors SHALL create a subscription for each server
Feature that is relevant for the corresponding Actor within this Scenario" (§3.4.n.1) — and
§3.3.4 names polling only as the fallback for a subscription that was refused. So the
descriptor derives the list rather than each actor keeping its own:

```rust
mpc::MONITORING_APPLIANCE.features_needing_subscription();
// [ElectricalConnection, Measurement] — five scenarios, two features, each named once
```

`tests/use_case_delivery.rs` holds every actor to it: what an actor's `attach` or `follow`
asks to subscribe to is exactly what its descriptor names. The features easiest to leave out
are the ones that check catches, and none of them is a formality — the Monitoring Appliance's
`ElectricalConnection`, where `acMeasuredPhases` says what a per-phase value *means*; the
Energy Guard's `DeviceConfiguration` and `ElectricalConnection`, where the failsafe pair is
writable at the appliance too and the contractual maximum is what a §14a agreement sets; and
the EV guard's `permittedValueSet`, which changes mid-session when a car raises its minimum
current.

That leaves the question a consumer actually has to answer, which is what to make of a
value that has not arrived for ten minutes. Since everything is subscribed, the distinction
is not notification-versus-poll but whether the notification comes on a **clock**:

```rust
use eebus::usecases::descriptor::Delivery;

mpc::MONITORING_APPLIANCE.delivery_of(&FeatureType::Measurement, &Function::MeasurementListData);
// Some(Delivery::OnChange) — a room holding its temperature sends nothing, and is fine

lpc::CONTROLLABLE_SYSTEM.delivery_of(&FeatureType::DeviceDiagnosis, &Function::DeviceDiagnosisHeartbeatData);
// Some(Delivery::Periodic(60 s)) — [LPC-005]; silence past this arms the failsafe
```

The heartbeats are the only functions any of these specifications puts a clock on: 60 s for
LPC, LPP and COB, and 4 s for OPEV and OSCEV, because a car follows a current at once. The
period is the specification's "at least every", not a tolerance — how many missed beats to
allow stays the consumer's, and LPC allows two where OPEV allows none.

## Addressing a peer's data

Knowing which feature to talk to is only half of it. Almost every list in SPINE is keyed by
an identifier the **device** chooses, and the specifications write those identifiers as
placeholders: `<l1#(1..1)>` for a load-control limit, `<k1#(1..1)>` for a configuration
key, `<p1#(1..1)>` for an electrical-connection parameter. "SHALL be used as the primary
identifier" is a promise that the device keeps *its own* number stable. It is not a number
a peer may assume.

What the specifications fix instead is what each entry **describes** — a key's `keyName`, a
parameter's `scopeType` and phases, a limit's type and category and direction — and every
such list has a description function beside it. Reading that description is the only way to
address the peer rather than a coincidence:

```rust
use eebus::model::DeviceConfigurationKeyName;
use eebus::usecases::addressing::KeyIds;

let mut keys = KeyIds::new();
keys.learn(&description_payload);              // keyId ⇄ keyName

let failsafe = keys.get(&DeviceConfigurationKeyName::FailsafeDurationMinimum);
let what_is_it = keys.name_of(some_key_from_a_value_list);
```

This is the one class of protocol error that produces no error. The write is well-formed,
it names a real entry of the peer's, the peer applies it and acknowledges it, and the value
has gone somewhere else — invisible to any test whose other end is this same crate.

Each use case wraps the resolver its own data needs — `limitation::PeerIds`,
`mgcp::Curtailment`, `charging::PhaseLimits`, `evcc::EvReader` — and the actors do it for
you. What is left for an application is to *let* the actor read the descriptions before
expecting it to write anything, which is what `EnergyGuardActor::is_ready` reports.

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

The first four are the ones certifiable since July 2026. Every one of them is implemented
on **both** sides — the appliance's and the manager's — which is where most of the
implementation guides' pages actually go.

The twelve HVAC rows are the **complete** contents of the two HVAC specification bundles —
`EEBUS_HVAC_SystemFunction_UseCases_V1.0.0` and `EEBUS_HVAC_Temperature_UseCases_V1.0.0`.
Nothing in either is left out.

Not implemented: **CEVC** (Coordinated EV Charging), which is a different shape from
everything above — three actors, power sequences, incentive tables, and a charging plan
negotiated rather than a ceiling imposed. **ITPCM** (Incentive Table Based Power
Consumption Management) is the other absence, and it is the same shape: a price signal over
`incentiveTable` that an appliance plans against, rather than an instruction it follows.
Nothing in the eight-device corpus announces either; the SMA Home Manager 2 announces
[OHPCF](https://docs.rs/eebus/latest/eebus/usecases/ohpcf/index.html), which is the
direct-control answer to the same question and is implemented.

## The four levers that can ask for *more*

Every use case in the grid, e-mobility and storage rows can only ask an appliance to do
less, and a ceiling an appliance is already under changes nothing at all — so a manager
holding only those can never spend a surplus. Four can ask for more, and they are not
interchangeable:

* **CDT / CRHT / CRCT** raise a **setpoint** and leave the appliance's own controller to
  decide what to do about it. A tank already at temperature will not run for a higher
  setpoint.
* **CDSF / CRHSF / CRCSF** change the **operation mode** — `eco` while the grid is
  expensive, `on` while it is not. Slower, and available only where the appliance says
  `isOperationModeIdChangeable`.
* **CDSF scenario 2** starts a **one-time hot water loading** outright: the button in the
  bathroom, pressed over the wire. The shortest path there is from "the roof is exporting"
  to "the tank is absorbing it", and scenario 3 gives it back when a cloud arrives.
* **OHPCF** **starts a process**: the compressor's optional power consumption, at a time
  the CEM names, with stop, pause and resume afterwards. It is what makes a heat pump
  pre-heat while the roof is exporting — the one thing a thermal model exists to do, and
  the thing no limit can express.

`ohpcf` is one function on one feature, and what a payload *means* is which of four phases
the compressor is in: an offer (`inactive`, no schedule), a process (`scheduled`, `running`
or `paused`), an ending (`completed` or `invalid`), or nothing (no `alternatives`). The
compressor refuses what the specification does not allow — a pause on something not running,
a stop it never announced as stoppable — before acknowledging it, the shape LPC's
Controllable System established.

## Two use cases, one implementation

LPC and LPP are the same use case pointed in opposite directions: the same four scenarios,
the same table numbers, the same thirteen state transitions, with requirement identifiers
differing only in the prefix. So the state machine and both actors are written once and
pointed by a `Direction`:

```rust
ControllableSystemActor::builder(system, lpc::DIRECTION, features).install(&mut engine, now)
ControllableSystemActor::builder(system, lpp::DIRECTION, features).install(&mut engine, now)
```

`features` is a [`CsFeatures`](https://docs.rs/eebus/latest/eebus/usecases/limitation/struct.CsFeatures.html)
with named fields rather than a row of `FeatureAddress` arguments, because nothing but the
order would say which is which — and a device that answers a grid operator's limit write on
its heartbeat feature is a device that compiles.

`install` publishes the limit description and locks the two features to one binding partner,
and it is the *only* way to obtain an actor. Skipping it leaves a device that looks perfectly
healthy — discovery answered, bindings and subscription granted, heartbeats flowing — whose
empty limit list gives an Energy Guard no `limitId` to write to.

Three more pairs work the same way:

* **MPC and MGCP** are the same measurements named from the appliance's side or the grid's.
* **OPEV and OSCEV** are the same per-phase current ceiling for opposite reasons, pointed
  by a `Purpose`: `obligation`/`overloadProtection` against
  `recommendation`/`selfConsumption`. Two words, and a car that confuses them charges on
  solar only.
* **EVCEM, EVSOC, MOI, MPS, MOB, COB, MDT, MRT and MOT** are built on the same
  `monitoring` machinery as MPC and MGCP — one implementation of "describe a measurement
  twice and read it back", serving eleven use cases. What each adds is vocabulary, not
  mechanism.
* **The twelve HVAC use cases are three exchanges.** `hvac::system_function` is the
  operation-mode one and serves six — MDSF/CDSF, MRHSF/CRHSF, MRCSF/CRCSF — told apart by
  `systemFunctionType` (`dhw`, `heating`, `cooling`) and by whether the actor may write.
  `hvac::setpoint` is the temperature-setting one and serves three — CDT, CRHT, CRCT.
  `hvac::temperature` is the measurement one and serves three — MDT, MRT, MOT. Twelve
  modules of constants and descriptors sit on top; the behaviour is written once.
* **MDT, MRT and MOT** share a `locate` of their own for the one thing they do not share
  with MPC: a tank, a room and the outdoors have no `ElectricalConnection`, so searching for
  one would find whatever else the device happens to serve.
* **The other nine share `hvac::peer`.** One `locate`, one `locate_all` and one `follow`,
  told apart by the use-case descriptor each passes in — so what `follow` reads is the
  specification's own scenario tables rather than a second list kept beside them. See
  [Heat pumps](@/docs/hot-water.md).
* **And one actor for all nine.** `HvacApplianceActor` serves both client actors of the
  family — the Monitoring Appliance reads, the Configuration Appliance also writes — keyed
  by `UnitId`, device **and** entity. That is the same key `MonitoringApplianceActor` uses,
  because a room's thermometer and its setpoint are the same entity.

The reference implementations duplicate each pair; here a fix to one is a fix to both, and
the only thing the direction changes is what the tests assert.
