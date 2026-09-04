+++
title = "On a network"
description = "The runtime Hub: listening and dialling in the background, interactive pairing, mDNS discovery, routing datagrams to peers, keep-alive, and reconnection with SKI-spread backoff."
weight = 110
[extra]
group = "Deployment"
+++

The protocol core owns no socket and reads no clock, which is what makes every timer in the
standard an ordinary unit test. The `runtime` feature is the part that does own them, and
it is the only part that does.

## The Hub

A `Hub` holds one SPINE engine and every connection to it. It listens, dials and browses in
the background, asks the application only when a trust decision is needed, and hands
everything else over as events.

```rust
let mut hub = Hub::new(node, engine);
hub.listen("0.0.0.0:4712").await?;          // the household end listens…
hub.browse(&mdns)?;                          // …and finds its peers
hub.dial("192.0.2.10:4712".parse()?);        // or is told one

loop {
    match hub.next().await? {
        HubEvent::TrustRequested { peer, .. } => { /* show peer.ski; then hub.approve/refuse */ }
        HubEvent::Found { peer, trusted } => { /* mDNS saw one; trusted peers are already being dialled */ }
        HubEvent::Connected { ski, version } => { /* TLS, WebSocket and SHIP are done */ }
        HubEvent::PeerDiscovered { device, .. } => { /* it has said what it is */ }
        HubEvent::Spine(event) => { actor.handle_event(hub.engine_mut(), &event, hub.now()); }
        HubEvent::Tick => { actor.handle_timeout(hub.engine_mut(), hub.now()); }
        HubEvent::Disconnected { ski, .. } => { /* the session is gone, the use case is not */ }
        HubEvent::HandshakeFailed { origin, error, .. } => { /* a dial found nobody, a peer refused us */ }
        _ => {}
    }
    hub.wake_at(actor.poll_timeout());
}
```

What it does for you:

* runs every TLS and SHIP handshake **off the loop**, so a peer that is slow, unreachable
  or waiting for a user holds nothing else up — a control box with ten devices to redial
  keeps its heartbeats going to the nine that answer;
* routes each datagram the engine produces to the peer it addresses;
* runs the opening discovery, so an application hears about a peer only once it knows what
  that peer *is*;
* keeps idle connections alive with the pings SHIP §10.4 asks for;
* resolves the double connections of two nodes that dial each other at once (SHIP §12.2.3);
* follows certificate updates, so a rotated peer certificate does not end the relationship;
* dials remembered peers back on a backoff, and stops when they withdraw from mDNS.

Routing is *by* the peer's SPINE device address, so that address is bound to a connection
once and not afterwards: a peer that restates a different one, or claims one another
connection holds, is disconnected with `Disconnect::AddressConflict`. The innocent cause is
common enough to report rather than drop in silence — two devices shipped with the same
vendor and serial produce exactly this.

`Disconnected` is not the end of a use case. The state machine survives the socket, which
is the correct behaviour — a heat pump under a limit stays under it while the LAN
reconverges, and the failsafe timer, not the socket, decides when that stops being true.

## Pairing

A peer this node has not approved completes TLS — it has to, so that its SKI is proven
rather than claimed — and is then told `hello: pending` and held in the SHIP hello phase
(§13.4.4.1). The hub reports it:

```rust
HubEvent::TrustRequested { peer, origin } => {
    println!("{} wants to pair, {origin}", peer.ski.to_display_string());
    println!("its certificate is {}", peer.fingerprint);
    // a real device shows this on a display, or lights a button
}
```

`peer` is a `PendingPeer`: both identities SHIP knows a node by, and both **proved** rather
than claimed, because the peer completed TLS to get here. The SKI is what §12.2 trusts and
what is printed on the box; the fingerprint is what a QR code carries as `FPH256`, and what
the Pairing Service admits a control unit on.

