# Changelog

Notable changes to `eebus`. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
[semantic](https://semver.org/), with the usual pre-1.0 caveat that a minor bump may break.

## [0.9.0] — unreleased

Times off the wire, read in both the forms SPINE permits; a descriptor that says what
the *absence* of the next message means; and the bugs that asking those two questions
turned up — three actors subscribing to less than their own use case requires, and two
places where an unreadable duration was read as no duration at all.

### Added

- **The absolute half of `AbsoluteOrRelativeTime` is readable.** `as_duration` read the
  relative form and `is_absolute` said which form arrived; nothing read the absolute one, so
  a consumer that had just been told "this is a timestamp" reached for its own parser — and
  SPINE's absolute form is `xs:dateTime`, which is not RFC 3339. The offset is *optional*,
  so `2026-09-05T08:15:00` is a valid timestamp that fixes no instant; `24:00:00` is
  midnight ending the day; the year may be negative and may exceed four digits. An RFC 3339
  parser rejects a conformant peer, and from the outside that looks exactly like a peer that
  sent nothing.

  `as_timestamp()` returns a `DateTime` that keeps the distinction the schema makes:
  `unix_seconds()` is `None` for a timezone-less value — a wall-clock reading is not an
  instant — and `unix_seconds_at(offset)` makes a caller name the zone it means to assume
  rather than defaulting to UTC and landing hours out of place. `to_system_time()` under
  `std`, `from_unix_seconds` and `AbsoluteOrRelativeTime::from_timestamp` for the sending
  side, and `parse()` returning a `TimeValue` for a consumer that has to handle both halves
  without guessing which arrived. Everything that carries meaning is refused rather than
  repaired: a thirteenth month, a 29 February in a common year, a sixtieth second, an offset
  past ±14:00, a `+0200` missing its colon.

  What stays the consumer's is the **policy**. How far a peer's clock may be wrong before
  its timestamps are worth less than the arrival time is not a decision a protocol crate can
  make for a product.

  Note for anyone reading the schema alongside this: `AbsoluteOrRelativeTimeType` is
  `xs:union memberTypes="xs:duration xs:dateTime"` and admits **no** bare `xs:time`. That
  belongs to `AbsoluteOrRecurringTimeType`, whose `time` element is a different field of a
  different type.

- **`Reading::taken_at_relative_to(arrived)`** — the relative form resolved. On a
  measurement it is an *age*, so the instant is `arrived − duration`, and every consumer
  keeping a monotonic clock was writing that subtraction. It saturates at zero rather than
  wrapping, and is `None` for the absolute form, which has no relation to a monotonic clock
  at all.

- **`descriptor::Delivery` — what silence means, as data.** A value that has not arrived for
  ten minutes is either a peer that stopped answering or a peer with nothing to say, and the
  two call for opposite responses. Nothing in the message says which, and until now nothing
  in the descriptors did either, so a consumer kept a hand-written list of which of its own
  drivers subscribe.

  The specification's answer turns out not to be the expected one. *Every* scenario of
  *every* use case here is subscription-driven — each UC TS §3.4.n.1 says "Actors SHALL
  create a subscription for each server Feature that is relevant for the corresponding Actor
  within this Scenario", and §3.3.4 names polling only as the fallback for a subscription
  that was **refused**. So notification-versus-poll is not a distinction these
  specifications draw. What they do draw is whether the notification comes on a **clock**:

  - `Delivery::OnChange` — sent when the value changes and at no other time. The age of the
    last value is not a health signal, and a driver that times out on it drops the units that
    are behaving.
  - `Delivery::Periodic(period)` — sent at least that often whether it changed or not.
    Silence past it *is* a fault. The heartbeats and nothing else: 60 s for LPC, LPP
    ([LPC-005], [LPC-006]) and COB ([COB-008]), 4 s for OPEV and OSCEV ([OPEV-005],
    [OSCEV-005]), because a car follows a current at once. The period is the specification's
    "at least every", not a tolerance — LPC allows two missed beats, OPEV none.

  Asked through `UseCaseDescriptor::delivery_of`, `periodic_functions`, and
  `Scenario::delivery`. `tests/use_case_delivery.rs` sweeps all fifty-seven descriptors — held to the
  source, so a new one cannot be added without a line in it — and
  holds the heartbeat to being the only function any of these specifications puts a clock
  on.

- **`UseCaseDescriptor::features_needing_subscription()`** — the peer features an actor has
  to subscribe to, the counterpart of `features_needing_binding`. Also
  `UseCaseDescriptor::scenario(number)`, which every caller was writing as a `find` over
  `scenarios`.

- **`limitation::HEARTBEAT_PERIOD` and `cob::HEARTBEAT_PERIOD`** — the 60 s cadence, beside
  the 120 s tolerance that was already there. Two missed beats is a decision; the cadence is
  the specification's.

### Fixed

- **A limit whose duration could not be read was applied as a limit with no duration.**
  LPC §3.1.8.2 — and LPP's and COB's, in the same words — says "Durations used within this
  Use Case SHALL be presented as relative times. The same holds for the `endTime` Element
  used for the duration of validity ([LPC-004])". The *schema* is looser: `timePeriod.endTime`
  is the `AbsoluteOrRelativeTimeType` union, so `2026-09-05T10:00:00Z` is schema-valid and
  use-case-invalid, and an Energy Guard written against the schema rather than the use case
  sends one in good faith.

  `read_limit_write` and `cob::read_setpoint_write` read that through `as_duration()` and
  fell back to [`None`] — and [`None`] there does not mean "unknown", it means **no expiry**.
  A limit the guard meant to lift after two hours was held until something else replaced it.
  Both now refuse the write, which produces a NACK the guard can act on, exactly as they
  already did for a `scaledNumber` whose scale overflows `f64`.

- **`ohpcf::activate` documented and accepted an absolute start time the feature forbids.**
  `SmartEnergyManagementPs` restricts `schedule.startTime` to `xs:duration` by
  `xs:restriction` on the `PowerSequences` type that uses the union
  (`EEBus_SPINE_TS_SmartEnergyManagementPs.xsd`), so a wall-clock instant is not expressible
  there at all. The doc said the opposite — "an absolute `2026-09-04T13:00:00Z` or a
  relative `PT0S`" — and the crate's own tests scheduled compressors with timestamps, which
  is a schema-invalid message a strict peer refuses.

  `activate` now takes a [`Duration`], so the wider spelling cannot be built, and the
  compressor side refuses one rather than storing it: a `Flexibility` that accepted a
  timestamp would announce itself `scheduled` for a time neither side could act on. The new
  `Refused::UnreadableStartTime` says which.

- **Three actors read a feature once and believed it afterwards.** Every UC TS §3.4.n.1 asks
  for "a subscription for each server Feature that is relevant for the corresponding Actor
  within this Scenario", and three of this crate's actors subscribed to some of theirs and
  read the rest. Found by holding each actor's subscriptions against its own descriptor,
  which `tests/use_case_delivery.rs` now does for all of them.

  - **`MonitoringApplianceActor::attach` did not subscribe to `ElectricalConnection`.** That
    feature is the only place `acMeasuredPhases` lives, so it does not merely *report* a
    per-phase value — it says what one **means**. A unit that re-describes its phases, an
    inverter that gains a string, a meter reconfigured from one phase to three, silently
    changed the meaning of every value the appliance kept resolving against the description
    it read at commissioning.
  - **`limitation::EnergyGuardActor::attach` did not subscribe to `DeviceConfiguration` or
    `ElectricalConnection`.** Neither value has this guard as its only author: the failsafe
    pair is writable at the appliance too ([LPC-024]), and the *contractual* maximum beside
    the nameplate in the characteristics is what a §14a agreement sets — changed by the grid
    operator, not by the guard reading it. A one-off read left the guard computing limits
    from values that had stopped being true.
  - **`emobility::charging::OverloadGuardActor::attach` did not subscribe to
    `ElectricalConnection`.** `permittedValueSet` is what says how far a car may be
    curtailed, and it changes *during* a session. A car that raises its minimum current has
    just made the guard's last write unacceptable, and the refusal would have been the first
    the guard heard of it.

### Changed

- **`FunctionUse` gained a `delivery` field**, so a struct literal of it no longer compiles.
  The six constructors set `Delivery::OnChange`; `.periodic(every)` is the const builder the
  heartbeats use.

- **The time helpers moved to `model::time`.** `parse_iso8601_duration`,
  `format_iso8601_duration` and the `AbsoluteOrRelativeTime` methods are re-exported from
  `eebus::model`, so every existing path still resolves; `model::values` is now about
  numbers alone.

- **The LPC golden vector records four subscription requests rather than one**, and the
  notifications that follow from them. That is the guard fix above, visible on the wire.

- **`ohpcf::activate` takes a `Duration`**, and `Flexibility::start_time`,
  `CompressorOffer::start_time` and `Request::Schedule::start_time` are `Duration` rather
  than `String`. The `ohpcf:startTime` signal is a number of seconds, like the two duration
  signals beside it. See the fix above for why the wider type was wrong.

- **`limitation::heartbeat` takes `Option<DateTime>` rather than a `&str` timestamp.** The
  element is the `AbsoluteOrRelativeTimeType` union, but the relative half says nothing on a
  heartbeat — "produced zero seconds ago" is not a fact about anything — so this takes the
  absolute one and writes it in canonical form. [`None`] is a device with no clock, which is
  what `HeartbeatProducer::beat` still sends; its documentation now shows how a device with a
  real-time clock fills the field in.

## [0.8.0] — 2026-09-05

The client side of the HVAC family: `locate`, `follow` and an actor for the nine `HVAC` use
cases, so a Configuration Appliance no longer assembles the exchange by hand. And a
measurement that carries the time the peer took it.

### Added

- **`hvac::peer` — `locate` and `follow` for the nine `HVAC` use cases.** `mdt`, `mrt` and
  `mot` each had a `locate`; the six system-function use cases and the three setpoint ones
  had none, so a Configuration Appliance resolved the feature by hand — `use_case(name,
  actor)`, then `address_of(found, &FeatureType::HVAC, Role::Server)` — with two chances to
  name the wrong actor and one to reach for the appliance's own entity instead of the one
  that announced the use case. That last is the use-case implementation guide's §3.3 rule,
  and it is not academic: §3.2.2.2.1 gives an entity **one** `HVAC` feature, so a heat pump
  that heats water and two rooms has three of them and a lookup by feature type reports a
  living room's operation mode from the tank's.

  `cdsf::locate` and its eight siblings return an `HvacPeer` — the `HVAC` feature, and the
  `Setpoint` beside it for `cdt`, `crht` and `crct`. `locate_all` returns one per entity,
  which every room use case needs for the same reason `mrt::locate_all` does.

  Locating was never the whole of it, which is why this is `follow` and not only `locate`.
  A `SystemFunction` answers nothing until six payloads have arrived — the system function
  descriptions, the operation modes, the relations between them, the state, and, for the hot
  water, the two overrun functions — and a client that read the descriptions and stopped
  holds a reader that refuses every write it is asked to build. `HvacPeer::follow` subscribes
  to both features and issues every read in one call, in the order the specification's own
  scenario tables put them, and returns a `Following` whose counters say which read a
  `ResultReceived` refused. The subscription goes out **before** the reads: a mode changed
  between the reply and a later subscription request is a change nothing ever hears about.
  There is no binding, as all nine use cases instruct.

  The read list is not a second list kept beside the specification — it is
  `UseCaseDescriptor::server_functions()`, the scenario tables this crate already carries as
  data, intersected with what the peer declared in discovery. So a circuit serving CDSF
  scenario 1 and not the one-time loading — which is conformant, scenario 3 being
  recommended — is asked for four functions rather than six, instead of earning two
  `errorNumber` 7 replies to questions discovery had already answered.

- **`hvac::HvacApplianceActor` — the family had readers and no actor.** Every other family
  here has one. The HVAC client side had a `SystemFunction`, a `Setpoints` and no way to
  hold them, so a Configuration Appliance wrote the same bookkeeping every time: a reader
  per system function, a reader per setpoint scope, routing from feature address to unit,
  change detection so a re-notified mode is not reported as a change, and the join between
  the two halves that makes a temperature write mean anything.

  It serves **both** client actors, which read the same data and differ only in whether
  they also write. `attach` takes a located peer — the `Subject` stamped on it says which
  readers to build — and use cases attached against one entity gather on one unit, because
  §3.2.2.2.1 gives an entity one `HVAC` feature and one `Setpoint` feature.

  The write side is where it earns its place. `set_temperature(&unit, &hvac::DHW, 60.0)` is
  four joins deep: the appliance's own `systemFunctionId`, the mode it is *currently* in,
  the relation keyed by both, and the constraints for the setpoint that names. A room's
  heating and its cooling resolve to different setpoints out of the same
  `roomAirTemperature` list, told apart by nothing but the relation's `systemFunctionId`.
  Two answers are refusals rather than guesses: `SeveralSetpoints` where the current mode
  reads more than one, which [CDT-003/1] permits `auto` to do, and `ModeUnknown` where no
  system function has been attached — [CDT-005] as an API.

  `handle_event` returns a **`Vec`** where every other actor returns an `Option`, because
  one `hvacSystemFunctionListData` carries every system function the appliance has and one
  notification can move a room's heating *and* its cooling. Only changes are reported, and
  `OverrunFinished` stays separate from `OverrunChanged`: [MDSF-002]'s `finished` is a
  one-shot announcement, not a state to rest in.

- **`usecases::UnitId` — one key for a room's thermometer and its setpoint.** It was
  `monitoring::UnitId`, which was the right shape in the wrong place: an
  `hvac::mrt` room and an `hvac::crht` room are the *same entity*, and an application
  joining "it is 19.5 °C in here" to "the target is 21.5" had no reason to be given two
  types to do it with. It is now one type both actors hand back, with `UnitId::of` and
  `UnitId::holds` as the primitives; `monitoring::UnitId` still resolves, as a re-export.

- **`Setpoints::current_setpoint` and `Setpoints::write_current`** — "the temperature this
  server is working to", without an application having to know which of the appliance's one
  to four setpoint identifiers the mode it happens to be in is reading. The three ways it
  can have no answer are three refusals rather than a `None`: no mode reported, a mode that
  relates to no setpoint at all (which [xDT-003/3] lets `off` be), and a mode that relates
  to several.

- **`Reading::timestamp` and `Readings::read_at` — when the peer says it measured.**
  [MPC-002], [MDT-002], [MRT-002] and [MOT-002] all permit a `timestamp` on a measurement
  and forbid the history that would otherwise justify one. The crate dropped it, leaving a
  consumer only the moment the value *arrived* — a different number for a subscribed value,
  which comes when something **changes**, so a room holding its temperature sends nothing at
  all. Ageing readings by arrival discards the ones most likely to still be true, and cannot
  tell that room from a sensor that died an hour ago.

  Carried verbatim as an `AbsoluteOrRelativeTime`, since SPINE permits a time *or* an ISO
  8601 duration and which one arrived is part of what the peer said. Most peers send none,
  and what that means stays the consumer's decision. The server half goes with it:
  `MonitoredUnit::set_at`, and `mdt`/`mrt`/`mot`'s `temperature_at` — neither invents a
  timestamp from the engine's clock, which is monotonic uptime and not a time of day.

- **`UseCaseDescriptor::server_functions()`** — every `(feature, function)` an actor serves,
  deduplicated, in scenario order. The scenario tables read back as a list of reads, which
  `HvacPeer::follow` is built on.

### Fixed

- **`Setpoints::write_effective` blamed the mode for a setpoint the server never
  published.** An identifier no description had mentioned went to `effect_of`, which
  answered `NotInCurrentMode` — true, and useless: "the server is in a mode that reads some
  other setpoint" sends a caller looking at the modes when what is wrong is the number it
  passed in. Whether the setpoint exists is now asked first, and answered
  `UnknownSetpoint`.

### Changed

- **`hvac::{cdsf, mdsf, …}::locate` take no arguments and return an `HvacPeer`**, and the
  generic `hvac::peer::locate` takes the use case's `Subject` beside its descriptor. Each
  module's wrapper pairs the two, so a caller cannot pass a descriptor and a subject that
  disagree.

- **`monitoring::MonitoringApplianceActor::readings` and friends still take a `&UnitId`**,
  but the type is now `usecases::UnitId`. Nothing on either actor changed shape; the path
  did, and `monitoring::UnitId` re-exports it.

- **`MonitoredUnit::set` no longer takes a `now`.** It never used it — the parameter was
  `_now` — and keeping it invited exactly the mistake the timestamp work is about: a
  monotonic uptime is not a time of day, and passing it off as one would put a meaningless
  `timestamp` on the wire. `set_at` takes the real thing, from the application that has a
  clock.

- **`Reading` gained a `timestamp` field**, so a struct literal or an exhaustive destructure
  of it no longer compiles. `Readings::latest` is a borrowing `get` for callers that only
  want to look.

- **`hvac::{mdt, mrt, mot}::temperature_from` and `hvac::temperature::reported` take the
  timestamp.** `temperature(degrees)` is unchanged and sends none; `temperature_at` is the
  short form for a stamped measured value.

## [0.7.0] — 2026-09-05

Twelve HVAC use cases, which is both specification bundles complete, and the interoperability
bug that made the previous five unusable: an unbound write was refused whatever it addressed,
where SPINE §7.3 makes the binding a property of the feature and every HVAC use case says not
to use one.

### Added

- **`usecases::hvac::mrt` and `usecases::hvac::mot` — the building the heat pump heats**,
  both actors of each. Monitoring of Room Temperature and Monitoring of Outdoor Temperature:
  the air temperature of an indoor space, and what it is like outside. They matter for a
  reason MRT §2.1 states and understates — "this information could also be used to estimate
  energy demands for heating or cooling". A building's thermal model is identified from the
  temperature inside it, the temperature outside it, and the heat delivered in between, and
  a fitted model is what turns "the roof will export at noon" into "start the compressor at
  eleven". Without them `ohpcf` could start a compressor for a house whose thermal state
  nothing on the wire reported: the plan was expressible and what it is planned against was
  not.

  A heat pump measures both anyway — its own heating curve runs on nothing else — and the
  outdoor sensor is on the wall of *this* building rather than in a forecast for the grid
  square, which on the days it matters is a difference of several degrees.

  `Quantity::RoomTemperature` and `Quantity::OutdoorTemperature` join the measurement layer,
  both on `commodityType: air`, so both are read through the `Readings` an MPC or MGCP
  appliance already runs. MDT, MRT and MOT are now one exchange under three names —
  `hvac::temperature` — differing in the `useCaseName`, the actor that serves it, and the
  `scopeType` that says which temperature. All three publish under `measurementId` 1 by their
  own reckoning, so the scope is the only thing that tells them apart on a heat pump that
  serves all three.

- **`ohpcf::CompressorPeer::follow` — the binding a CEM had no way to know it needed.** The
  two OHPCF scenarios are served from one feature and ask for different pre-scenario
  communication: §3.4.1.1 says of scenario 1 that "Binding SHOULD NOT be used for this
  Scenario", and §3.4.2 says of scenario 2 that an actor writing part of the feature "needs
  to create a binding […] Only one binding partner is allowed to write the data specified in
  this Scenario". Nothing in `locate`, `CompressorPeer` or the four command constructors said
  so, and every other use case a monitoring-shaped controller has met here needs no binding
  at all — so a CEM subscribed, read a perfectly good offer, and had `activate` answered
  `BindingRequired` before the compressor's own state machine ever saw it. An offer it could
  see, report, and never take up.

  `follow` sends the binding, the subscription and the initial read in the order the two
  scenarios put them, and returns a `Following` with the three counters separately: a refused
  binding and a refused subscription are different commissioning faults, and
  `ResultReceived` is the only place either is visible. The documentation on `locate`, on
  `CompressorPeer` and on all four of `activate`/`stop`/`pause`/`resume` now says which
  privilege each needs.

- **The other seven HVAC use cases, so the family is complete.** `hvac::cdsf`,
  `hvac::mrhsf`, `hvac::crhsf`, `hvac::mrcsf`, `hvac::crcsf`, `hvac::crht` and `hvac::crct`
  join the five already here, which is every use case in both HVAC specification bundles —
  the hot water's mode, overrun and temperature; a room's heating and cooling modes and
  setpoints; the room and outdoor thermometers. Both actors of each.

  The one an energy manager reaches for first is **CDSF scenario 2**: starting a one-time
  DHW loading is the shortest path there is from "the roof is exporting" to "the tank is
  absorbing it". Unlike a setpoint, which the circuit's own controller may decline to act
  on, and unlike `ohpcf`'s process, which the compressor has to have announced first, it is
  the button in the bathroom pressed over the wire — and scenario 3 gives it back when a
  cloud arrives. CDSF also closes a hole the crate documented against itself: [CDT-005]
  makes MDSF or CDSF mandatory beside CDT, and a circuit that served CDSF instead of MDSF
  had no mode this crate could read, so `write_effective` refused every setpoint write with
  `ModeUnknown` for ever.

  Twelve use cases, three exchanges. `hvac::system_function` is the operation-mode one and
  serves six of them, told apart by `systemFunctionType` and by whether the actor may write;
  `hvac::setpoint` is the temperature-setting one and serves three; `hvac::temperature` is
  the measurement one and serves three. `SystemFunction` replaces `mdsf::DhwSystemFunction`
  and `Setpoints` replaces `cdt::DhwSetpoints`, each told at construction which function or
  scope it is following — because one `HVAC` feature carries every system function the
  appliance has (§3.2.2.2.1 gives an entity one feature of a type) and picking the wrong one
  is invisible: same payloads, peer's identifiers, answer about a different room.

  `SystemFunction::set_mode_named` is the writing half, and it refuses what the peer would:
  a mode this system function does not relate to — the modes are described once for the
  device and a subset is related to each function, so `eco` may be a hot water mode and not
  a room one — or a peer that said `isOperationModeIdChangeable: false`. `SystemFunction::apply`
  is the server's side, turning an incoming write into a `Request` or a `ModeRefused` before
  the acknowledgement rather than after it. It takes the write's **fragment**, which is the
  opposite of the rule everywhere else here and is the right way round for a list function
  with more than one entry: the resolved state holds every system function the appliance
  has, each with the mode it was already in, so it cannot say which entry the peer
  addressed. `ModeRefused::NotAddressed` is how a server with two functions on one feature
  dispatches — "this names the other one, try it" — and only when no reader claims a write
  is it an answer to send back.

- **Plural publishers for a device that serves more than one system function.**
  `system_function::descriptions`, `operation_mode_relations_of`, `states`,
  `setpoint::descriptions`, `constraints_of`, `values` and `relations_of`. A room that heats
  *and* cools has to publish both functions in **one** `hvacSystemFunctionListData` and both
  setpoints in one `setpointDescriptionListData`; publishing them one at a time replaces
  rather than adds, and the crate could not express the conformant shape at all.

- **`RemoteDevice::use_cases_played`** — every entity of a peer that plays a use case as an
  actor, where `use_case` returns the first. §7.5 keys use-case information by address and a
  device may announce the same actor on several entities, which `hvac::mrt::locate_all` and
  `hvac::mdt::locate_all` are built on.

### Fixed

- **Every unbound write was refused, which made the whole HVAC family unusable.** The
  engine applied `TC_SPINE_BIND_002` as a rule of the protocol: a write from a peer holding
  no binding came back `errorNumber` 9, whatever it was addressed to. But §7.3 puts the
  requirement on the **feature** — "please note that some feature types define requirements
  for binding!" — and the test case says the same, rejecting an unbound write "if the target
  feature **requires** a binding (e.g. LoadControl)". The use cases then say which do, and
  they disagree: LPC/LPP, OPEV/OSCEV, COB, EVCS and OHPCF scenario 2 each require one, while
  **every** HVAC use case, including all six that write, says "Binding SHOULD NOT be used
  for this Scenario".

  So a conformant Configuration Appliance's hot water setpoint write was answered
  `bindingRequired` by a circuit built on this crate, and CDT could not run at all against
  any other implementation. It was invisible here because the crate's own test bound first —
  the one thing a real peer will not do. `LocalFeature::with_unbound_writes` and
  `spine::WriteBinding` are the fix; the default stays `Required`, which is the safe way
  round, and `hot_water_over_the_wire` now runs its whole suite with no binding anywhere.

- **A setpoint relation was keyed by the operation mode alone, and the mode is not unique.**
  `hvacSystemFunctionSetpointRelationData` is keyed by `systemFunctionId` (PRIMARY) *and*
  `operationModeId` (SUB) — the crate's own generated identifier table had it right — but
  the CDT reader stored one entry per mode. Operation modes are described once for the
  device and shared between system functions, so a room that heats and cools relates the
  same `auto` twice, and `for_mode(auto)` returned whichever entry arrived last. A manager
  asking which setpoint to write for heating would be handed the **cooling** setpoint: the
  room applies it, acknowledges it, and gets colder. `Setpoints::for_mode` now takes both
  identifiers, and `describes` tells "relates to no setpoint" apart from "never described".

- **`system_function_id` was one number for every system function.** A device serving CDT
  and CRHT published the hot water and the room heating under `systemFunctionId` 1 — one
  system function claiming to be both, and a client reading whichever description arrived
  last. It is now per kind: `dhw` 1, `heating` 2, `cooling` 3.

- **Declaring a function twice on a `LocalFeature` appended a second entry.** §7.1.2's
  `supportedFunction` list names each function once, so the duplicate was announced twice in
  detailed discovery — and the *first* of the two decided what a peer was allowed to do with
  it. A feature built up in layers, which is how every configuration use case here is built
  (the writeable feature is the read-only one plus two writes), therefore declared a write
  and refused it, with nothing in either end's logs saying why. The later declaration now
  replaces the earlier one.

- **A Monitoring Appliance held one unit per device, so the second entity evicted the
  first.** `MonitoringApplianceActor` keyed its units by `AddressDevice`: `attach` dropped
  any unit already tracked on that device, and `handle_event` resolved a notification against
  the *first* unit of the device it came from. One device is regularly several units — the
  use-case implementation guide §3.3 puts an actor's features on the entity that announced
  it, and §7.5 keys use-case information by address, so a heat-pump gateway announces one
  `HVACRoom` per room — and the failure is silent in the worst way: the bedroom's temperature
  is reported as the living room's, in the right unit, in a plausible range. Units are now
  keyed by `UnitId` (device **and** entity), a notification is attributed by the feature
  address it arrived on rather than by its device, and several units of one device sit side
  by side. Latent for MPC and MGCP, where a device with two `MonitoredUnit` entities is
  unusual; unavoidable for MRT, where more than one room is the normal case.

### Changed

- **`mdsf::DhwSystemFunction` is `hvac::system_function::SystemFunction`, and
  `cdt::DhwSetpoints` is `hvac::setpoint::Setpoints`.** Both are told at construction which
  system function or scope they follow — `SystemFunction::dhw()`, `Setpoints::room_air()`,
  and each use-case module has a `reader()` that picks the right one. `SetpointEffect` and
  `WriteRefused` move to `hvac::setpoint`, `OverrunReport` to `hvac::system_function`.
  `hvac::SYSTEM_FUNCTION_ID` becomes `hvac::system_function_id(kind)`, and
  `hvac::find_dhw_system_function` becomes `find_system_function(data, kind)`.

- **`Quantity` gained two variants**, `RoomTemperature` and `OutdoorTemperature`, so an
  exhaustive `match` on it downstream no longer compiles. Deliberately not
  `#[non_exhaustive]`: a controller that reads temperatures off a heat pump should be made
  to say what it does with a room and with the weather, rather than have them fall into a
  wildcard arm beside a heatsink.

- **`MonitoringApplianceActor` is addressed by `UnitId`, not by `AddressDevice`.** The fix
  above, as an API: `readings`, `curtailment`, `feed_in_limit` and `detach` take a
  `&UnitId`, and `MonitoringEvent::UnitDescribed`, `Measured` and `CurtailmentChanged` carry
  `unit: UnitId` where they carried `device`. `MonitoredUnitPeer::id()` is where one comes
  from, `UnitId::device` is still there for a caller that wants only that, and
  `detach_device` removes every unit of a device — what a lost connection takes with it.

## [0.6.0] — 2026-09-05

Everything in this release came out of one round of feedback from a consumer (`hems`), and
half of it is bugs that consumer could not have found from the outside — the shape of the
API stopped it looking.

### Added

- **`ship::TrustLevel` — SHIP §12.3.2's trust model, with the rules that read it.** Three
  independent categories and Table 10 transcribed, so `permits_communication`,
  `permits_pin_transmission` and `permits_ship_commissioning` are the specification's own
  sentences rather than an assumption. `Trust::Trusted` and `TrustedPeer` carry one;
  `TrustStore::award_pin_trust` applies §12.5's once-only second factor — the first peer to
  prove the PIN since a factory reset may act as a SHIP commissioning tool, every later one
  is capped at 16 — and `HubEvent::PinVerified` / `PinWithheld` report both halves, because
  a withheld PIN looks from the far end exactly like having none.

- **`Engine::read_filtered` — the partial *read* the crate could serve and not send.**
  §7.1.3 and §7.5.3 both ask for one by name: "where is your `LoadControl` feature?" and
  "do you play the Controllable System?", each a few hundred bytes instead of a large
  device's whole tree. `write_filtered` had existed since 0.5; the read half had not, so the
  claim that Restricted Function Exchange was "sent as well as served" was only half true.

- **`usecases::ohpcf` — Optimization of Self-Consumption by Heat Pump Compressor
  Flexibility**, both actors, both scenarios. The second lever in this crate that can ask an
  appliance for *more*, and a different mechanism from the first: `hvac::cdt` raises a hot
  water **setpoint** and leaves the circuit's controller to decide, while this **starts a
  process** — the compressor's optional power consumption — at a time the CEM names, then
  stops, pauses or resumes it. A tank already at temperature will not run for a higher
  setpoint; it will run for this, which is what makes pre-heating on surplus PV expressible
  at all.

  `Flexibility` is the compressor's side: a six-state machine that **refuses before it
  acknowledges**, the shape LPC's Controllable System established. A pause on a process that
  is not running, a stop on one never announced as stoppable, a re-schedule after it has
  started — each is a `Refused` with an `errorNumber`. `Interrupt` has no empty case because
  [OHPCF-011/7] has none: a compressor always offers at least one way out, and a peer that
  announces neither is reported rather than assumed. `CompressorOffer` is the CEM's side,
  and it tells apart four phases of one function — an offer, a process, an ending, and
  "there is no process" [OHPCF-003], which is a fact rather than an unreadable payload.

  The SMA Home Manager 2 in `tests/fixtures/devices` announces this use case, which is what
  moved it from specified to deployed.

- **`usecases::emobility::evcs` — EV Charging Summary**, both actors. What a charging
  session cost and where its energy came from, split into the share off the roof and the
  share off the grid. It is the one function in this crate a *client* actor writes for
  somebody else's screen: the EVSE serves a writeable `Bill` and the energy manager fills it
  in, because only the manager knows what the tariff was.

  Four of the eight devices in the corpus announce it, and they do not agree about who they
  are — `EVSE` (Elli, Spelsberg), `CEM` (Kostal, which §3.2.2.1 explicitly permits for the
  client side) and `EV` (the Porsche Mobile Charger Connect, which the specification does not
  define for this use case at all). So `evcs::locate` matches on the **role**: §3.2.2 gives
  the Energy Broker "only client functionality", so a peer serving a `Bill` is the EVSE
  whatever it calls itself, and a CEM announcing the name is excluded by the same rule.

- **`conformance::harness` — the device-level half of the suite, runnable against a box.**
  Fourteen of the 203 abstract test cases are about the device rather than the protocol, and
  until now the crate named them and stopped. Seven procedures now carry the High-Level Test
  Specification's own steps and expected results (tables 15, 17, 19, 28, 31, 33, 34), each
  answering the LPC case and its LPP twin, and **judge** what a consumer observed against
  what the device declared in its parameter sheet: the failsafe defaults a factory reset must
  restore, the `StartUpDur` of §6.11.8 a black start has to come back inside, the sixty
  seconds a rebooted Energy Guard has to send a heartbeat and the limit that follows it.

  `Verdict::Inconclusive` is the variant that earns its place: a persistence test that writes
  back the value the device already held proves nothing, and counting it as either a pass or
  a failure would be a lie in one direction or the other.

- **`Engine::add_entity`, `remove_entity` and `modify_entity`, and SPINE §7.1.5 on both
  sides.** A
  device may add, remove or modify its entities while a peer is connected, and neither half
  of that existed. `LocalDevice::remove_entity` removes an entity and everything addressed
  beneath it; the engine announces the change as §7.1.5 requires — a notify, restricted, with
  `lastStateChange: removed` and no `featureInformation` (rule 4) — and releases the bindings,
  subscriptions and use cases that named the entity's features.

  This is the one message an arrival is not: a peer learns of a new entity from any re-read
  of discovery, and of a *departure* only from a device that sends this, because a merged
  discovery document cannot shrink on its own. EVCC scenario 8, the car that drove away, is
  unobservable without it — in this crate and in a simulator built on it alike.

- **`Node::pending_peers`, `Node::watch_pending`, and public `accept_reporting` /
  `connect_reporting` / `connect_over_reporting`.** A peer waiting on a trust decision was
  visible only through `Hub`, and a driver that owns its own `Engine` — because it has to be
  testable without a socket — could not learn the SKI of the box asking to pair. It can now,
  told or asked, with a plain `accept` filling the list in either way. Field reports make the
  SKI exchange the commonest §14a commissioning failure there is, and a box that cannot
  *display* the number cannot take part in it.

- **`hvac::mdt::locate`**, and `hvac::cdt`'s mode gate: `DhwSetpoints::write_effective` and
  `effect_of`. A setpoint the circuit's current operation mode does not read is written,
  acknowledged and changes nothing, and nothing on the wire says so — the gate refuses it,
  the same shape as the limitation actor refusing a limit with no recent heartbeat. An
  overrun in progress is reported as a distinct effect rather than refused: the write lands
  where it was meant to, and takes effect when the one-time heating ends.

- **An interop direction that had never been run: `eebus-go` dialling *this* crate's
  listener.** Both existing interop tests dial a container that listens, which is the wrong
  way round for the installation §14a describes and means the accept path had never met an
  implementation it did not share code with. It needs `--network host` on a native Linux
  daemon — `eebus-go`'s examples discover their peer over mDNS rather than taking an address,
  so `host.docker.internal` does not help — and the test skips with that reason rather than
  reporting a pass where the daemon cannot do it.

### Fixed

- **A datagram overtaken in flight was dropped as a duplicate.** The receiver kept only the
  highest `msgCounter` a peer had sent, so anything numbered below it was refused. But
  §5.2.4 makes the counter a sender-unique name for `msgCounterReference` to point at, not a
  sequence number, and a peer that allocates it in one task and writes the datagram from
  another sends them interleaved — `eebus-go` numbers a binding request 10, then writes a
  result numbered 11 from another goroutine first. The binding call was discarded without
  even the `result` its `ackRequest` asked for, so the Energy Guard was never identified,
  its heartbeat never subscribed to, and every limit it wrote refused with `errorNumber` 7.
  `MsgCounterTracker` now keeps a sixty-four-counter window, and reports the new
  `CounterCheck::Reordered` for a message that arrives late but has not been seen.

- **`Mdns::announce` refuses an announcement with no address.** DNS-SD resolves an instance
  to a host name and the host name to an address, so a service registered with none is one a
  peer finds, reads the SKI from, and cannot dial. `mdns-sd` accepts the registration and
  nothing reports an error at either end: the announcing node simply waits for a connection
  that can never be made. It is now `MdnsError::NoAddress` at the call, and it is what the
  `eebus-go`-dials-us interop test was tripping over.

- **One unapproved peer could fill the whole pending-trust table by dialling repeatedly.**
  `MAX_PENDING_TRUST` counted handshakes, and the question is deduplicated by SKI — so a
  device dialling four times was asked about once and spent four slots, leaving no room for
  a peer a user might actually want to approve. It is now one slot per SKI, which is safe to
  enforce because a SKI came out of a completed TLS handshake and an address proves nothing.
  (R13)

- **This node's PIN was sent to any peer that asked for one.** SHIP §12.5: "The PIN is an
  authentication secret that must be kept confidential and SHALL only be shared with
  authenticated and authorized communication partners. Therefore, the SHIP node PIN SHALL
  NOT be transmitted if the public key of the corresponding communication partner has a
  user trust level that is less than '32'." Trust here was a boolean, so there was nothing
  to check against — safe only because `TrustStore` has no auto-accept mode, which is an
  invariant nothing checked and nothing wrote down. SHIP §12.3.2's trust model is now
  implemented: three categories, Table 10 transcribed, and the three rules that read the
  numbers. (D97)

- **What identifies a list entry was guessed from a naming convention, and was wrong in
  both directions.** The generator took an entry's identity to be the leading run of
  elements typed `*IdType`. SPINE §3.4.2.1 distinguishes an identifier that *addresses* an
  entry — PRIMARY, SUB — from a FOREIGN one that refers to another feature and "is **not**
  used to create further dimensions of list entries", and the convention cannot see the
  difference. Every way it was wrong produced no error anywhere: a partial update whose
  entry matches nothing stored is *appended*.

  * `hvacSystemFunctionData` was identified by `{systemFunctionId, currentOperationModeId}`
    — the second being *the mode the DHW circuit is in*, which Table 64 marks FOREIGN. A
    circuit notifying `auto → off` partially left the appliance holding **two** system
    functions and `mdsf::DhwSystemFunction` reading whichever came first, for ever.
  * `setpointDescriptionData` demanded `measurementId` and `timeTableId`, both marked
    FOREIGN by Table 117 and both conditional — "Otherwise it SHALL be omitted" — so a
    conformant description that links to neither had no identity and was refused with
    `errorNumber` 7.
  * `powerTimeSlotScheduleData` was identified by `{sequenceId}` alone, because
    `PowerTimeSlotNumberType` does not end in `IdType` — losing the case §3.4.1 uses as its
    own worked example: "a 'slot' with number 2 of 'sequence' 5 is different to 'slot' 2 of
    'sequence' 7".

  The generator now reads the specification's own answer: **Annex B.7, Table 358** lists
  every identifier type and the element it is used as, and two tests derived from §3.4.2.1
  separate identity from reference. Six entry types changed, each checked against the
  element-rules table that governs it, and `tests/list_identifiers.rs` writes the answers
  down. (D93)

- **Nothing subscribed to discovery, so a peer's runtime changes were announced to nobody.**
  §7.5.4 says it outright — "a device that uses use case discovery **should always
  subscribe** to use case discovery" — and §7.1.5 gives it teeth: a peer's *departing*
  entity is announced only to subscribers, because a discovery document merges and a shorter
  re-send says nothing about what went. `Hub` now asks for one when discovery completes,
  which is the first moment the peer's device address is known; `Engine::subscribe_to_discovery`
  is the same thing for a driver that owns its engine. One subscription covers both
  functions — §7.4.1 permits only whole-feature subscriptions. (D94)

- **A partial `nodeManagementUseCaseData` erased every use case a peer plays.** The same
  defect as the discovery tree below and for the same reason: its entries are keyed by an
  `address` the wire makes optional — absent from every SPINE 1.1.1 peer in the corpus — and
  an `actor`, which is not an identifier type, so the generic merge replaced the list. It is
  now merged by §7.5.4's own rules, beside detailed discovery. (D94)

- **The binding and subscription tables were sent to every peer in full.** §7.4.3: "between
  two devices A and B only those subscription entries are exchanged that concern the devices
  A and B. I.e. no subscription entries of a third device C are exchanged between A and B."
  Serving the whole table tells every peer which other peers this device is talking to and
  how — on a §14a box, the grid operator's relationship with the household, handed to
  whatever else is on the LAN. The tables are now built per recipient, and the list element
  is *absent* rather than empty when nothing concerns them, which Table 18 distinguishes
  deliberately. (D95)

- **A relation this device held existed only inside whichever actor asked for it.** §7.4.1
  rule 4 — "Subscription information SHALL be kept persistently by **both** subscription
  partners" — and §7.4.3 has each entry describe an own feature's relation to a partner's,
  in either direction. `Relations` now records which side this device is on, and the engine
  records a held relation when the `result` for its own request comes back clean. A held
  relation carries no identifier, because Table 18 makes `subscriptionId` optional and says
  the value is the *server's* to assign. (D95)

- **An accepted partial write on OHPCF's feature notified every subscriber of a compressor
  that had withdrawn everything it announced.** `smartEnergyManagementPsData` is one value
  rather than a list SPINE can address entries within, and OHPCF's Table 10 makes the
  partial write mandatory — so storing the CEM's two-field fragment replaced the
  compressor's whole `alternatives` element, and §7.4.1 notified every subscriber of the
  result, the writer included. `Engine::accept_write_with` stores the document the
  *application* says the write results in and notifies once; `accept_write` is unchanged and
  stays right for every list function, which is every other writeable function here. (D96)

- **A peer's discovery tree was destroyed by any conformant runtime update to it.**
  `nodeManagementDetailedDiscoveryData` is not a list in SPINE's sense — it is three parallel
  lists inside one value — so the generic partial merge *replaced* `entityInformation`
  wholesale. §7.1.5 rule 3 has a device send only what changed, so a peer announcing one new
  entity would have taken every other entity it had ever announced with it. The engine now
  merges this one function by §7.1.5's own rules and keeps it outside the bounded per-peer
  table, where an eviction would have left the next fragment merging into nothing. A
  `cmdClassifier: delete` — which the specification does not prescribe here, but which a peer
  meaning "this entity is gone" might send — removes what it names instead of clearing the
  whole document.

- **`RemoteDevice::feature_for` located nothing on three of the eight devices in the
  corpus.** A use case may announce its address as a device with no entity part, and the
  entity path of such an address is `[]` — which named an entity no device has, so every
  `locate` in the crate returned `None` for the Elli Charger Connect Pro, the Spelsberg
  Wallbox Smart Pro and the Kostal Smart Energy Meter. An address that names no entity now
  takes the same route as no address at all: the feature is what says where the actor is.
  Found while implementing EVCS against the corpus.

- **`monitoring::locate` required features MGCP ties to optional scenarios.**
  §3.2.2.2.1 says the feature table's presence indications are "meant relative to the ones of
  the according Scenario stated in Table 1", and Table 1 marks those separately per actor:
  `DeviceConfiguration` belongs to scenario 1, `Measurement` and `ElectricalConnection` to
  scenarios 2 to 7. All three fields of `MonitoredUnitPeer` are now `Option`, `attach` reads
  only what the peer serves, and `locate` requires only that it serve at least one of them —
  a peer announcing the use case and none of its features is still reported as `None`,
  because there would be nothing to read from it.

### Changed

- **Every enum this crate *reports* through is `#[non_exhaustive]`.** `HubEvent`,
  `SpineEvent`, `ship::Event`, every actor's event type, `ConnectionError`, `AbortReason`,
  `Disconnect`: a `match` on one now needs a `_` arm. Enums the **specification** closes —
  `LimitationState`'s five states, `Trust`, `PinState`, every generated enumeration — stay
  exhaustive, because a new variant there means the standard moved and a consumer should be
  made to look. This release alone added five `HubEvent` variants; without the attribute
  each was a breaking change for a consumer that had done nothing. (D98)
- **`Trust::Trusted` carries a `TrustLevel`.** `Trust::VERIFIED` is the old meaning — a
  person approved the SKI, Table 10's `user verified`.
- **`Handshake::send_pin` returns `bool`** and is gated by §12.5;
  `send_pin_if_permitted` sends what `HandshakeConfig::peer_pin` holds under the same rule
  and discards it afterwards, which §12.5 asks for.
- **`Relation` says which side of a relation this device is on.** `id` is now
  `Option<u32>` — a relation this device holds carries the peer's identifier, which it does
  not know — and `held` says which direction it is. `is_bound` and `is_subscribed` still
  mean *granted*, because they answer "may this peer write to us?" and "who do we notify?";
  `holds_binding` and `holds_subscription` are the other question.
- **`HubEvent::TrustRequested` carries a `PendingPeer` instead of a bare `ski`.** Both
  identities SHIP knows a node by, both proved rather than claimed: the SKI printed on the
  box, and the certificate fingerprint a QR code carries as `FPH256`. Match on `peer.ski`
  where you matched on `ski`.
- **`runtime::TrustReporter` is public** and takes a `PendingPeer`.
- **`LocalDevice`, `LocalEntity`, `LocalFeature` and `FunctionEntry` derive `PartialEq`.**

## [0.5.0] — 2026-09-04

### Fixed

- **A single unanswered message stopped a use case for good, in silence.** The SPINE
  implementation guide §2.6.2 gives an escalation path for an unresponsive peer — retry
  after 30 s, then 5 min, then 15 min — and three places in this crate cited it as the
  reason `SpineEvent::RequestTimedOut` exists while nothing implemented it. The engine now
  walks it. A request outlives the transmissions that carry it: each retry goes out under a
  fresh `msgCounter`, because a repeated one is a duplicate a conformant receiver drops
  (`TC_SPINE_DATA_003`), and the request keeps the first counter as the name every event
  uses — so a caller's acknowledgement still arrives under the counter it was handed and
  `RETRY_SCHEDULE` is invisible to it.

  What made this expensive rather than merely incomplete was reading every identifier from
  the peer's own description: once a write waits on a description read, a lost read is a use
  case that never starts, and an outstanding write already blocked the next one, so a lost
  acknowledgement was a use case that stopped. The Energy Guard was the worst of it — write
  a limit, drop the acknowledgement, and it heartbeats forever without writing another,
  which under § 14a is a control box that has silently stopped controlling. Actors now
  release what they were holding when the path is exhausted, write the limit that is still
  owed, and report the peer: `GuardEvent::PeerUnresponsive` with what went unanswered, and
  `charging::GuardEvent::Unresponsive`. An unacknowledged limit is recorded in the audit log
  as unaccepted, because the acknowledgement is the evidence and its absence is the fact.
  (D92)

- **`outOfRange` measurements were handed back as numbers to act on.** MPC §2.5.2 and MDT
  §2.5.1 list the three value states together and say the same of both abnormal ones: "out
  of range: value is out of range and **SHALL be ignored** by the Monitoring Appliance",
  "error: value is erroneous and SHALL be ignored". `Reading::usable()` filtered only
  `error`, and a test asserted the mistake in as many words — "out of range is still a
  reading; only `error` invalidates the number". It is not. An out-of-range grid connection
  point reading was reaching an energy manager as a number to plan with; it now returns
  `None`, and the raw `value` field still carries the number for a display, which is a
  different act. Found while implementing MDT, whose §2.5.1 is the same paragraph. (D91)

**Every identifier a client writes to is the peer's, and four of them were this crate's
own.** Found by asking what a peer that is not built out of this crate would do with the
datagrams we send it — which no test in this repository could have answered, because both
ends of every one of them are this crate and so both ends agreed on the numbers.

The specification is explicit and consistent about this. `limitId` is `<l1#(1..1)>` (LPC/LPP
Table 22), the failsafe keys are `<k1#(1..1)>` and `<k2#(1..1)>` (Table 24), the curtailment
factor's key is `<k1#(1..1)>` (MGCP Table 23), a car's per-phase limits are `<x1>`…`<x3>`
(OPEV/OSCEV Table 6). "SHALL be used as the primary identifier" says the *device* keeps its
number stable; it does not say what the number is. What the specification fixes instead is
what each entry *means* — a limit's type, category, direction and scope; a configuration
key's `keyName` — and that is what a client has to match on.

None of these failures produce an error. The write is well-formed, it names a real entry of
the peer's, and the peer acknowledges it.

- **An Energy Guard wrote the §14a limit to `limitId` 1** and never read the peer's limit
  description at all. A Controllable System that serves LPC *and* LPP from one `LoadControl`
  feature — an appliance with a battery — publishes two limits, and which of them is `1` is
  its own business: the guard would limit the wrong direction, be acknowledged, and record
  a limitation that never happened. `EnergyGuardActor::attach` now reads
  `loadControlLimitDescriptionListData` and `deviceConfigurationKeyValueDescriptionListData`
  alongside the bindings, and nothing is written until the identifiers come back. New
  `limitation::PeerIds`, `limitation::find_limit_id`, `limitation::find_key_id` and
  `EnergyGuardActor::peer_ids`.
- **The failsafe values went to `keyId` 1 and 2.** A `DeviceConfiguration` feature holds
  every configuration key a device has, not this use case's two; on a heat pump, key 1 is
  as likely to be something else entirely. `write_failsafe_limit` and
  `write_failsafe_duration` now resolve the peer's keys by `keyName` and return `None`
  until they are known.
- **A guard read a peer's own reported limit by `limitId` 1 too**, so `observe_applied`
  either saw nothing — and kept rewriting a limit already in force — or read some other
  limit of the peer's as the answer to its own.
- **A Monitoring Appliance read MGCP scenario 1 from `keyId` 1.** `mgcp::read_curtailment`
  matched this crate's own key rather than the `pvCurtailmentLimitFactor` name the
  specification fixes. Against a connection point with several configuration keys it
  returns another key's number — in range, plausible, and on its way to becoming a §9 EEG
  export ceiling. It now takes the identifier it is reading; new `mgcp::Curtailment` holds
  the description beside the value and finds it, and `mgcp::find_curtailment_key` is the
  pure form.
- **An Overload Protection manager wrote a car's per-phase currents to `limitId` 1, 2 and
  3.** A car that numbers them differently has phase B curtailed while phase A — the one
  the supply was worried about — stays at full current, and acknowledges it. New
  `charging::PhaseLimits` composes the car's limit descriptions with its parameter
  descriptions to get phase → `limitId`, keeping `obligation` and `recommendation` apart
  so an overload limit is never written as advice; `OverloadGuardActor::attach` reads what
  it needs and writes nothing until it has it.

A second sweep, of every remaining place a client reads or writes a peer's list, found
four more:

- **An energy manager read a car's commissioning data by `keyId` 1 and 2** (EVCC scenarios
  2 and 3). `keyName` is what Table 6 fixes — `communicationStandard`,
  `asymmetricChargingSupported` — and a car with other configuration keys has its own
  numbering. Reading key 1 hands whatever string is there to
  `CommunicationStandard::read`, and a manager that finds a matching word concludes the car
  speaks ISO 15118 when it does not, then asks it for a state of charge nothing answers.
