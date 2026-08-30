+++
title = "LPC and LPP"
description = "Limitation of Power Consumption and Production: the §14a EnWG use case, its four scenarios, the failsafe state machine, and both actors including the Energy Guard's write rules."
weight = 80
[extra]
group = "Use cases"
+++

**Limitation of Power Consumption** is how a grid operator keeps a congested low-voltage
grid intact without disconnecting anyone: an Energy Guard tells a Controllable System how
much power it may draw, and the system honours it. **Limitation of Power Production** is
the same thing for feed-in. Together they are the technical basis for §14a EnWG and §9 EEG.

## The four scenarios

1. **Control the active power limit** — the limit itself, with an optional duration,
   acknowledged or refused.
2. **Failsafe values** — what applies when the Energy Guard falls silent, and for how long
   at minimum.
3. **Heartbeat** — a message every sixty seconds in each direction; its absence is what
   triggers the failsafe.
4. **Constraints** — the nominal maxima, so the Energy Guard knows what it is limiting.

## The state machine

A Controllable System is in exactly one of three states, and the transitions between them
are the use case:

```text
        a limit arrives                 the limit expires
   ┌──────────────────────▶ Limited ──────────────────────┐
   │                          │                           ▼
Normal ◀───────────────────────┘                       Normal
   │                    guard heard from
   │  guard silent for the heartbeat timeout
   ▼
FailsafeState  ── guard heard from again ──▶  Normal
```

The failsafe is not optional and not a suggestion. §14a fixes 4.2 kW as the floor an
installation must keep available, and the failsafe duration has a minimum so that a brief
network hiccup does not leave a house heating at full power for hours.

```rust
use core::time::Duration;
use eebus::usecases::limitation::{ControllableSystem, CsConfig};

// A heat pump that falls back to the 4.2 kW §14a leaves it, for at least two hours.
let system = ControllableSystem::new(
    CsConfig::new(4_200.0, Duration::from_secs(2 * 3_600)).with_nominal_max(11_000.0),
    Duration::ZERO,
);
```

## The application decides

A limit write is answered by the use case, not by a generic setter. Under §14a the
acknowledgement *is* the record that the limit was applied, so it has to say what the
appliance actually did — and only the appliance knows that.

```rust
match engine.poll_event() {
    Some(SpineEvent::WriteRequested(w)) => {
        let write = limitation::read_limit_write(&w.resolved)?;   // merged, not the fragment
        match system.on_limit_write(&write, decide(&write), now) {
            WriteOutcome::Accepted => engine.accept_write(w.token, now),           // ACK
            outcome => engine.reject_write(w.token, outcome.error_number(), now),  // NACK
        }
    }
    _ => {}
}
```

## The other half: the Energy Guard

The 2026 implementation guides spend most of their pages on the controlling side, and its
rules are unforgiving. Here they live behind one call:

```rust
guard.require(&device, Some(LimitWrite::active(4_200.0)), now);
```

Behind that line:

* a heartbeat immediately before the write, and only once the peer has subscribed to it
  (§2.11);
* never a deactivation as the first limit after a reconnection (§2.13);
* never a zero duration on an activated limit (§2.2);
* a refusal retried a minute later, and a minute later again, with the device kept (§2.5);
* and no more than one write every five minutes otherwise (§2.10).

## The §14a record

Every accepted or refused limit is recorded with its timestamp, its value and the outcome.
That log is the evidence a grid operator's limit was honoured — which is the entire point
of the regulation, and something an implementation that only moves messages will not
produce.

```rust
guard.audit().records();   // what was asked, what was answered, when
```