`hub.approve(ski)` adds the SKI to the trust store and **completes the handshake it is
waiting in** — no reconnection, no timeout, no forty hex digits typed in advance. Adding the
SKI to the `TrustStore` directly does the same, because the store is what the waiting
handshake watches, so an approval from a user-interface thread needs nothing else.
`hub.refuse(ski)` tells the peer `hello: aborted` instead, and both sides report
`HandshakeFailed`. Neither happens on its own: SHIP IG §2.3 forbids an auto-accept mode,
and there is none.

The same flow runs the other way round. A peer this node dials, and which has not approved
this node, holds *us* pending while its user decides; `Connected` arrives when they do.

A peer `browse` finds but the store does not trust is reported as `Found { trusted: false }`
and kept aside; `approve` then dials it. That is the whole of commissioning from the
control box's side: browse, see, approve.

### And a PIN is not sent to just anybody

§12.5: the PIN "SHALL NOT be transmitted if the public key of the corresponding
communication partner has a user trust level that is less than '32'", and a peer admitted by
auto-accept alone is at 8 — [trust is a number](@/docs/security.md), not a yes.

```rust
HubEvent::PinWithheld { ski, level } => { /* only trusted at `level`, so it was not sent */ }
HubEvent::PinVerified { ski, level } => { /* it proved ours; §12.5 awarded a second factor */ }
```

The first event matters because from the far end a withheld PIN looks exactly like having
none: the connection carries on with data exchange restricted either way.

### Without a hub

A driver that owns its own `Engine` — because it has to be testable without a socket — has
no `Hub` to hear `TrustRequested` from, and needs the SKI just as much: a box that cannot
*display* it cannot take part in the exchange that most often goes wrong on site. `Node`
reports it directly, two ways:

```rust
// Told, on the handshake's own task. Must not block: send it on a channel.
let connection = node
    .accept_reporting(stream, Some(Box::new(move |peer| { let _ = tx.send(peer); })))
    .await?;

// Or asked, at any time, and filled in by a plain `accept` too.
for peer in node.pending_peers() {
    println!("approve {}? ({})", peer.ski, peer.fingerprint);
}
let mut changes = node.watch_pending();   // `changed().await` rather than polling
```

`connect_reporting` and `connect_over_reporting` are the dialling halves. An entry
disappears when its handshake ends — approval, refusal, timeout, dropped socket. Answer with
`node.trust_store().trust(ski)` or `node.refuse_pairing(ski)`.

### …or nobody is asked at all

The **SHIP Pairing Service** (TS 1.0.0) exists because a metering control unit is
installed by an electrician who never sees the household's screen. The unit — `devZ` — is
configured from the household device's QR code, and announces a `_shippairing._tcp`
record whose `digest` is an HMAC over its own fields under the printed secret. The
household device — `devA` — recomputes it and, if it matches, trusts the certificate.

**`devA` receives requests**, in two calls:

```rust
let receiver = Receiver::new(hub.ship_id().to_string(), hub.fingerprint(), secret)
    .with_guard(replay_guard_from_disk);   // §11: or a capture is honoured twice
hub.accept_pairing_requests(receiver);
hub.browse_pairing(&mdns)?;
```

An authentic request arrives as `HubEvent::Paired { unit, displaced }` — persist the trust
store and the replay guard. One addressed to this node that fails arrives as
`HubEvent::PairingRefused`, the mistyped-secret case §5.5 expects to be corrected; a
request for *another* node is not reported at all.

**`devZ` sends them**, from the other device's QR payload:

```rust
let qr: ShipQr = payload.parse()?;                  // ID, FPH256 and SPSEC come off it
let request = PairingRequest::new(
    qr.id.unwrap().as_str(), qr.certificate_fingerprint.unwrap(),
    node.ship_id(), node.fingerprint(), Nonce::from_bytes(random),
);
let mut requester = Requester::new(request.sign(&secret)?);

// after construction, and after every hub event
match requester.poll_action() {
    Some(RequesterAction::Announce) => mdns.announce_pairing(&instance, requester.announcement(), port, &addresses)?,
    Some(RequesterAction::Withdraw) => mdns.withdraw_pairing()?,
    None => {}
}
```

