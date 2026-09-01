# Changelog

Notable changes to `eebus`. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
[semantic](https://semver.org/), with the usual pre-1.0 caveat that a minor bump may break.

## [Unreleased]

## [0.2.0] — 2026-09-01

### Added

- `conformance`: the 203 abstract test cases of the LPC, LPP, MPC and MGCP High-Level Test
  Specifications 1.0.2 as data, with `Coverage` to measure a suite against them.
  `tests/conformance.rs` runs them and prints the number.
- `usecases::signals`: the `lpc:`/`mpc:`/`cob:` runtime values a certification laboratory
  reads off a device through a debug interface.
- Eight use cases, both actors of each: EVCC, OSCEV, EVCEM, EVSOC, MOI, MPS, MOB and COB.
- LPC/LPP scenario 4: `electrical_connection_feature`, `constraints`, `read_constraints`,
  `NominalMax`, `ControllableSystemActor::with_electrical_connection` and
  `GuardEvent::ConstraintsLearned`. [LPC-041] for a device, [LPC-042] for a CEM.
- `mgcp::FeedInLimit`: [MGCP-011] as a ceiling in watts rather than a bare percentage.
- Trust-store persistence: `runtime::TrustedPeer`, `TrustStore::to_json`/`from_json`,
  `TrustStore::forget_all` and `Node::eebus_reset` (SHIP §12.2.2).
- `Engine::start_discovery` and `Engine::discover`: the opening exchange, for an
  application that owns its own engine.
- `Hub::flush`.
- `Measurand::unphased`, and eighteen measurement quantities covering the DC side, inverter
  yields, battery state and the e-mobility values.
- `monitoring::functions`: the feature-and-function tables the monitoring use cases share.
- `examples/steuerbox.rs` and `examples/heat_pump.rs`: the two halves of a §14a
  installation as separate programs, with mDNS, persistent identity and trust, and an
  EEBUS reset.
- Golden wire vectors for the LPC exchange (`tests/fixtures/golden/`).

### Changed

- **The cryptography provider is no longer chosen by this crate.** `cert`, `tls` and
  `runtime` now require exactly one of the new `ring` and `aws-lc-rs` features; naming both
  or neither is a `compile_error!`. `rustls`' provider is process-global, so the choice
  belongs to whoever builds the binary. `--all-features` is therefore not a valid
  configuration — the new `full` feature is everything, on `ring`.
- `usecases::emobility::opev` now carries only descriptors. The state machine, payloads and
  both actors moved to `usecases::emobility::charging` and are parameterised by `Purpose`,
  so OPEV and OSCEV share one implementation. `EvActor::new`, `locate`, `locate_guard` and
  `limit_descriptions` take a `Purpose`.
- `monitoring::Measurand::phases` is now `Option<ElectricalConnectionPhaseName>`; `None` is
  a measurement that has no phases, and its parameter description omits `acMeasuredPhases`.
- `limitation::LimitRecord::write` is now `Option<LimitWrite>`, so an unreadable write is
  recorded as one rather than as a fabricated limit of nought watts.
- `runtime::TrustStore` stores `TrustedPeer` records rather than bare SKIs.
- `Hub::shutdown` sends what the engine has queued before closing.

### Fixed

- A refused limit write now moves the Controllable System out of `init`, `failsafe` and
  `unlimited/autonomous` into `unlimited/controlled`. [LPC-TS-018] and [LPC-TS-035/1]; the
  laboratory's `ATC_LPC_COM_PT_CSTransition1_001`, `CSConnection_003`, `CSTransition8_001`
  and `CSTransition11_001`.
- The opening-sequence gate re-arms when control is lost, so a stale controller cannot
  rewrite the failsafe values from the failsafe state. [LPC-TS-037],
  `ATC_LPC_COM_PT_CSFS_003`.
- The heartbeat gate is evaluated before the value in `on_limit_write`: a write with no
  heartbeat behind it is not evaluated and changes nothing, where a write that is evaluated
  and refused moves the state machine.
- The Energy Guard sends the opening limit write implementation guide §2.11 requires as
  soon as it is bound, whether or not the application has asked for a limit. Without it a
  Controllable System never left `init`.
- The Energy Guard counts only heartbeats the peer could have received — one emitted before
  the peer subscribed reached nobody, and the first limit was refused for want of it.
- `EnergyGuardActor::poll_timeout` always reports an instant that can still be reached.
- `Hub::next` no longer reads with a zero-length timeout when a deadline has passed, which
  starved the connections it was watching.
- `CsEvent::LimitUnreadable` and `NackReason::Unreadable` replace a `NegativeValue`
  mislabelling for a write whose payload could not be read.

## [0.1.0] — 2026-08-31

First tagged version. SHIP transport, SPINE model and engine, the Tokio runtime, and six
use cases: LPC, LPP, MPC, MGCP, EVSECC and OPEV.

[Unreleased]: https://github.com/hupe1980/eebus/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/hupe1980/eebus/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/hupe1980/eebus/releases/tag/v0.1.0
