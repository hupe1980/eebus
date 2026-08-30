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

## What is implemented

| Use case | Actors | Specification |
|---|---|---|
| **LPC** — Limitation of Power Consumption | Controllable System, Energy Guard | UC TS 1.0.0 + IG 1.1.0 |
| **LPP** — Limitation of Power Production | Controllable System, Energy Guard | UC TS 1.0.0 + IG 1.1.0 |
| **MPC** — Monitoring of Power Consumption | Monitored Unit, Monitoring Appliance | UC TS 1.0.0 |
| **MGCP** — Monitoring of Grid Connection Point | Monitored Unit, Monitoring Appliance | UC TS 1.0.0 |
| **EVSECC** — EVSE Commissioning and Configuration | EVSE, CEM | EVSECC 1.0.1 |
| **OPEV** — Overload Protection by EV Charging Current Curtailment | EV, CEM | OPEV 1.0.1b |

The first four are the ones certifiable since July 2026. Every one of them is implemented
on **both** sides — the appliance's and the manager's — which is where most of the
implementation guides' pages actually go.

## Two use cases, one implementation

LPC and LPP are the same use case pointed in opposite directions: the same four scenarios,
the same table numbers, the same thirteen state transitions, with requirement identifiers
differing only in the prefix. So the state machine and both actors are written once and
pointed by a `Direction`:

```rust
ControllableSystemActor::new(system, lpc::DIRECTION, load_control, config, diagnosis)
ControllableSystemActor::new(system, lpp::DIRECTION, load_control, config, diagnosis)
```

MPC and MGCP share their implementation the same way. The reference implementations
duplicate each pair; here a fix to one is a fix to both, and the only thing the direction
changes is what the tests assert.