`Requester` is §4.2's timing, sans-IO. The request goes up **when it is configured**, not
when a connection succeeds — `devA` cannot trust `devZ` until it has heard it. A
connection decides when it comes *down*: `on_connected` starts the clock,
`on_disconnected` stops it, and fifteen uninterrupted minutes settle it for good, reboots
included. The SRV port is required by DNS-SD and meaningless: §5.3 says it SHALL be one
nothing listens on.

**What gets trusted is a certificate**, not a key identifier — the request names no SKI —
so the store holds a `PairedUnit` beside its `TrustedPeer`s, and a matching fingerprint
admits a peer exactly as an approved SKI would (§10.2). The SKI is recorded once a
connection proves one, so persist on `Connected` as well as on `Paired`. A node holds
**one** unit (§10.3): pairing a second untrusts the first and closes its connection.

**A working pairing is not replaceable.** §4.3 stops `devA` processing requests once it
has accepted one, and resumes only after fifteen minutes of being unable to reach the unit
it paired — so a broken unit can be replaced without a factory reset, and a captured
announcement cannot break a pairing that is doing its job. The hub tracks it, being the
only thing that knows whether messages are flowing.

## Discovery

With the `mdns` feature the hub browses for you:

```rust
hub.browse(&mdns)?;
```

Every `_ship._tcp` announcement arrives as `HubEvent::Found`, already acted on: a trusted
peer is remembered and being dialled, an untrusted one is waiting on an approval. A
withdrawal arrives as `HubEvent::Lost` and has already left the redial schedule. An
application that runs its own browse hands each sighting to `remember_discovered` and each
withdrawal to `forget_discovered` and gets the same behaviour.

A record whose SKI cannot be read is skipped rather than reported with the missing part
guessed at: the SKI is what a trust decision rests on.

## Reconnection

`remember` is what keeps a §14a installation working across a router reboot. Nothing tells
the hub the peer is back, so it keeps asking — backing off from one second to two minutes,
with the delay **jittered by the peer's SKI**, so a building coming back from a power cut
does not have every device dialling in the same instant. The dial runs in the background:
a peer that is down never holds up the ones that are up, and a dial that reaches nobody is
given up after `CONNECT_TIMEOUT` rather than the operating system's own minutes.

## `next` is not cancel-safe

The one rule for the event loop: **do not put `hub.next()` in a `tokio::select!` or a
`timeout`.** `Hub::run(handler)` is the loop that cannot get this wrong.

The hazard is the *sending* half. Reading is cancel-safe — `WebSocketStream` buffers a
partial frame and resumes — but `next` flushes the engine's queue before it reads, and
`Sink::send` carries no such guarantee. Dropped part-way through it leaves **half a frame on
the wire**, and the peer's parser is then out of step with the stream.

**The hub notices.** A write in flight is marked before the `await` and unmarked after, so a
future dropped in between leaves the mark set; the next call finds it and closes that
connection with `Disconnect::InterruptedWrite`. A reconnection costs a second. Carrying on
costs the session, and costs it silently — minutes later, as a subscription that was never
granted, a heartbeat that never arrived, and a limit refused for want of one.

Everything a caller would reach for `select!` to do has somewhere else to go. Sockets, the
listener and mDNS are the hub's own and arrive as events. Timers go to `wake_at`, which the
hub folds into its own deadlines and answers with `HubEvent::Tick` — and **a deadlock guard
is a timer**, which is the most natural reason to reach for `timeout` and the one to
resist. Anything else arriving from elsewhere — a command channel, a user's answer to a
pairing question — is drained *between* calls, on the tick:

```rust
loop {
    hub.wake_at(hub.now() + Duration::from_secs(1));
    match hub.next().await? {
        HubEvent::Tick => {
            while let Ok(command) = inbox.try_recv() { /* … */ }
        }
        /* … */
    }
}
```

`wake_at` takes an instant on the same clock as `hub.now()`, and an instant already in the
past is not an error: the hub runs its timers and comes straight back rather than reading
with a zero-length timeout, which would starve the connection it was supposed to be
watching.

