+++
title = "Certification"
description = "The 203 abstract test cases of the four High-Level Test Specifications as data, a coverage number from cargo test, and the runtime signals a laboratory reads off a device."
weight = 135
[extra]
group = "Project"
+++

Certification for LPC, LPP, MPC and MGCP is a laboratory booking, and the bill arrives
whether or not the device passes. What the laboratory runs is not a secret: each use case
has a High-Level Test Specification listing its *abstract test cases* — an identifier, the
requirements it covers, which actor is under test, and whether it is mandatory.

This crate carries all 203 of them as data, and measures itself against them.

## The catalogue

```rust
use eebus::conformance::{self, Coverage};
use eebus::usecases::descriptor::{Support, actors, names};

let mandatory: Vec<_> = conformance::for_actor(names::LPC, actors::CONTROLLABLE_SYSTEM)
    .filter(|tc| tc.level == Support::Mandatory)
    .collect();

let claimed = ["ATC_LPC_COM_PT_CSTransition5_001"];
let report = Coverage::of(mandatory.iter().copied(), &claimed);
println!("{}%", report.percent());
```

Each [`AbstractTestCase`](https://docs.rs/eebus/latest/eebus/conformance/struct.AbstractTestCase.html)
carries the specification's own description, the `[LPC-TS-…]` requirements it covers, and
the conditions that raise an optional marking to mandatory — *"if the device is black-start
capable"*, *"if the `unlimited/autonomous` state is implemented"*, *"if the phase is one the
device actually operates on"*. A device's parameter sheet is those conditions answered, so
the same list tells a manufacturer what it has committed to.

## A coverage number from `cargo test`

`tests/conformance.rs` drives this crate through the abstract test cases' own steps, one
test per identifier, and prints what it adds up to:

```text
$ cargo test --test conformance -- --nocapture coverage

LPC   ControllableSystem    39/43   90%
LPC   EnergyGuard            5/8    62%
LPP   ControllableSystem    39/43   90%
LPP   EnergyGuard            5/8    62%

      total                 88/102  86%
```

The number that is *not* covered is the interesting one, and it is not padding. Roughly a
third of the abstract test cases are about the device rather than the protocol — that a
factory reset restores the defaults, that a value survives a power cut, that a reboot
completes inside the declared start-up time — and no library can answer those for the
device it is linked into. Each is listed with the reason:

```text
Not covered here, and why (the same list for LPP):
  CSConnection_006     what the appliance actually draws, which the library reports a
                       ceiling for and does not control
  CSConnection_009     powering the device off and on again
  CSInit_002           a factory reset and the defaults it restores are the device's
  CSInit_003           where values are stored across a power cut is the device's decision
  EGConnection_001     the test rebuilds the actor rather than rebooting a device
```

A coverage number that quietly counted "we cannot test this" would be worth nothing.

## The tester hooks

Every High-Level Test Specification carries the same footnote: *"the manufacturer must
specify conditions on how the test case can be tested (e.g. via debug interface)."* Half of
the LPC transitions are timers — a duration expiring, a heartbeat that stops — and nothing
goes on the wire when one fires, so a tester that can only see the wire cannot tell
`unlimited/controlled` from `failsafe state`.

[`usecases::signals`](https://docs.rs/eebus/latest/eebus/usecases/signals/index.html) is
that interface, as a shape rather than a transport:

```rust
use eebus::usecases::signals::Signals;
use eebus::usecases::lpc;

println!("{}", controllable_system.signals(lpc::DIRECTION));
```

```text
lpc:state             limited
lpc:limit             3000 W
lpc:duration          600 s
lpc:isActive          true
lpc:failsafeLimit     4200 W
lpc:failsafeDuration  7200 s
lpc:nominalMax        11000 W
lpc:contractualMax    -
lpc:effectiveLimit    3000 W
lpc:powerCeiling      3000 W
lpc:lastHeartbeat     11 s
lpc:nextDeadline      130 s
```

Three details are deliberate:

* **The state is spelled the way §2.3.2 spells it** — `unlimited/controlled`, not
  `UnlimitedControlled` — because a laboratory compares it against the specification and
  against the `CF_CS_…` pre-conditions, not against Rust.
* **An unset value is `-`, never `0`.** A tester reading nought watts for a limit that was
  never set would record a device limited to zero, which is the opposite of unlimited.
* **`nextDeadline` is there** because the timed transitions are exactly what the wire cannot
  show. `ATC_LPC_COM_PT_CSTransition5_001` waits 120 seconds for one.

A Monitored Unit reports the same way under `mpc:` or `mgcp:`, and a measurand whose value
state is not `normal` reports the state instead of a number — which is the whole subject of
the `NT_` test cases in those two specifications.

## What this does not claim

That a passing test here would pass there. Only that the rules are the ones the laboratory
will check, under the same names — and that what is *not* checked is written down rather
than discovered on the day.