- **And a car's charging power band by `parameterId` 1** (EVCC scenario 6, Table 11). The
  parameter is the one whose description says `acPowerTotal`.
- **`charging::read_permitted_range` took the first permitted value set in the list**,
  whatever parameter it belonged to. Table 9's sets are per phase and addressed by
  `parameterId`; a car that also publishes a power parameter would have its charging
  currents clamped into a range in watts, and be written a "curtailment" of 11 000 A.
- **`cob::control_mode_payload` addressed the inverter's Active Control Mode by `keyId`
  1** (COB Table 20, `<k1#(1..1)>`), and `read_control_mode` read it back the same way.

`tests/foreign_identifiers.rs` is the regression test the crate could not previously have
had: a Controllable System built by hand, serving both directions from one feature, with
its limit on `7` and its failsafe keys on `5` and `6`. The same shape is tested for MGCP in
`tests/monitoring_both_actors.rs` — a connection point with a decoy `peakPowerOfPvSystem`
on key 1 — and for EVCC and OPEV in their modules' own tests.

- **The conformance suite scored MGCP scenario 1 on an exchange that never happened.**
  `MonitoringPair::curtailment` read the factor out of the *Grid Connection Point's own*
  store, and nothing was subscribed to the feature it is served from, so
  `ATC_MGCP_SCE1_PT_MAPowerLimitFactor_001` — a Monitoring **Appliance** test case — passed
  without a datagram crossing. The harness now reads the description, subscribes, and
  resolves the factor from what arrived. The reported coverage is unchanged (189/203, 93 %);
  what changed is that the MGCP Monitoring Appliance's 29/29 is now true.
