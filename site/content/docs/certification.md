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

This crate carries all 203 of them as data, **drives every one of them**, and prints what
that adds up to.

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

**The fourteen that are missing are the whole point.** They are exactly the seven
device-level test cases, counted once for LPC and once for LPP; a test holds the gap to that
set, so the number moves neither by dropping a case nor by relabelling one. The Energy
Guard's 62 % is the same arithmetic, not a shortfall: three of its eight cases are about a
device rebooting. Each is listed with the reason:

```text
Not covered here, and why (the same list for LPP):
  CSConnection_006     what the appliance actually draws, which the library reports a
                       ceiling for and does not control
  CSConnection_009     powering the device off and on again
  CSInit_002           a factory reset and the defaults it restores are the device's
  CSInit_003           where values are stored across a power cut is the device's decision
  EGConnection_001     the test rebuilds the actor rather than rebooting a device
```

A coverage number that quietly counted "we cannot test this" would be worth nothing. Each of
these is a real obligation that lands on the integrator, and the list is that integrator's
checklist **as data**: `conformance::device_level()` yields each case with its reason, so a
harness that drives a real device — one that can cut its power and press its reset — can
iterate exactly the cases the library left it, filtered by actor and by what the parameter
sheet commits to. This crate's own suite derives its list from the same table and asserts
the two agree, so the checklist cannot drift from the number.

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

The report prints one softer case alongside them: the library notifies a change the moment
the application publishes it (SPINE IG §2.4), but whether the *measured* value reaches it
inside the 120 seconds MPC and MGCP allow is the application's own publish cadence.

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
