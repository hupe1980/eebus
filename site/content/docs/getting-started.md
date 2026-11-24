+++
title = "Getting started"
description = "Add the eebus crate, run the §14a exchange between a grid control box and a heat pump against a virtual clock, then over a real TLS socket."
weight = 20
[extra]
group = "Start"
+++

## Install

```sh
cargo add eebus
```

Default features are `std`, `pairing` and `conformance`. Sockets, TLS and certificates are
opt-in — see [Feature flags](@/docs/architecture.md#feature-flags). Minimum supported Rust
version is **1.88**.

Anything that touches TLS also needs exactly one cryptography provider, `ring` or
`aws-lc-rs`; otherwise the build stops with an explanation. See
[the security model](@/docs/security.md#the-cryptography-provider-is-the-consumer-s-choice).

```sh
cargo add eebus --features runtime,ring
```

## Run the exchange

Two examples play out the whole stack.

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

The SHIP handshake works the same way. See [Architecture](@/docs/architecture.md), and
[On a network](@/docs/networking.md) for the `Hub` that supplies sockets and the clock.

## Answer a limit

Pass incoming events to the installed Controllable System actor. It reads the merged state of partial writes,
validates the signed wire value for the configured direction and calls your policy with
the local magnitude in watts. Return `LocalDecision::Apply` when the appliance can follow
the limit.

```rust
use eebus::usecases::limitation::CsEvent;

while let Some(event) = engine.poll_event() {
    if let Some(CsEvent::LimitDecided { write, outcome, .. }) =
        pump.handle_event_with(&mut engine, &event, now, decide)
    {
        println!("{} W, active={}: {outcome:?}", write.watts(), write.is_active);
    }
}
```

The actor sends the acknowledgment or refusal and records the outcome in `pump.audit()`.

## Where to go next

* [What EEBUS is](@/docs/introduction.md) — the standard, if you have not met it.
* [LPC and LPP](@/docs/limitation.md) — the §14a use case, both actors.
* [On a network](@/docs/networking.md) — sockets, mDNS and reconnection.
* [Certification](@/docs/certification.md) — the laboratory's test cases, and coverage.