- **`conformance`'s own documentation said "roughly a third of the LPC and LPP catalogues"
  are device-level.** It is seven of each — fourteen of the two hundred and three cases, or
  seven per cent. A consumer scoping a device harness from that sentence would have planned
  for four times the work.
- The changelog dated 0.4.0 as unreleased after it had been tagged.

### Added

- **The DHW pair (`usecases::hvac`), all four actors.** The first HVAC use cases here, and
  between them the only *control* lever in the crate that can ask an appliance for **more**:
  LPC, LPP, OPEV, OSCEV and COB all set ceilings, and a ceiling an appliance is already
  under changes nothing. A hot water tank is the cheapest thermal battery most buildings
  have, and raising its setpoint while the roof exports is not expressible as a limit.

  **`hvac::mdsf` — Monitoring of DHW System Function.** Which operation mode the circuit is
  in (`auto`, `on`, `off`, `eco`, exactly one enabled [MDSF-001]) and whether a one-time
  heating is overriding it. `DhwSystemFunction` reads all six functions; the system function
  is found by `systemFunctionType: dhw` because a heat pump serves heating and hot water
  from one `HVAC` feature, and reading the heating circuit's mode as the tank's is a manager
  that thinks the water is off while the house is being heated. `OverrunReport` encodes
  Table 14's transient: `finished` **may only be a notification, sent once**, becoming
  `inactive` afterwards and never appearing in a reply — a circuit that leaves it standing
  tells every later reader that a heating has just completed, repeatedly. The status is
  modelled as three resting states with the announcement handed back separately.

  **`hvac::cdt` — Configuration of DHW Temperature.** The first HVAC use
  case here, and the only *control* use case in the crate that can ask an appliance for
  **more**: LPC, LPP, OPEV, OSCEV and COB all set ceilings, and a ceiling an appliance is
  already under changes nothing. A hot water tank is the cheapest thermal battery in most
  buildings, and raising its setpoint while the roof is exporting is not expressible as a
  limit.

  One scenario, mandatory for both actors. `DhwSetpoints` is the Configuration Appliance's
  reader: a `Setpoint` feature carries every setpoint the device has — a room air
  temperature is the same `valueAbsolute` in the same `degC` — so which identifier is the
  hot water comes from `scopeType: dhwTemperature`, its range and step from
  `setpointConstraintsListData`, and which mode uses it from
  `hvacSystemFunctionSetpointRelationListData`. `write` refuses a temperature outside the
  published range rather than letting the circuit answer with a bare error number; a value
  off the *step size* is not refused, because Table 8 makes rounding the server's job.
  `relation_is_valid` holds §2.3.1.1's three different rules on how many setpoints each
  operation mode may relate to.

  **Why both.** CDT §2.4.2–2.4.3: a DHW Circuit that does not serve "Monitoring of DHW
  System Function" SHALL serve "Configuration of DHW System Function" [CDT-005]. One of the
  two is mandatory, because CDT's setpoints are addressed *through* the operation modes —
  Table 10 relates each mode to the setpoints it reads, so "write 60 °C" is only a complete
  instruction once the mode is known. `DhwSystemFunction::current_setpoints` is that join,
  and without it a write lands on a setpoint the circuit is not reading, is acknowledged,
  and changes nothing measurable. Both specifications also give an entity one `HVAC` feature
  (§3.2.2.2.1), so `mdsf::with_cdt` is the single feature a circuit serving both publishes.
  `tests/hot_water_over_the_wire.rs` runs all of it against a circuit that keeps its room
  air temperature on setpoint `1` and its hot water on `3`.

  **`hvac::mdt` — Monitoring of DHW Temperature.** What the water actually got to, which is
  a different number from what was asked for: it depends on the mode, on the circuit's own
  step size, and on whether somebody has just had a shower. It has no reader of its own —
  it is a `Measurement`, so the machinery that resolves MPC and MGCP resolves this too, via
  the new `monitoring::Quantity::DhwTemperature`. That quantity is deliberately distinct
  from `Temperature`, which is a `componentTemperature` on electricity: MDT Table 7 fixes
  `commodityType: domesticHotWater`, the one measurement in this crate that is not power,
  and a client filtering on the commodity would not find a tank published as electricity.
  CDT's setpoint `measurementId` is required to be the one MDT publishes (§3.2.1.2.2.1), so
  `cdt::setpoint_description_measuring` takes `mdt::MEASUREMENT_ID` and an appliance can tie
  a reading to the setpoint that governs it.
