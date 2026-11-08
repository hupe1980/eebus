+++
title = "On a network"
description = "The runtime Hub: listening and dialling in the background, interactive pairing, mDNS discovery, routing datagrams to peers, keep-alive, and reconnection with SKI-spread backoff."
weight = 110
[extra]
group = "Deployment"
+++

The protocol core owns no socket and reads no clock; the `runtime` feature is the only part
that does.

## The Hub

A `Hub` holds one SPINE engine and every connection to it. It listens, dials and browses in
the background, asks the application only when a trust decision is needed, and hands
everything else over as events.

```rust
let mut hub = Hub::new(node, engine);
hub.listen("0.0.0.0:4712").await?;          // every node binds a port (§8.1)…
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
  or waiting for a user holds nothing else up;
* routes each datagram the engine produces to the peer it addresses;
* runs the opening discovery, so an application hears about a peer only once it knows what
  that peer *is*;
* keeps idle connections alive with the pings SHIP §10.4 asks for;
* resolves the double connections of two nodes that dial each other at once (SHIP §12.2.3);
* follows certificate updates, so a rotated peer certificate does not end the relationship;
* dials remembered peers back on a backoff, and stops when they withdraw from mDNS.

Routing is *by* the peer's SPINE device address, so that address is bound to a connection
once and not afterwards: a peer that restates a different one, or claims one another
connection holds, is disconnected with `Disconnect::AddressConflict` — as are two devices
shipped with the same vendor and serial.

`Disconnected` is not the end of a use case. The state machine survives the socket: a heat
pump under a limit stays under it while the LAN reconverges, and the failsafe timer decides
when that ends.

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

`peer` is a `PendingPeer`: both identities SHIP knows a node by, both proved by TLS. The
SKI is what §12.2 trusts and what is printed on the box; the fingerprint is what a QR code
carries as `FPH256`, and what the Pairing Service admits a control unit on.

`hub.approve(ski)` adds the SKI to the trust store and **completes the waiting handshake** —
no reconnection. Adding the SKI to the `TrustStore` directly does the same, since the
waiting handshake watches the store, so a user-interface thread can approve.
`hub.refuse(ski)` tells the peer `hello: aborted` instead, and both sides report
`HandshakeFailed`. There is no auto-accept mode (SHIP IG §2.3).

The same flow runs the other way round. A peer this node dials, and which has not approved
this node, holds *us* pending while its user decides; `Connected` arrives when they do.

A peer `browse` finds but the store does not trust is reported as `Found { trusted: false }`
and kept aside; `approve` then dials it. Commissioning from the control box's side is
browse, see, approve.

### And a PIN is not sent to just anybody

§12.5: the PIN "SHALL NOT be transmitted if the public key of the corresponding
communication partner has a user trust level that is less than '32'", and a peer admitted by
auto-accept alone is at 8 — [trust is a number](@/docs/security.md), not a yes.

```rust
HubEvent::PinWithheld { ski, level } => { /* only trusted at `level`, so it was not sent */ }
HubEvent::PinVerified { ski, level } => { /* it proved ours; §12.5 awarded a second factor */ }
```

`PinWithheld` matters because from the far end a withheld PIN looks exactly like having
none: the connection carries on with data exchange restricted either way.

### Without a hub

A driver that owns its own `Engine` has no `Hub` to hear `TrustRequested` from, and still
needs to display the SKI. `Node` reports it directly, two ways:

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

The **SHIP Pairing Service** (TS 1.0.0) serves a metering control unit installed by an
electrician who never sees the household's screen. The unit — `devZ` — is configured from
the household device's QR code, and announces a `_shippairing._tcp` record whose `digest`
is an HMAC over its own fields under the printed secret. The household device — `devA` —
recomputes it and, if it matches, trusts the certificate.

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
when a connection succeeds — `devA` cannot trust `devZ` until it has heard it. A connection
decides when it comes *down*: `on_connected` starts the clock, `on_disconnected` stops it,
and fifteen uninterrupted minutes settle it for good, reboots included. The SRV port is
required by DNS-SD but unused: §5.3 says it SHALL be one nothing listens on.

**What gets trusted is a certificate**, not a key identifier — the request names no SKI —
so the store holds a `PairedUnit` beside its `TrustedPeer`s, and a matching fingerprint
admits a peer exactly as an approved SKI would (§10.2). The SKI is recorded once a
connection proves one, so persist on `Connected` as well as on `Paired`. A node holds
**one** unit (§10.3): pairing a second untrusts the first and closes its connection.

**A working pairing is not replaceable.** §4.3 stops `devA` processing requests once it
has accepted one, and resumes only after fifteen minutes of being unable to reach the unit
it paired — so a broken unit can be replaced without a factory reset, and a captured
announcement cannot break a working pairing. The hub tracks this.

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

### A node is not its own peer

A node that announces `_ship._tcp` and browses for it meets its own announcement. That is
not reported, not remembered and not offered for pairing.

TLS cannot catch it — a node dialling itself presents exactly the certificate the verifier
demands — so a connection proving this node's own SKI is refused with
`ConnectionError::SelfConnection`, whichever end opened it and however it was reached —
`remember`, a configured address, or `adopt` from an application's own transport.
`Hub::approve` refuses the same SKI.

One layer down, a peer claiming this node's own SPINE device address is
`Disconnect::AddressConflict`, and the engine discards such a datagram rather than filing
this node as its own peer.

## Reconnection

`remember` keeps a peer reachable across a router reboot: the hub keeps dialling, backing
off from one second to two minutes, with the delay **jittered by the peer's SKI** so a
building coming back from a power cut does not dial all at once. Dials run in the
background, and one that reaches nobody is given up after `CONNECT_TIMEOUT`.

## `next` is not cancel-safe

**Do not put `hub.next()` in a `tokio::select!` or a `timeout`.** `Hub::run(handler)` is
the loop that cannot get this wrong.

Reading is cancel-safe, but `next` flushes the engine's queue first, and a `Sink::send`
dropped part-way leaves **half a frame on the wire**. The next call then closes that
connection with `Disconnect::InterruptedWrite`.

Sockets, the listener and mDNS arrive as events. Timers — a deadlock guard included — go to
`wake_at`; drain other channels, such as a command inbox, on `HubEvent::Tick`:

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

`wake_at` takes an instant on the clock of `hub.now()`; one already in the past runs the
timers and returns at once rather than reading with a zero-length timeout.

## How many connections

A hub holds at most `DEFAULT_MAX_CONNECTIONS` — sixteen — and two to the same peer, which
§12.2.3 produces while a double connection is arbitrated. The cap counts handshakes still
running, including peers waiting in the pending state. `Hub::set_max_connections` raises it
for a gateway, or lowers it to no less than one, the floor §623 sets.
[Security](@/docs/security.md#resource-exhaustion) lists every bound.

**A slot is reserved in each direction** (SHIP §8.1). §628 asks a node holding more than one
connection to *"always reserve one connection for the TCP server port"* — so of `x`
connections at most `x-1` may be ones this node dialled — and §632 says the same in reverse
for ones it accepted, so a gateway that has dialled every known peer still accepts a new
Steuerbox. Past either limit the outcome is `TooManyConnections`.

**Every SHIP node listens.** §623 puts the floor at one active connection, §624 and §628
both require a listening TCP server, and §638 asks for it to be open whenever the node is
under its limit — regardless of which end a use case says opens the conversation. A node
that announces `_ship._tcp` at a port it does not serve is retried by every peer for ever.

Beyond the cap a socket is dropped before TLS and reported as
`HandshakeFailed { error: TooManyConnections }`; a connection that completed its handshake
in the meantime is closed with a `connectionClose`. Nothing already held is dropped to make
room: a cap decides what is *accepted*.

**A second cap covers the unapproved.** `MAX_PENDING_TRUST` — four — is how many peers may
be waiting for a decision, **one slot per SKI**: a peer that dials four times is still one
decision. Beyond the cap a peer is told `hello: aborted` at once and reported as
`TooManyPendingPairings`, so it retries rather than squatting. The SKI key is safe because
it comes from a completed TLS handshake.

## Closing a connection, and blocking a peer

SPINE IG §2.6.2 escalates a peer that stops answering, and its fourth step is *close the
SHIP connection*. The engine stops before it — §2.6.4: one unresponsive use
case is not a reason to drop a connection carrying others — and reports
`SpineEvent::RequestTimedOut`; closing is the application's decision.

```rust
hub.close(&ski, ConnectionCloseReason::Unspecific);       // §2.6.2: end this session
```

**A closed peer comes back.** A remembered peer is redialled on the usual backoff.
§2.6.3's *block the peer* is two calls:

```rust
hub.forget_peer(&ski);                                     // stop dialling it back
hub.close(&ski, ConnectionCloseReason::RemovedConnection); // and end what is up
```

Forget first: a peer closed while remembered is dialled again.

Neither touches trust; a closed peer that dials back in is still approved. Removing trust is
`TrustStore::forget`.

## Deciding is not answering

`hub.next()` sends what the engine has queued on the way *in*, so a loop never has to
flush. `shutdown` flushes for you; a test or one-shot that stops as soon as it has seen what
it was waiting for calls `Hub::flush`. Otherwise a `result` — such as the acknowledgement of
an accepted limit — stays in the queue and the peer never receives it.

## Owning your own engine

`Hub` owns an `Engine` and runs the opening exchange for you. An application that owns its
own — a different transport, a test harness, a gateway — needs the same two reads, and
`Engine::start_discovery` is them:

```rust
// A connection just opened. Ask what is on the other end of it.
let [discovery, use_cases] = engine.start_discovery(now);
```

Both are addressed *without* a device part, which SPINE IG §2.7 permits for this message
only, since the answer is what contains the peer's address. `Engine::discover` is the same
pair addressed to a known peer.

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
programs, for testing against real hardware.

```sh
# In one terminal: the household end. Prints its SKI and its QR payload, and asks on the
# terminal when a control box it does not know connects.
cargo run --example heat_pump --features full

# In another: the grid end. Finds the household over mDNS, offers to pair with it, and
# holds it to 4.2 kW once paired.
cargo run --example steuerbox --features full -- --limit 4200
```

Answer `y` on each side — the button press on a real device — or pass `--trust <SKI>` to
either and skip the question. They persist their identity and
their trust store between runs, and print the `lpc:` runtime signals of
[Certification](@/docs/certification.md) as the state moves. `--reset` is the EEBUS reset
of SHIP §12.2.2: forget every peer, and the identity with it.

Where there is no multicast — a container, a hardened host, a CI runner — the control box
takes the address instead:

```sh
cargo run --example steuerbox --features full -- \
    --dial 127.0.0.1:4712 --trust <the appliance's SKI> --limit 4200
```

With `--trust` the address is *remembered*, so it is redialled across a reboot at the other
end; without, it is a single dial and the appliance decides whether to trust this box.

`cargo xtask simulators` is that arrangement as a gate, run on every push: both simulators
against each other, asserting the §14a exchange completes and that the control box serves the
port it announces.
