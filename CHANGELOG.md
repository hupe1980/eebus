# Changelog

Notable changes to `eebus`. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
[semantic](https://semver.org/), with the usual pre-1.0 caveat that a minor bump may break.

## [0.4.0] — unreleased

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

[0.4.0]: https://github.com/hupe1980/eebus/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/hupe1980/eebus/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/hupe1980/eebus/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/hupe1980/eebus/releases/tag/v0.1.0
