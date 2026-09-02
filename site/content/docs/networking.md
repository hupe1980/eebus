+++
title = "On a network"
description = "The runtime Hub: TCP, TLS and WebSocket sockets, mDNS discovery, routing datagrams to peers, keep-alive, and reconnection with SKI-spread backoff."
weight = 110
[extra]
group = "Deployment"
+++

The protocol core owns no socket and reads no clock, which is what makes every timer in the
standard an ordinary unit test. The `runtime` feature is the part that does own them, and
it is the only part that does.

## The Hub

A `Hub` holds one SPINE engine and every connection to it.

```rust
let mut hub = Hub::new(node, engine);
hub.connect("192.0.2.10:4712").await?;            // TCP, TLS, WebSocket, SHIP

loop {
    match hub.next().await? {
        HubEvent::PeerDiscovered { device, .. } => { /* it has said what it is */ }
        HubEvent::Spine(event) => { actor.handle_event(hub.engine_mut(), &event, hub.now()); }
        HubEvent::Tick => { actor.handle_timeout(hub.engine_mut(), hub.now()); }
        HubEvent::Disconnected { ski, .. } => { /* the session is gone, the use case is not */ }
        HubEvent::Connected { version, .. } => { /* 1.1, or 1.0 with an older peer */ }
    }
    hub.wake_at(actor.poll_timeout());
}
```

What it does for you:

* routes each datagram the engine produces to the peer it addresses;
* runs the opening discovery, so an application hears about a peer only once it knows what
  that peer *is*;
* keeps idle connections alive with the pings SHIP §10.4 asks for;
* resolves the double connections of two nodes that dial each other at once (SHIP §12.2.3);
* follows certificate updates, so a rotated peer certificate does not end the relationship.

Routing is *by* the peer's SPINE device address, so that address is bound to a connection
once and not afterwards: a peer that restates a different one, or claims one another
connection holds, is disconnected with `Disconnect::AddressConflict`. The innocent cause is
common enough to report rather than drop in silence — two devices shipped with the same
vendor and serial produce exactly this.

`Disconnected` is not the end of a use case. The state machine survives the socket, which
is the correct behaviour — a heat pump under a limit stays under it while the LAN
reconverges, and the failsafe timer, not the socket, decides when that stops being true.

## Discovery

With the `mdns` feature, finding the address is the network's job rather than the
installer's, and staying connected to it is the hub's:

```rust
let browse = mdns.browse()?;
while let Some(event) = browse.recv() {
    match event {
        BrowseEvent::Found(found) => { hub.remember_discovered(&found); }   // trusted peers only
        BrowseEvent::Lost { instance } => hub.forget_discovered(&instance),
    }
}
```

A record whose SKI cannot be read is skipped rather than reported with the missing part
guessed at: the SKI is what a trust decision rests on. A removal names only the instance,
since the TXT record that carried the SKI is what has been withdrawn.

## Reconnection

`remember` is what keeps a §14a installation working across a router reboot. Nothing tells
the hub the peer is back, so it keeps asking — backing off from one second to two minutes,
with the delay **jittered by the peer's SKI**, so a building coming back from a power cut
does not have every device dialling in the same instant.

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

Everything a caller would reach for `select!` to do has somewhere else to go. Timers go to
`wake_at`, which the hub folds into its own deadlines and answers with `HubEvent::Tick` —
and **a deadlock guard is a timer**, which is the most natural reason to reach for `timeout`
and the one to resist. Work arriving from elsewhere — a listener accepting connections, an
mDNS browse, a command channel — is drained *between* calls, on the tick:

```rust
loop {
    // Between calls: whatever else has turned up.
    while let Ok(stream) = inbox.try_recv() {
        hub.accept(stream).await?;
    }
    // The deadline, and the tick that carries it.
    hub.wake_at(hub.now() + Duration::from_secs(1));

    // And then the one await that owns the sockets, uninterrupted.
    match hub.next().await? { /* … */ }
}
```

`wake_at` takes an instant on the same clock as `hub.now()`, and an instant already in the
past is not an error: the hub runs its timers and comes straight back rather than reading
with a zero-length timeout, which would starve the connection it was supposed to be
watching.

## How many connections

A hub holds at most `DEFAULT_MAX_CONNECTIONS` — sixteen — and two to the same peer, which is
what §12.2.3 legitimately produces while a double connection is arbitrated.
`Hub::set_max_connections` raises it for a gateway that serves more. SHIP caps neither, and
a device that malfunctions and dials in a thousand times takes the memory of every node that
answers.

Beyond the cap, `accept` and `connect` close with a `connectionClose` and report
`ConnectionError::TooManyConnections`; `adopt` hands the connection back. Nothing already
held is dropped to make room: a cap decides what is *accepted*.

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
# In one terminal: the household end. Prints its SKI and its QR payload.
cargo run --example heat_pump --features full

# In another: the grid end. Trust each other, then hold the household to 4.2 kW.
cargo run --example steuerbox --features full -- --trust <the pump's SKI> --limit 4200
cargo run --example heat_pump --features full -- --trust <the box's SKI>
```

They announce themselves over mDNS and find each other, persist their identity and their
trust store between runs, and print the `lpc:` runtime signals of
[Certification](@/docs/certification.md) as the state moves. `--reset` is the EEBUS reset of
SHIP §12.2.2: forget every peer, and the identity with it.

Between them they cover the parts a single-process example cannot show — an installer
reading a SKI off one screen and typing it into another, a device that comes back after a
restart still paired, and a control box that finds a household appliance it has never been
told the address of.