- **`usecases::addressing`.** The two resolvers that are not specific to one use case, in
  one place with the reasoning written down once: `KeyIds` for a peer's
  `DeviceConfiguration` keys and `ParameterIds` for its `ElectricalConnection`
  parameters. `KeyIds::name_of` is the direction a reader needs — a
  `deviceConfigurationKeyValueListData` carries identifiers and no names at all — and
  `ParameterIds::by_scope` deliberately answers `None` when *several* parameters match,
  because an ambiguous answer is the one case where guessing is worst: both candidates are
  real and only one is meant.
- **`emobility::charging::ChargingBand`** — a car's permitted band **per phase**, which is
  how Table 9 is actually published. `ChargingCurrents::clamped` now holds each phase
  inside its own band, falling back to the band permitted everywhere for a phase the car
  did not describe.
- **`emobility::evcc::EvReader`** — the manager's side of EVCC, split from the value type.
  It keeps the car's descriptions, keeps values that arrive before them, and resolves the
  two against each other whenever either moves, so neither reply has to be the one that
  comes second.
- `cob::CONTROL_MODE_KEY_NAME`, the fixed half of COB Table 20.
- **`GuardEvent::NoLimitPublished`.** A Controllable System whose `LoadControl` feature
  describes no limit for this guard's direction is an installation that looks commissioned
  from both ends and is not — a device that implements the other direction, or an actor
  built and never installed. The read succeeds, so nothing is reported as an error and the
  guard simply never writes. This is the one place that is visible, and it is reported once
  per attach. Both simulators print it.
