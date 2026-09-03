+++
title = "Getting started"
description = "Add the eebus crate, run the §14a exchange between a grid control box and a heat pump against a virtual clock, then over a real TLS socket."
weight = 20
[extra]
group = "Start"
+++

## Install

The crate is not on crates.io yet. Take it from git:

```toml
[dependencies]
eebus = { git = "https://github.com/hupe1980/eebus" }
```

Default features are `std`, `pairing` and `conformance`. Sockets, TLS and certificates are
opt-in — see
[Feature flags](@/docs/architecture.md#feature-flags). Minimum supported Rust version is
**1.88**.

Anything that touches TLS also needs a cryptography provider, and the crate does not pick
one for you: add `ring` or `aws-lc-rs`, exactly one, or the build stops with an explanation.
The choice is process-global in `rustls`, so it belongs to whoever builds the binary — see
[the security model](@/docs/security.md#the-cryptography-provider-is-the-consumer-s-choice).

```toml
eebus = { git = "https://github.com/hupe1980/eebus", features = ["runtime", "ring"] }
```

## Run the exchange

Two examples play out the whole stack, and between them they are the fastest way to
understand it.

```sh
git clone https://github.com/hupe1980/eebus && cd eebus
cargo run --example grid_limit                          # against a virtual clock
cargo run --example networked --features runtime,ring   # over a real socket
```

Two more — `steuerbox` and `heat_pump` — are the same exchange as separate programs on a
real network; see [On a network](@/docs/networking.md#the-two-simulators).

`grid_limit` plays out §14a in miniature — handshake, discovery, binding, a 3 kW limit,
its acknowledgement, and the failsafe taking over when the control box goes quiet. Every
datagram crosses the SHIP framing and the JSON-UTF8 codec; nothing crosses a socket.

```text
1. SHIP handshake complete after 9 frames
   version 1.1, format JSON-UTF8
3. Discovery found the heat pump
   plays     limitationOfPowerConsumption as ControllableSystem
   scenarios [1, 2, 3, 4]
4. Bound as the Energy Guard
   the heat pump takes limits from entity 1 feature 1
5. Heartbeat, then a 3000 W limit
6. The heat pump is now in Limited
   limited to Active(3000.0)
   the guard recorded 3000 W accepted — the §14a evidence
7. After two minutes of silence: FailsafeState
   limited to Failsafe(4200.0)
```

`networked` does the same thing over loopback: two nodes generate certificates, approve
each other's SKI, complete TLS 1.2 with mutual authentication and the SHIP handshake, then
discover, bind and exchange a limit.

## Build a device

A SPINE device is a tree: the device holds entities, an entity holds features, a feature
declares which functions it supports and what may be done to them. Entity 0 is created for
you and carries NodeManagement, the feature every peer talks to first.

```rust
use eebus::model::{DeviceType, EntityType, FeatureType, Function, Role};
use eebus::spine::{LocalDevice, LocalEntity, LocalFeature, Operations};

let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)?;

let mut appliance = LocalEntity::new([1], EntityType::HeatPumpAppliance);
appliance.add_feature(
    LocalFeature::new(1, FeatureType::LoadControl, Role::Server)
        .with_function(Function::LoadControlLimitListData, Operations::read_write()),
)?;
device.add_entity(appliance)?;
```

## Drive the engine

The SPINE engine owns no socket and reads no clock. You hand it bytes and a timestamp, and
ask it what it wants to do:

```rust
engine.handle_datagram(&datagram, now);   // something arrived
engine.handle_timeout(now);               // a timer fired

engine.poll_transmit();                   // what to send
engine.poll_timeout();                    // when to come back
engine.poll_event();                      // what the application should know
```

That is the whole shape of it, and the SHIP handshake works the same way. See
[Architecture](@/docs/architecture.md) for why, and [On a network](@/docs/networking.md)
for the `Hub` that supplies the sockets and the clock when you would rather not.

## Answer a limit

Under §14a EnWG the acknowledgement is the record that the limit was applied, so a write
is answered by the use case rather than by a generic setter — only the application knows
what the appliance actually did.

```rust
use eebus::spine::SpineEvent;
use eebus::usecases::limitation::{self, WriteOutcome};

if let Some(SpineEvent::WriteRequested(w)) = engine.poll_event() {
    // `resolved` is the update merged into what is stored; `data` is what arrived.
    let write = limitation::read_limit_write(&w.resolved)?;
    let outcome = system.on_limit_write(&write, decide(&write), now);
    if outcome.is_accepted() {
        engine.accept_write(w.token, now)?;                          // ACK — or what the peer was told instead
    } else {
        engine.reject_write(w.token, outcome.error_number(), now);   // NACK
    }
}
```

## Where to go next

* [What EEBUS is](@/docs/introduction.md) — the standard, if you have not met it.
* [LPC and LPP](@/docs/limitation.md) — the §14a use case, both actors.
* [On a network](@/docs/networking.md) — sockets, mDNS and reconnection.
* [Certification](@/docs/certification.md) — the laboratory's test cases, and coverage.
