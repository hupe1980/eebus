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
        HubEvent::Connected { .. } => {}
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

## Trying it

```sh
cargo run --example networked --features runtime
```

Two nodes generate certificates, approve each other's SKI, complete TLS 1.2 with mutual
authentication and the SHIP handshake, and then discover, bind and exchange a limit over
loopback.