- **MGCP scenario 1 in `MonitoringApplianceActor`.** `MonitoredUnitPeer` gains
  `curtailment`, which `monitoring::locate` fills from discovery; `attach` reads the
  description and subscribes; the factor arrives as `MonitoringEvent::CurtailmentChanged`
  and is available as `curtailment` and `feed_in_limit`. A consumer playing the Monitoring
  Appliance no longer builds a feature or parses a payload for itself. The reason there is
  no `mgcp::curtailment_client_feature` beside `curtailment_feature` is now written where
  it is looked for: LPC IG §3.3 asks an actor to hold **one** `Generic` client feature for
  all of its client functionality, and `limitation::client_feature` is it.

### Changed

**Breaking.** The crate is unpublished; each of these removes a way to address the wrong
data.

- `limitation::read_limit_write` takes the `limitId` it is reading.
- `mgcp::read_curtailment` and `mgcp::FeedInLimit::from_data` take the `keyId` they are
  reading. Prefer `mgcp::Curtailment`, which finds it.
- `charging::limit_data` and `charging::deactivated` take a `&PhaseLimits`;
  `charging::EvPeer` gains `purpose`, because a car serving both use cases publishes two
  limits per phase and they must not be confused.
- `EnergyGuardActor::is_ready` is false until the peer's `limitId` is known, and
  `GuardEvent::Ready` follows the description read where it arrives after the bindings.