## How many connections

A hub holds at most `DEFAULT_MAX_CONNECTIONS` — sixteen — and two to the same peer, which is
what §12.2.3 legitimately produces while a double connection is arbitrated. The cap counts
handshakes still running as well as connections held: a peer that dials in and sits in the
pending state has taken a slot, and a hundred of them would otherwise be a hundred TLS
sessions waiting for a user who is not there. `Hub::set_max_connections` raises it for a
gateway that serves more. SHIP caps none of this, and a device that malfunctions and dials
in a thousand times takes the memory of every node that answers.

Beyond the cap a socket is dropped before TLS and reported as
`HandshakeFailed { error: TooManyConnections }`; a connection that completed its handshake
in the meantime is closed with a `connectionClose`. Nothing already held is dropped to make
room: a cap decides what is *accepted*.

**A second cap covers the unapproved,** who hold a slot on nobody's authority.
`MAX_PENDING_TRUST` — four — is how many peers may be waiting for a decision, **one slot per
SKI**: a peer that dials four times is still one decision, so it keeps the slot it has and
the table stays open for somebody a user might want to approve. Beyond the cap a peer is told
`hello: aborted` at once and reported as `TooManyPendingPairings`, so it retries rather than
squatting. Keying that on the SKI is safe where keying it on an address would not be: the SKI
came out of a completed TLS handshake.

## Deciding is not answering

`hub.next()` sends what the engine has queued on the way *in*, so a loop that keeps calling
it never has to think about flushing. The two places that are not a loop do: before
`shutdown` — which flushes for you — and in a test or a one-shot that stops as soon as it
has seen what it was waiting for. `Hub::flush` is there for the second.

A Controllable System that accepts a limit and then stops driving its hub has left the
`result` message in the queue, and the Energy Guard is still waiting for it. Under §14a
that is the difference between a limitation that was honoured and one that cannot be shown
to have been.

## Owning your own engine

`Hub` owns an `Engine` and runs the opening exchange for you. An application that owns its
own — a different transport, a test harness, a gateway — needs the same two reads, and
`Engine::start_discovery` is them:

```rust
// A connection just opened. Ask what is on the other end of it.
let [discovery, use_cases] = engine.start_discovery(now);
```

Both are addressed *without* a device part, which the SPINE implementation guide §2.7
permits for exactly this message and no other: the peer's device address is what the answer
contains, so it cannot be in the question. `Engine::discover` is the same pair addressed to
a peer whose address is already known.

`Node` is usable without the hub too: `Node::accept` and `Node::connect` run one handshake
to completion and hand back a `ShipConnection`, and a trust decision added to the store
meanwhile lets a pending one through, exactly as it does under the hub.

## Trying it

```sh
cargo run --example networked --features runtime,ring
```

Two nodes generate certificates, approve each other's SKI, complete TLS 1.2 with mutual
authentication and the SHIP handshake, and then discover, bind and exchange a limit over
loopback.

## The two simulators

`networked` is one program playing both sides. The two simulators are the sides as separate
programs, which is what testing against real hardware needs — a control box with no
household to limit, or a household appliance with no control box.

```sh
# In one terminal: the household end. Prints its SKI and its QR payload, and asks on the
# terminal when a control box it does not know connects.
cargo run --example heat_pump --features full

# In another: the grid end. Finds the household over mDNS, offers to pair with it, and
# holds it to 4.2 kW once paired.
cargo run --example steuerbox --features full -- --limit 4200
```

Answer `y` on each side, which is what pressing the button on a real device amounts to —
or pass `--trust <SKI>` to either and skip the question. They persist their identity and
their trust store between runs, and print the `lpc:` runtime signals of
[Certification](@/docs/certification.md) as the state moves. `--reset` is the EEBUS reset
of SHIP §12.2.2: forget every peer, and the identity with it.

Between them they cover the parts a single-process example cannot show — a person deciding
whether to trust a device while it waits on the wire, a device that comes back after a
restart still paired, and a control box that finds a household appliance it has never been
told the address of.
