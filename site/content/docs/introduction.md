+++
title = "What EEBUS is"
description = "EEBUS explained from the ground up: the actors, the four-layer stack of SHIP and SPINE, and why §14a EnWG made the grid use cases certifiable in 2026."
weight = 10
[extra]
group = "Start"
+++

EEBUS is the open standard by which appliances in a building — a heat pump, a wallbox, a
battery, a photovoltaic inverter — talk to an energy manager, and by which that energy
manager talks to the distribution grid operator. It is maintained by the EEBus Initiative
e.V., and it is the interface a growing amount of European energy regulation now assumes
exists.

## Why it became mandatory reading

Two German rules did most of the work.

**§14a EnWG** obliges the operator of a congested low-voltage grid to be able to reduce
the draw of a *controllable consumption device* — a heat pump, a wallbox, a storage
heater — rather than disconnect the street. Since 2024 every new such device must accept a
limit, and the fallback is not a suggestion: 4.2 kW is the floor an installation must keep
available.

**§9 EEG** is the mirror image on the production side: a photovoltaic plant must be able to
have its feed-in curtailed.

A limit means nothing without evidence that it was honoured, so the acknowledgement an
appliance sends back is a regulatory artefact, not a protocol nicety. That single fact
shapes more of this crate's API than any other.

**Since July 2026** four use cases are formally certifiable: limitation of power
consumption and production, and monitoring of power consumption and of the grid connection
point. Certification means a laboratory runs a published test suite against your device.

## The actors

EEBUS describes participants by the *role* they play, not by what they are:

| Role | Who plays it | What it does |
|---|---|---|
| Controllable System | heat pump, wallbox, battery | accepts limits and reports what it did |
| Energy Guard | grid operator's control box, or an energy manager acting for one | sends limits, and is answerable for them |
| Monitoring Appliance | energy manager, display, gateway | reads measurements and their meaning |
| Monitored Unit | anything with a meter | publishes measurements |

A single physical box is usually several roles at once: a home energy manager is an Energy
Guard toward the heat pump and a Controllable System toward the grid operator.

## The layers

EEBUS is not one protocol but a stack of four documents, and this crate implements the
whole of it.

```text
┌──────────────────────────────────────────────────────────┐
│  Use cases   LPC · LPP · MPC · MGCP · EVSECC · OPEV       │  what the device is for
├──────────────────────────────────────────────────────────┤
│  SPINE       device model, discovery, bindings,           │  what the device says
│              subscriptions, partial reads and writes      │
├──────────────────────────────────────────────────────────┤
│  SHIP        mDNS-SD, TLS 1.2, WebSocket, handshake,      │  how the bytes travel
│              trust establishment by SKI                   │
├──────────────────────────────────────────────────────────┤
│  Transport   TCP/IP over the building's LAN               │
└──────────────────────────────────────────────────────────┘
```

**SHIP** — *Smart Home IP* — is the transport. It finds peers with mDNS-SD, connects over
TLS 1.2 with mutual authentication, upgrades to a WebSocket, and runs a five-phase
handshake that establishes trust and agrees on a message format. Identity is the
certificate: a peer *is* its Subject Key Identifier, a 20-byte hash of its public key.
There are no usernames and no central authority.

**SPINE** — *Smart Premises Interoperable Neutral-message Exchange* — is everything above
that. It defines an information model (devices contain entities, entities contain features,
features hold functions) and a protocol for reading, writing, subscribing to and
discovering that model.

**Use cases** are the application layer: named bundles of scenarios that say which features
a device must have and which messages it must exchange to claim it plays a role.

## The vocabulary you will keep meeting

**SKI** — Subject Key Identifier. Twenty bytes, hex-encoded, taken from a node's
certificate. It is the node's name and the whole basis of trust: two devices are paired
when each has recorded the other's SKI.

**Feature address** — where in the model a thing lives: `device / entity / feature`. Written
as `d:_i:46925_HeatPump-1` with an entity path like `[1]` and a feature id.

**Function** — a named piece of data on a feature, such as `loadControlLimitListData`. What
you actually read, write and subscribe to.

**Binding** vs **subscription** — a binding grants the right to *write*; a subscription asks
to be *notified*. LPC requires both, in that order.

**Scenario** — one numbered exchange within a use case. LPC has four: the limit, the
failsafe values, the heartbeat, and the constraints.

## Prior art

EEBUS has implementations in several languages, and this crate was written after reading
them.

| Project | Language | What it is |
|---|---|---|
| [`enbility/ship-go`, `spine-go`, `eebus-go`](https://github.com/enbility/eebus-go) | Go | The de-facto reference, used by [evcc](https://docs.evcc.io/en/features/external-control/) and by several vendors. |
| [`openmuc/jEEBus`](https://www.openmuc.org/eebus/ship/) | Java | An XSD-generated model with fluent builders — the generated-model idea is the one worth copying. |
| [`NIBEGroup/openeebus`](https://github.com/NIBEGroup/openeebus) | C | The grid four, one remote connection at a time: the constrained-device niche. |
| [`DerAndereAndi/eebus-rust`](https://github.com/DerAndereAndi/eebus-rust) | Rust | An early experiment — the client handshake only. |
| [`enbility/devices`](https://github.com/enbility/devices) | data | Captured discovery responses from real devices. |

## Where this crate fits

It differs less in what it covers — the whole stack, both sides of every use case — than in
how it is built: a protocol core with no sockets and no clock, a model generated from the
specifications' own XML Schemas, and a test suite that carries the certification's test
identifiers. Where it reaches a different answer from the reference implementations, the
sentence of the specification driving the difference is named under
[Conformance](@/docs/conformance.md).

What it does *not* set out to do is worth reading before you commit to it:
[Scope and non-goals](@/docs/scope.md).

## Further reading

* EEBus Initiative — [Specifications & media](https://www.eebus.org/specifications-media/),
  [certification launch](https://www.eebus.org/eebus-certification-launch/)
* Sans-IO as a design — [Firezone's write-up](https://www.firezone.dev/blog/sans-io),
  [`quinn-proto`](https://docs.rs/quinn-proto), [`str0m`](https://github.com/algesten/str0m)

Next: [Getting started](@/docs/getting-started.md).