- `limitation::LIMIT_ID`, `FAILSAFE_LIMIT_KEY`, `FAILSAFE_DURATION_KEY`,
  `mgcp::CURTAILMENT_KEY` and `charging::limit_id` are documented as what this
  implementation publishes for itself, and are not to be used to address a peer.
- `GuardEvent` gains a variant, so a `match` over it needs a new arm.
- `evcc::EvProfile::apply` is gone; `EvReader::apply` replaces it and `EvReader::profile`
  is the value. The car's half of `EvProfile` is unchanged.
- `charging::read_permitted_range` takes a `&ParameterIds` and returns a `ChargingBand`;
  `ChargingCurrents::clamped` takes a `&ChargingBand`;
  `charging::GuardEvent::Ready` carries `band` rather than `range`, and
  `OverloadGuardActor::range_of` is now `band_of`.
- `cob::control_mode_payload` and `cob::read_control_mode` take the `keyId` they address.
- `limitation::find_key_id` moved to `usecases::addressing::find_key_id`, where the other
  use cases can reach it.
- `tests/fixtures/golden/lpc_exchange.txt` re-recorded: the guard's two description reads
  are new on the wire.

## [0.4.0] — 2026-09-03

### Fixed

- **An Energy Guard could not send an open-ended limit.** To write a limit with no
  duration it omitted the `timePeriod`, and under the partial concept an omitted element
  means *unchanged* — so the Controllable System kept the previous end time, and a limit
  meant to be indefinite lapsed when the old one would have: the household returned to
  full draw while the operator's record said otherwise. LPC UC TS §3.4.1.4 requires a
  `delete` filter alongside the write and gives the combined command; the engine has
  always served that shape, but nothing could send it. New `Engine::write_filtered`, and
  the guard now sends the delete whenever the limit it writes carries no duration.
- **A pending SHIP node re-announced instead of aborting when its Wait-For-Ready-Timer
  expired.** SHIP §13.4.4.1.3, `SME_HELLO_STATE_PENDING_TIMEOUT` rule 1, has no exemption
  for a node waiting on a user: the timer aborts. What was actually missing was the other
  half of the rule — a peer's `ready` **retires** this node's own Wait-For-Ready-Timer
  (`PENDING_LISTEN` rule 2), and from then on the connection is held up by prolongation
  requests against the *peer's* timer, which is what gives a person as long as the peer
  will allow. A `ready` carrying no `waiting` leaves nothing to prolong and now aborts
  (rule 1). A ten-minute commissioning still completes.
- **A payload with two commands was executed in part.** SPINE §5.3.2 permits exactly one
  `cmd` per payload; the schema says `1..unbounded`, so a peer can send more, and the
  engine ran each of them and answered with the worst outcome. One `result` cannot report
  two outcomes, so a peer told "applied" about a pair of writes of which one failed had
  been told something untrue — under §14a the difference between evidence and a guess.
  Such a datagram is now refused whole with `errorNumber` 1 and neither command reaches
  the application. Several *filters* in one command are unaffected.

**A second full audit, of the runtime and the engine's authority checks.** Two of these are
structural and the rest are defects nobody had reached; each is written up in
`concepts/DECISIONS.md` (D75–D82).

- **Pairing could not happen while a peer waited.** The SHIP handshake held an unapproved
  peer `pending` and had `set_trust` for the answer since 0.1, and no runtime path ever
  called it: `Node::accept` ran to completion or abort and exposed nothing in between, so
  the only way to pair was to know the SKI before the connection. The handshake driver now
  waits on the trust store beside the socket and the SHIP timer, so an approval completes
  the handshake it is waiting in and a refusal ends it with `hello: aborted`. The hub reports
  the waiting peer as `HubEvent::TrustRequested` and answers with `Hub::approve` and
  `Hub::refuse`. (D75)
- **The hub dialled and accepted inline, and a peer that was down starved every heartbeat.**
  `redial` awaited a ten-second connect from inside `next`, one offline peer per turn;
  a control box with ten unplugged devices stopped reading for the better part of a
  backoff round, and every appliance that *was* connected fell into its failsafe state for
  want of a heartbeat. Handshakes now run in tasks of their own and report back over a
  channel the loop waits on alongside the sockets. (D76)
- **A paired peer could release another peer's binding.** A binding or subscription call
  names its client in the payload, and nothing checked that the address belonged to the
  device SHIP authenticated as the sender. It has to now, or the call is refused with
  `errorNumber` 7; a call that leaves the device part out is completed from the header,
  which keeps `spine-go`'s spelling working. (D77)
- **Bindings and subscriptions were unbounded**, which D46 had missed: a peer chooses its own
  client addresses, so a paired peer could grow both tables one call at a time forever.
  `spine::MAX_RELATIONS_PER_PEER` bounds each kind per device; a peer evicted under
  `MAX_PEERS` now takes its relations, requests and undecided writes with it. (D78)
- **`Engine::accept_write` could not say that it had failed.** A use case that accepted a
  write the engine then could not store had already written "accepted" into the §14a
  record while the peer was told `errorNumber` 3. It returns `Result<(), WriteError>` now,
  `reject_write` returns whether the token was live, and the limitation and charging actors
  record what the peer was told — `NackReason::NotStored` — and put the state machine
  where a refused write leaves it. (D79)
- **Any peer's heartbeat kept a Controllable System out of its failsafe state.** The actor
  took every `deviceDiagnosisHeartbeatData` it was notified as the guard's; it now counts
  only the diagnosis feature of the entity that holds both bindings (§3.8). (D80)
- **Discovery was filed under the payload's device address, and a payload could rename a
  record.** The header's source — what SHIP authenticated — is the key now; the payload
  fills a record that has no address yet and nothing else. (D81)
- **A `write` with no command was acknowledged as a success.** It is `errorNumber` 1. (D82)
- The Controllable System's heartbeat now runs from the moment `install` puts it on the
  wire rather than from the moment its builder was made.

### Added

- **The SHIP Pairing Service, end to end, in both roles.** Previously the crate had the
  digest and the replay guard and nothing joined them to a network; now an installer's
  configuration is the whole of commissioning. `devA` — the household energy manager —
  turns it on with `Hub::accept_pairing_requests(Receiver)` plus `Hub::browse_pairing`,
  and an authentic request arrives as `HubEvent::Paired { unit, displaced }`; one
  addressed to this node that fails arrives as `HubEvent::PairingRefused`, which is the
  mistyped-secret case §5.5 expects to be corrected, while a request for another node is
  not reported at all. `devZ` — the control unit — builds it from the other device's QR
  payload and `Requester` decides when it is on the air (§4.2): from the moment it is
  configured — not from the first connection, since `devA` cannot trust `devZ` until it
  has heard the request — across interruptions, until one uninterrupted connection has
  held fifteen minutes. Both simulators
  do it over a real network; `examples/heat_pump.rs --pairing` and
  `examples/steuerbox.rs --pair-with '<QR payload>'`.

  What it required, and what is new with it:

  - **`ship::Fingerprint`** — the SHA-256 of a DER certificate, which is the identity the
    Pairing Service trusts. Strict about the 64-uppercase-hex wire form on the way in,
    because the digest covers the text as sent. `Identity::fingerprint`,
    `ShipTls::fingerprint`, `Node::fingerprint`, `Hub::fingerprint`,
    `tls::peer_fingerprint` and `ShipConnection::peer_fingerprint` expose it; `ShipQr`'s
    `certificate_fingerprint` is now typed as one.
  - **`runtime::PairedUnit`** — a control unit trusted by certificate, as a first-class
    trust-store entry rather than a `TrustedPeer` with an invented SKI. The request names
    no SKI and one cannot be derived from a fingerprint. `TrustStore::trust_unit`,
    `unit`, `forget_unit`, `is_certificate_trusted`; `TrustedPeer` gains `fingerprint`.
    A peer is admitted on **either** identity, which §10.2 makes equivalent, and the SKI
    is recorded once a connection has proved one. One unit at a time (§10.3): pairing a
    second untrusts the first, its SKI included, and closes its connection.
  - **`pairing::Receiver`** (§9, the four steps), **`pairing::Requester`** (§4.2),
    **`PairingAnnouncement`** with `to_pairs`/`from_pairs` (§5.4 — `txtvers` first,
    unknown keys ignored, every mandatory value held to its pattern), **`pairing::Nonce`**,
    `PairingRequest::sign`, `ReplayGuard::contains`, `SETTLED_AFTER`,
    `REPLACEABLE_AFTER`.
  - **`mdns::PairingEvent`, `PairingBrowse`, `Mdns::withdraw_pairing`** and an
    `announce_pairing` that takes a signed announcement rather than a SHIP TXT record.
  - **§4.3 lives in the hub**, because whether SHIP messages are flowing with the paired
    unit is the one thing only the connection table knows: requests are not processed
    while the pairing is working, and are taken up again after fifteen minutes of being
    unable to reach it. That is what stops a captured announcement breaking a pairing that
    is doing its job, and what lets a broken control unit be replaced without a factory
    reset.
  - The trust store's JSON is one object with `peers` and `unit` rather than a bare
    array — §10.4 asks the Pairing Service's trust to live in the same store as SHIP's,
    and a device writes one file. `TrustStore::to_json`/`from_json` handle both halves,
    and `forget_all` counts the unit.
  - **`tls::random`** — the backend's cryptographic generator, which §6.3 asks for behind
    every nonce and secret.
  - `PairedUnit::new`, for a store restored from disk. `PairedUnit::from_request` needs
    `pairing`, and every pairing-only part of the hub is behind `mdns` **and** `pairing`,
    so `runtime` on its own still builds.
- **`HubEvent::HandshakeFailed` names the peer where one was proved.** An accepted
  connection that failed after TLS reported `ski: None`, so a refusal could not be told
  from a peer that never presented a certificate.
- **`runtime::MAX_PENDING_TRUST`** — at most four peers may wait on an approval at once.
  A peer held in `hello: pending` occupies a connection slot for minutes on nobody's
  authority, and a handful of them held every slot; the one past the cap is now answered
  `hello: aborted` immediately and reported as `ConnectionError::TooManyPendingPairings`,
  so it retries rather than squatting. Closes R13.
- **Interactive pairing, end to end.** `HubEvent::TrustRequested { ski, origin }` for a peer
  waiting in the SHIP pending state, `Hub::approve` and `Hub::refuse` to answer it, and the
  same through a `Node` driven by hand: `TrustStore` wakes a pending handshake on every
  change (`TrustStore::watch`), and `Node::refuse_pairing` ends one. The two simulators ask
  on the terminal — `y` to pair — which is what pressing the button on a real device
  amounts to. `tests/runtime_over_a_socket.rs` drives both answers over loopback.
