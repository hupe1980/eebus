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
has gone somewhere else. Nine such defects were fixed in 0.5.0 and every one of them was
invisible to a test whose other end was this same crate.

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
| **MOI** — Monitoring of Inverter | Inverter, Monitoring Appliance | MOI 1.0.0 |
| **MPS** — Monitoring of PV String | PVString, Monitoring Appliance | MPS 1.0.0 |
| **MOB** — Monitoring of Battery | Battery, Monitoring Appliance | MOB 1.0.0 |
| **COB** — Control of Battery | Inverter, CEM | COB 1.0.0 |
| **CDT** — Configuration of DHW Temperature | DHW Circuit, Configuration Appliance | CDT 1.0.0 |
| **MDSF** — Monitoring of DHW System Function | DHW Circuit, Monitoring Appliance | MDSF 1.0.0 |
| **MDT** — Monitoring of DHW Temperature | DHW Circuit, Monitoring Appliance | MDT 1.0.0 |

The first four are the ones certifiable since July 2026. Every one of them is implemented
on **both** sides — the appliance's and the manager's — which is where most of the
implementation guides' pages actually go.

Not implemented: **CEVC** (Coordinated EV Charging), which is a different shape from
everything above — three actors, power sequences, incentive tables, and a charging plan
negotiated rather than a ceiling imposed.

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
* **EVCEM, EVSOC, MOI, MPS, MOB, COB and MDT** are built on the same `monitoring`
  machinery as MPC and MGCP — one implementation of "describe a measurement twice and read
  it back", serving nine use cases. What each adds is vocabulary, not mechanism.

The reference implementations duplicate each pair; here a fix to one is a fix to both, and
the only thing the direction changes is what the tests assert.
