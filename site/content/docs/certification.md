+++
title = "Certification"
description = "The 203 abstract test cases of the four High-Level Test Specifications as data, a coverage number from cargo test, and the runtime signals a laboratory reads off a device."
weight = 135
[extra]
group = "Project"
+++

Certification for LPC, LPP, MPC and MGCP is done by a laboratory. Each use case has a
published High-Level Test Specification listing its *abstract test cases* — an identifier,
the requirements it covers, the actor under test, and whether it is mandatory.

This crate carries all 203 of them as data, drives every one, and prints the coverage.

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
device actually operates on"*. A device's parameter sheet answers those conditions, so the
same list tells a manufacturer what it has committed to.

## A coverage number from `cargo test`

`tests/conformance.rs` drives this crate through the abstract test cases' own steps, one
test per identifier:

```text
$ cargo test --features full --test conformance -- --nocapture coverage

LPC   ControllableSystem      39/43   90%
LPC   EnergyGuard              5/8    62%
LPP   ControllableSystem      39/43   90%
LPP   EnergyGuard              5/8    62%
MPC   MonitoredUnit           20/20  100%
MPC   MonitoringAppliance     34/34  100%
MGCP  GridConnectionPoint     18/18  100%
MGCP  MonitoringAppliance     29/29  100%

      total                  189/203  93%
```

The fourteen missing are the seven device-level test cases, counted once for LPC and once
for LPP; a test holds the gap to exactly that set. The Energy Guard's 62 % is the same
arithmetic: three of its eight cases are about a device rebooting. Each is listed with the
reason:

```text
Not covered here, and why (the same list for LPP):
  CSConnection_006     what the appliance actually draws, which the library reports a
                       ceiling for and does not control
  CSConnection_009     powering the device off and on again
  CSInit_002           a factory reset and the defaults it restores are the device's
  CSInit_003           where values are stored across a power cut is the device's decision
  EGConnection_001     the test rebuilds the actor rather than rebooting a device
```

Each is an obligation on the integrator. `conformance::device_level()` yields them with
their reasons, so a harness that can cut a real device's power and press its reset can
iterate exactly those cases, filtered by actor and by the parameter sheet. The suite asserts
the list matches the number.

```rust
use eebus::conformance;
use eebus::usecases::descriptor::actors;

for (case, reason) in conformance::device_level()
    .filter(|(case, _)| case.dut == actors::CONTROLLABLE_SYSTEM)
{
    println!("{}: {}\n  because {reason}", case.id, case.description);
}
```

The catalogue is behind the default-on `conformance` feature, which a build with
`default-features = false` has to name.

### What a passing suite still cannot see

Both ends of every test here are this crate, which hides any defect where the two sides
agree on a convention the *specification* left to the device. Identifiers are the main case:
`limitId`, the failsafe `keyId`s, MGCP's curtailment key and a car's per-phase limits are
placeholders — `<l1#(1..1)>`, `<k1#(1..1)>`, `<x1>`…`<x3>` — and a client that assumes its
own numbering writes a well-formed message to a real entry of the peer's, is acknowledged,
and still passes every abstract test case.

Two suites cover it. `tests/interop.rs` runs against a live `eebus-go` peer, and
`tests/foreign_identifiers.rs` builds a Controllable System by hand that numbers everything
differently — its limit on `7`, its failsafe keys on `5` and `6`, and both directions on one
`LoadControl` feature so that `limitId` 1 is the *production* limit an LPC guard must not
touch. Neither is a conformance test.

The report also prints one softer case: the library notifies a change as soon as the
application publishes it (SPINE IG §2.4), but whether the *measured* value arrives inside
the 120 seconds MPC and MGCP allow depends on the application's publish cadence.

## The tester hooks

Every High-Level Test Specification carries the footnote *"the manufacturer must specify
conditions on how the test case can be tested (e.g. via debug interface)."* Half of the LPC
transitions are timers — a duration expiring, a heartbeat that stops — and nothing goes on
the wire when one fires, so the wire alone cannot tell `unlimited/controlled` from
`failsafe state`.

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

* **The state is spelled as §2.3.2 spells it** — `unlimited/controlled`, not
  `UnlimitedControlled` — matching the specification and the `CF_CS_…` pre-conditions.
* **An unset value is `-`, never `0`.** Zero watts would read as a device limited to
  zero, the opposite of unlimited.
* **`nextDeadline`** shows the timed transitions the wire cannot.
  `ATC_LPC_COM_PT_CSTransition5_001` waits 120 seconds for one.

A Monitored Unit reports the same way under `mpc:` or `mgcp:`; a measurand whose value state
is not `normal` reports the state instead of a number, which is what the `NT_` test cases in
those two specifications check.

## What this does not claim

A passing test here is not a laboratory pass — only that the rules are the ones the
laboratory checks, under the same names, and that what is *not* checked is written down.