- **`Hub::listen`, `Hub::dial` and `Hub::browse`**: the listener, every dial and the mDNS
  browse are the hub's own now and run in the background; what they produce arrives as
  `HubEvent::Connected`, `HandshakeFailed { origin, ski, error }`, `Found { peer, trusted }`
  and `Lost { instance, ski }`. `runtime::Origin` says which end dialled and where the peer
  is. A trusted peer mDNS finds is dialled and kept dialled; an untrusted one is kept aside
  and dialled the moment `approve` names it. `runtime::CONNECT_TIMEOUT` bounds reaching a
  peer and not the handshake, which has SHIP's own timers and may wait for a person.
- **The device-level test cases as data.** `conformance::DEVICE_LEVEL`,
  `conformance::device_level()` and `AbstractTestCase::owed_by_device` carry the fourteen
  abstract test cases no library can answer — a factory reset, a power cut, a start-up
  duration, what the appliance draws — with the reason each is the device's. A harness
  driving a real device iterates them instead of transcribing them; this crate's own suite
  derives its uncovered set from the same table and asserts the two agree.
- `spine::MAX_RELATIONS_PER_PEER`, `spine::WriteError`, `NackReason::NotStored`,
  `ControllableSystem::on_unstored_limit_write`.
- Four engine tests for the authority rules: a relation cannot be released in another
  peer's name, an empty write is not acknowledged, an unstorable acceptance is reported,
  and discovery is filed under the header.

### Changed

**Breaking.** The crate is unpublished; each of these removes a way to get something wrong.

- `Hub::connect` is gone: `Hub::dial(SocketAddr)` starts a dial and returns at once, and
  `HubEvent::Connected` carries the SKI. `Hub::accept(TcpStream)` returns at once too, and
  `Hub::listen` replaces a hand-written accept loop. `HubEvent` no longer derives
  `PartialEq`, because `HandshakeFailed` carries the `ConnectionError`.
- `Engine::accept_write` returns `Result<(), WriteError>`; `reject_write` returns `bool`.
- `TrustStore` no longer derives `PartialEq`; compare `peers()` instead. Its JSON is now
  an object with `peers` and `unit` rather than a bare array of peers, so a store written
  by 0.3 does not load — re-approve, or wrap the old array as `{"peers": […]}`.
- `ShipQr::certificate_fingerprint` is `Option<Fingerprint>` rather than
  `Option<String>`, and a `FPH256` field that is not 64 uppercase hex digits now makes the
  payload unreadable rather than being carried through.
- `ship::pairing` is reshaped: `PairingRequest`'s `for_par`/`trust_par` are
  `Fingerprint` and `trust_nonce` is a `Nonce`; `to_pairs` moved to
  `PairingAnnouncement`, which `sign` produces; `verify` no longer records a digest that
  did not verify, and the authenticity check alone is `verify_digest`.
- `Mdns::announce_pairing` takes a `PairingAnnouncement`, not a `ShipTxtRecord`;
  `Mdns::browse_pairing` returns a `PairingBrowse` of `PairingEvent`, not a `Browse` of
  SHIP nodes.
- A payload carrying more than one `cmd` is refused with `errorNumber` 1 (SPINE §5.3.2).
- A pending SHIP node aborts when its Wait-For-Ready-Timer expires, and aborts on a
  `ready` that carries no `waiting` (§13.4.4.1.3).
- A binding or subscription call whose client address names a device other than the
  sender is refused; a peer past `MAX_RELATIONS_PER_PEER` is answered `errorNumber` 3.

## [0.3.0] — 2026-09-02

### Fixed

**Four interoperability defects, all found by `tests/interop.rs` against a live
`eebus-go` peer, none of which any test written against this crate's own output could
have found.** Before this, the crate could not have talked to a real device at all.

- **A read was only understood when it carried a `function` element.** SPINE lets a read
  name its function either with `function` or with an empty instance of the data element;
  `spine-go` — and therefore most of the deployed base — sends the second, and this crate
  answered `errorNumber` 1 to every one. `handle_read` now derives the function from the
  data element when `function` is absent.
- **And a read this crate *sent* carried only the `function` element**, which `spine-go`
  answers `errorNumber` 6 to. The specification's own example reads carry **both** —
  `tests/fixtures/spine/RFE_RD-*.json` — so this was a deviation from the specification as
  well as from the reference implementation. `Cmd::read` now carries the empty data element
  too. The knock-on effect was the interesting part: our reads failing meant we never
  discovered the guard's `DeviceDiagnosis`, never subscribed to its heartbeat, and then
  correctly refused every limit for want of one.
- **A Controllable System subscribed to the guard's heartbeat from its own *server*
  feature.** A subscription runs client → server (SPINE §5.3.6) and `spine-go` refuses a
  role mismatch outright. New `limitation::device_diagnosis_client_feature`, and
  `CsFeatures` gains `device_diagnosis_client`.
- **An entity could not hold two features of the same type in different roles.**
  `LocalEntity::add_feature` read implementation guide §3.4 as "one feature per type"; it
  means one per type *and role*, since that is exactly what discovery reports. The
  over-strict reading made real devices unrepresentable — `evcc` and the Porsche Mobile
  Charger Connect both carry a `DeviceDiagnosis` server and client on one entity, and
  Porsche does the same with `DeviceClassification`, all visible in
  `tests/fixtures/devices`.

- **The byte-for-byte round-trip claim was stated without its scope**, and real traffic
  showed why that matters. SPINE 1.3.0 defines a *restricted* address type inside detailed
  discovery carrying only the entity path; `eebus-go` — and therefore evcc — puts a
  `device` in it anyway. The model is generated from the schema, so the field has nowhere
  to go and is dropped.

  Dropping it stays: it is redundant, since the enclosing `deviceInformation` names the
  device, and refusing would mean refusing to talk to evcc. What changed is that the claim
  now says "for a schema-valid message", and that the gap is **measured** — a test asserts
  which captures are not reproduced exactly, so a second one appearing is a failure rather
  than a discovery.
- **The documentation put NodeManagement on feature 1.** It is feature 0, which is what the
  code has always done and what all eight devices in the new corpus do. Two pages said
  otherwise.
- **`ScaledNumber::to_f64` was inexact for a negative scale.** It multiplied by `10^scale`,
  and no negative power of ten is representable in binary, so the value rounded twice —
  once building the constant, once in the product. `12345 × 10⁻⁴` read as
  `1.2345000000000002`. It now divides by the exact positive power, which rounds once, and
  the powers of ten come from a table rather than from repeated multiplication. Two
  spellings of the same quantity now agree, which is what stops a limit changing because a
  peer chose a different scale for it.
- **`ScaledNumber::from_f64` saturated instead of scaling.** A value beyond `i64` reached
  the wire as `i64::MAX` — for `1e30`, a different number by a factor of nine. It now raises
  the scale until the mantissa fits, which is what the scale is for. The tolerance for
  "close enough to an integer" is relative rather than an absolute `1e-9`, which was
  meaningless beside a megawatt and impossibly strict beside a microamp.
- **The double-connection fallback could not terminate.** The smaller-SKI node pinged every
  connection and, if they all answered, pinged again — for as long as the peer stayed up.
  Against a peer that has stopped arbitrating, or applies `ship-go`'s rule instead, the
  duplicate was never resolved. It now runs exactly one ping round and then decides, and the
  decision is a pure function of `ship::resolve` rather than a branch inside the socket loop.
- **`Hub::next`'s cancel-safety warning named the wrong half.** Reading is cancel-safe —
  `WebSocketStream` buffers a partial frame — and the *sending* half is not. A cancelled
  `next` leaves half a frame on the wire, which puts the peer's parser out of step with the
  stream. The rule is unchanged; the reason was wrong.

**Sixteen defects found by a full audit of the protocol core**, none reachable from a test
the crate already had. Six are on the receiving side; six are claims the documentation made
that the code did not keep. Each is written up in `concepts/DECISIONS.md`.

- **A `delete` ignored its filter, and a command that both deletes and writes did only the
  delete.** LPC UC TS §3.4.1.4's example carries two filters — one withdrawing a limit's
  `timePeriod.endTime`, one writing its new value — and the engine answered the first and
  dropped the second, lifting a curtailment that should have become open-ended. A command's
  filters now run in order, and a delete's `selectors` and `elements` are honoured. New
  `LocalFeature::delete_filtered` and `CmdData::delete_restricted`. (D60)
- **A partial notification handed the fragment to the use case.** An omitted `scale` means
  "unchanged", so a measurement notified as a bare `number` was read off by a power of ten.
  The engine keeps the merged state of every function a peer has sent, and `DataNotified`
  and `ReplyReceived` carry `resolved` alongside `data`. (D66)
- **Two of the seven corpus devices resolved to zero use cases.**
  `useCaseInformation.address` is optional and absent from every SPINE 1.1.1 peer, and
  `apply_use_case_data` required it. `RemoteUseCase::address` is now an `Option`, resolved
  from the features instead. (D67)
- **A partial detailed-discovery notification erased what it did not mention** — the peer's
  device type, entity list and SPINE version list. (D66)
- **A scale of 23 or more was wrong by a factor of ten.** The exact table of powers of ten
  stops at `10^22` and the assembly past it was off by one. A scale is now applied to the
  value in exact steps. (D62)
- **A four-second safety fallback could be waited on for ever.** `EvCharging::source` used
  `>` and `HeartbeatMonitor::is_alive` used `<=`, so waiting until the instant
  `poll_timeout` published produced no transition — and the same instant again. Both fire at
  the published instant now. (D74)
- **An undecided write was never a deadline.** `Engine::poll_timeout` reported only requests
  the device had *sent*, so a Controllable System never expired one and the
  `MAX_DEFERRED_WRITES` queue filled. `Engine::remove_peer` also releases a departed peer's
  writes. (D63)
- **A subscription to NodeManagement could never be served**: its functions are computed
  rather than stored, which `handle_read` knew and `notify` did not. (D61)
- **A response from one peer could answer a request sent to another.** `msgCounter` is
  allocated per engine, so a response now resolves a request only when it comes from the
  device the request went to. (D64)
- **A peer's device address was never checked.** It is a routing key and a stored identity,
  and neither its length nor its character class was bounded. New
  `spine::is_usable_device_address`; `validate_device_address` stays the full §7.1.1.2 check
  for what this node builds. (D73)
- **A peer's announced `maxResponseDelay` was parsed and ignored**, so a peer answering
  within the minute it announced was reported as timed out — which is what §2.6.2's
  staggered retry follows. Both directions are honoured now. (D71)
- **`{:?}` printed the secrets the documentation said it redacted.** `ShipQr`,
  `PinRequirement` and `HandshakeConfig` all derived `Debug`; `PairingSecret` derived
  `PartialEq`. Each redacts and compares in constant time now. (D68)
- **A `delete` was served on a feature announcing whole-function writes only.**
  `possibleOperations` carries one `partial` flag for the whole of §5.3.4; it is answered
  `errorNumber` 8. (D60)
- **`ShipConnection::next_message` documented the wrong reason for being cancel-safe** —
  "performs no writes", when `tungstenite`'s `read` sends pong and close responses. It is
  cancel-safe, for a different reason, now named. (D72)
- **`Hub::next` cancelled mid-write corrupted the peer's stream silently.** A write in
  flight is marked across its `await`, so the next call closes that connection with
  `Disconnect::InterruptedWrite`. (D70)
- **`self_signed`'s rustdoc summary was a warning**, because the doc block opened with a
  heading.

### Added

