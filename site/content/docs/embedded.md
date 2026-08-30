+++
title = "Embedded targets"
description = "Building eebus for no_std + alloc on a Cortex-M4F: what is available without the standard library, and what the sans-IO core buys on a controller."
weight = 120
[extra]
group = "Deployment"
+++

Everything below `runtime` builds for `no_std + alloc`. CI checks it against
`thumbv7em-none-eabihf` — a Cortex-M4F, the class of part a heat-pump controller actually
uses.

```sh
cargo build --no-default-features --target thumbv7em-none-eabihf
```

## What you get

The codec, the model, the SHIP framing and handshake, the SPINE engine and every use case.
That is the whole protocol; what you supply is the socket and the clock.

## What you supply

The sans-IO shape is what makes this work. You own the TCP stack — smoltcp, a vendor
library, whatever the part provides — and you own the timer. The core asks for both through
its ordinary interface:

```rust
engine.handle_datagram(&datagram, now);   // bytes you read from your stack
while let Some(out) = engine.poll_transmit() { /* write it back */ }
let deadline = engine.poll_timeout();     // arm your timer for this
```

`now` is whatever monotonic instant your platform offers.

## What is not available

`std` brings in `serde/std`, `serde_json/std` and `thiserror/std`; without it, errors
implement `core::fmt::Display` rather than `std::error::Error`. TLS is not available in
`no_std` — `rustls` needs `std` — so on a controller you supply a TLS implementation from
your platform's stack and hand the crate the plaintext.

## Why the fuzzing matters here

Five `cargo fuzz` targets cover everything reachable from the network: SHIP framing, the
JSON codec, the QR payload, the TXT record, and a whole datagram through the engine. On a
server a panic is a stack trace. On a heat-pump controller a panic is a reboot, and a
reboot during a grid event is exactly what §14a exists to prevent.
