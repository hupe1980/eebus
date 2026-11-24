# eebus

[![crates.io](https://img.shields.io/crates/v/eebus.svg)](https://crates.io/crates/eebus)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

An unofficial Rust implementation of EEBUS for heat pumps, wallboxes, inverters and energy managers.
It implements the SHIP transport, the SPINE information model and both actors for 28 energy use cases.

[Getting started](https://hupe1980.github.io/eebus/docs/getting-started/) ·
[Use cases](https://hupe1980.github.io/eebus/docs/use-cases/) ·
[API reference](https://docs.rs/eebus) ·
[Changelog](https://github.com/hupe1980/eebus/blob/main/CHANGELOG.md)

## Install

Rust 1.88 or later is required. Choose the features for your target.

```sh
cargo add eebus --features full         # Tokio sockets, TLS with ring, and mDNS
cargo add eebus --no-default-features   # embedded protocol core with alloc
```

The default features are `std`, `pairing` and `conformance`. For a networked build with
the alternative TLS provider, use `--features runtime,mdns,aws-lc-rs`. TLS requires exactly
one provider. [Feature flags](https://hupe1980.github.io/eebus/docs/architecture/#feature-flags)

## Run a power limit exchange

From the repository root, run a control box and a heat pump against a virtual clock or over loopback.

```sh
cargo run --example grid_limit
cargo run --example networked --features runtime,ring
```

On an Energy Guard, request a limit as a nonnegative magnitude. This caps a 10 kW inverter
at 40% of rated output. The commissioned guard writes −4,000 W over SPINE.

```rust
guard.require(&inverter, Some(LimitWrite::active(4_000.0)), now)?;
```

A consumption guard writes +4,000 W for the same request. An active zero caps the selected
direction at zero. Passing `None` releases it. The guard reports the peer's acknowledgment
through `GuardEvent::LimitAccepted`. [LPC and LPP guide](https://hupe1980.github.io/eebus/docs/limitation/)

## Integrate

| Task | Guide |
|---|---|
| Pair devices, discover peers and run a network loop | [Networking](https://hupe1980.github.io/eebus/docs/networking/) |
| Embed SHIP and SPINE in your own event loop | [Architecture](https://hupe1980.github.io/eebus/docs/architecture/) · [Embedded targets](https://hupe1980.github.io/eebus/docs/embedded/) |
| Discover features and apply partial updates | [SPINE engine](https://hupe1980.github.io/eebus/docs/spine/) |
| Regenerate the model from the EEBUS schemas | [Code generation](https://hupe1980.github.io/eebus/docs/codegen/) |

## Conformance

The library exercises 189 of 203 LPC, LPP, MPC and MGCP abstract test cases. The remaining
14 require a device. The protocol-level tests cite 24/33 SHIP cases, 19/31 SPINE cases and
0/40 Pairing Service cases by identifier. The interoperability suite exchanges limits
with an independent `eebus-go` peer. [Coverage and test methods](https://hupe1980.github.io/eebus/docs/conformance/)

```sh
cargo test --workspace --features eebus/full
```

Pre-1.0 minor releases may break the Rust API.

## License

MIT or Apache-2.0, at your option. EEBUS® is a trademark of EEBus Initiative e.V.
This project is independent of the Initiative.