- **`tests/interop.rs`: this crate against a live `eebus-go` peer, over a real socket, in
  both directions.** A container runs `eebus-go`'s own examples at a **pinned** revision —
  `examples/controlbox` as an Energy Guard, `examples/evse` as a Controllable System — and
  this crate stands up the other half of each. What is asserted is the whole §14a exchange:
  TLS 1.2 with mutual authentication, the five-phase SHIP handshake, SPINE discovery, the
  binding and the subscription, and a consumption limit written, accepted and recorded.

  The direction where *this crate* writes the limit is the more demanding one — the 2026
  implementation guides spend most of their pages on the controlling side, and until now the
  Energy Guard had only ever been exercised against an appliance written by the same hands.
  **Its rules held first time**: the heartbeat before the write and only once the peer had
  subscribed (§2.11), the opening write when the bindings settle, the retry schedule. So did
  peer location in a case this crate does not itself produce — `eebus-go` names its use case
  at *entity* granularity, omitting the optional feature part of
  `useCaseInformation.address`, and the lookup already resolved by entity.

  It is behind `--features interop` and `required-features` on the test target, so an
  ordinary `cargo test` stays hermetic, needs no Docker and finishes in seconds. It runs as
  its own CI job. **It deliberately does not use `testcontainers`**: this crate's small,
  auditable dependency set is most of what it offers a manufacturer facing the CRA, and
  driving the `docker` CLI is forty lines of `Command` and no dependency at all.

  Four defects fell out of the first runs, all on the appliance side; see *Fixed*.
- **The crate has met other implementations.** Every test here used to exercise `eebus`
  against itself, which proves the encoder agrees with the decoder and nothing else.
  `tests/fixtures/devices` now carries fifteen datagrams captured from **eight real devices
  by seven manufacturers** — Elli, evcc, Kostal, Porsche, SMA, Spelsberg, Vaillant,
  Viessmann — recorded with `eebus-go` and published as MIT-licensed
  [`enbility/devices`](https://github.com/enbility/devices). `cargo run -p xtask -- devices`
  converts them from the ordinary JSON they are stored as into the JSON-UTF8 of SHIP §11.4.

  `tests/real_devices.rs` asserts that every one parses, that its header is one this crate
  would accept off a socket, that detailed discovery resolves into entities and features
  across at least eight feature types, and that use-case discovery resolves into the twelve
  or more use cases the corpus names — including all four certifiable ones. CI seeds the
  fuzz corpora from them too, so the fuzzers explore shapes real hardware produces rather
  than shapes this crate produces.

  It found two things on the first run. One is a defect — see *Fixed*. The other is that
  **`evChargingSummary` is played by four of the seven devices in the corpus and was not on
  the backlog at all**: a use case that a backlog written by reading specifications had
  missed, and that appeared the moment real hardware was looked at. A test now asserts the
  set of use cases the corpus plays that this crate does not implement, so the backlog
  cannot go stale without a failure.
- **The conformance suite now drives all four certifiable use cases, not two.** MPC's 54
  and MGCP's 47 abstract test cases were carried as data with no test behind them; they
  are now driven through a real Monitored Unit and Monitoring Appliance exchanging real
  datagrams. Coverage went from **88/102 over two use cases to 189/203 over four (93 %)**,
  and the fourteen that remain are exactly the seven device-level cases — a factory reset,
  a power cut, a start-up duration — counted once for LPC and once for LPP.

  A test asserts that equality, in both directions: it fails if a case is quietly dropped,
  and it fails if something is marked *device* to make the number go up. **Every abstract
  test case a library can answer is now answered.**

  The 101 measurement cases are a table rather than 101 functions, because every one of
  them is the same four shapes — publishes, resolves, refuses a non-`normal` value, or
  answers a poll — and the copy that drifted would be the one nobody read. A row that
  fails names its own `ATC_…` identifier.
- **`ship::ShipVersion`, and a way to find out which one a session negotiated.** A
  consumer could set the ceiling with `Node::handshake_config` and had no way to observe
  the result — the one thing about a session that could not be found out from the API.
  `ShipConnection::ship_version`, `ShipConnection::message_format`, `Hub::ship_version`,
  and a `version` field on `HubEvent::Connected`. `ShipVersion::V1_0` is the certification
  minimum, `V1_1` what this crate announces; they order and print the way versions do.
- **Resource limits on everything a peer can grow.** SHIP and SPINE cap none of these, and
  the omission is exploitable from a LAN.
  - `runtime::DEFAULT_MAX_CONNECTIONS` (16) and `MAX_CONNECTIONS_PER_PEER` (2), with
    `Hub::max_connections` and `Hub::set_max_connections`. Two per peer is what §12.2.3
    legitimately produces while a double connection is arbitrated; a third is not.
  - `spine::MAX_DEFERRED_WRITES` (16). Beyond it a write is answered `errorNumber` 3 —
    overload — rather than queued against an application that is not deciding.
  - `spine::MAX_LIST_ENTRIES` (128), with `FeatureError::TooManyEntries`. A partial write
    appends any entry whose identifier matches nothing stored, so a bound peer could grow a
    stored list one message at a time — a legitimate protocol flow with no natural end.
  - `CmdData::entry_count`, which is what the cap above counts.
- `SpineEvent::WriteAbandoned`: a deferred write that went undecided past §5.2.5's maximum
  response delay. Its token stops resolving, and under §14a a limit that was never decided
  is worth a log line rather than a silent disappearance.
- `ConnectionError::TooManyConnections`.
- `usecases::limitation::CsFeatures`.
- **`cargo xtask check-floats`**, in CI. Both arithmetic defects below were the same shape —
  a power of ten built by hand, a float-to-integer cast that saturated — in the two
  directories where the wire meets a number. A blanket `no-floats` ban is the wrong tool
  here, since the use-case API is watts and amperes as `f64` on purpose; this bans the
  narrow shape instead, with `values.rs` the one audited exception. Verified by reinstating
  the original defect and watching the guard fail.
- **`cargo xtask devices`**, which converts the MIT-licensed `enbility/devices` captures
  into wire-format fixtures.

- **`Hub::run(handler)`**, a loop that owns `next` and cannot cancel it.
- **`ControllableSystemBuilder`**: `ControllableSystemActor::builder(…).install(…)` is the
  only way to obtain an actor, so the step whose omission published no limit description —
  and so was never sent a limit — cannot be skipped. (D69)
- **`cargo xtask rfe-table`** and the page it writes,
  [Which functions can be exchanged in part](https://hupe1980.github.io/eebus/docs/functions/):
  141 of the 142 functions the payload defines, generated from the committed
  `eebus_restrict!` invocation and regenerated in CI.
- **`tests/specification_fixtures.rs`**: all twenty-nine Restricted Function Exchange
  examples from the specification's annex, served to an engine rather than only decoded.
  `real_devices.rs` resolves every device capture through one too.
- **`tests/deadlines.rs`**: the `poll_timeout`/`handle_timeout` contract, asserted once
  against every state machine.
- `tests/fixtures/golden/mpc_exchange.txt`, a second golden vector for the measurement side.
- `Engine::remote_data`, `spine::MAX_REMOTE_FUNCTIONS`, `LocalDevice::from_address`,
  `LocalFeature::with_max_response_delay`, `RemoteFeature::max_response_delay`,
  `RemoteDevice::feature_at`, `spine::is_usable_device_address`.
- CI runs the whole test suite on `aws-lc-rs` as well as `ring`, and checks that the
  published RFE table is current.

### Changed

- **The Energy Guard attaches itself.** A peer that announces the Controllable System is
  now taken up automatically — the two bindings, the heartbeat subscription, the scenario-4
  read — instead of waiting for `attach`. It has to be: implementation guide §2.11 requires
  the opening limit write as soon as the bindings settle, whether or not anybody has asked
  for a limit, so a guard that waits to be attached leaves a conformant appliance in `init`
  indefinitely. `attach` stays public, and is still what a reconnection uses to restart the
  pre-scenario exchange deliberately.
- **`EnergyGuardActor::require` no longer depends on ordering.** It used to be
  `if let Some(tracked) = … { … }` with no else: a limit required for a device the guard had
  not attached to yet was **silently discarded**. A requirement is a fact about the
  installation rather than about how far the pre-scenario exchange has got, so one set for
  an unknown device is now held and applied the moment that device appears.
  `deferred_requirements()` reports anything still waiting — empty in a healthy
  installation, and under §14a EnWG a device that stays there is one the operator is owed an
  answer about.

  Both changes come from the same finding, which cost an hour twice over while writing
  `tests/interop.rs`: a required setup call that, when missed, produces a session where
  discovery, bindings, subscriptions and heartbeats all succeed and **no limit is ever
  written, with nothing on the wire to say why**.

- **`CsFeatures` gains `device_diagnosis_client`**, and a Controllable System must now
  build a client-role `DeviceDiagnosis` feature with
  `limitation::device_diagnosis_client_feature`. Without it the heartbeat subscription is
  refused by any peer that checks roles, and every limit is then refused for want of a
  heartbeat.
- **`Cmd::read` now carries the empty data element**, so the recorded wire in
  `tests/fixtures/golden/lpc_exchange.txt` changed. That is the golden vector doing its
  job: a behaviourally correct change that alters the bytes.
- **`ControllableSystemActor::new` takes a `CsFeatures` instead of three `FeatureAddress`
  arguments.** All three were the same type and only the order said which was which, so
  passing the heartbeat's address where the limit's belonged compiled, ran, and produced a
  device that answered a grid operator's limit write on the wrong feature. The rest of the
  crate spends distinct newtypes preventing exactly this.
- **`Hub::adopt` returns `Result<Ski, Box<ShipConnection>>`.** A hub with no room hands the
  connection back rather than taking it; `accept` and `connect` close it with a
  `connectionClose` and report `ConnectionError::TooManyConnections`.
- **`ship::resolve` takes a `probed` flag, and `Resolution::PingThenClose` is now
  `Resolution::Probe` and `Resolution::CloseAfterProbe`** — the two steps of a fallback that
  has to run once and then decide.
- **`HandshakeConfig::max_version` is a `ShipVersion` rather than a `(u16, u16)`**, and so
  is what `Handshake::negotiated` returns. A tuple of two numbers says nothing about which
  is the major.
- **`HubEvent::Connected` carries the negotiated `version`.**
- **The conformance catalogue is behind the new `conformance` feature**, on by default. It
  is tens of kilobytes of static strings that a device in the field never reads, and
  `--no-default-features` — what a firmware build uses — now leaves it out.

**Breaking.** The crate is unpublished; each of these removes a way to get something wrong.

- `SpineEvent::DataNotified` and `ReplyReceived` gain `resolved`. Read that, not `data`.
- `RemoteUseCase::address` is `Option<FeatureAddress>`.
- `ControllableSystemActor::new`, `with_electrical_connection`, `with_audit_log` and
  `install` are replaced by `ControllableSystemActor::builder(…)`, the same methods on
  `ControllableSystemBuilder`, and its consuming `install`.
- `ShipTls::client_config`/`server_config` take no arguments and return `Arc<…>`, built once
  and shared. `tls::PeerObserver` is removed; use `tls::peer_ski`.
- `PinRequirement`, `HandshakeConfig` and `ShipQr` no longer derive `Debug`; each has one
  that redacts.
- `model::rfe::delete_elements` and `select` are replaced by `addresses_named`,
  `addresses_selected`, `delete_addressed` and `clear_addressed`.
- `spine::FeatureError` gains a `Restricted` variant, reported as `errorNumber` 8.
- Removed as unused, each a second spelling of something else:
  `spine::addresses_node_management`, `spine::addresses_match`, `MsgCounterSource::peek`,
  `spine::heartbeat::with_timestamp`.

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

[0.9.0]: https://github.com/hupe1980/eebus/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/hupe1980/eebus/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/hupe1980/eebus/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/hupe1980/eebus/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/hupe1980/eebus/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/hupe1980/eebus/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/hupe1980/eebus/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/hupe1980/eebus/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/hupe1980/eebus/releases/tag/v0.1.0
