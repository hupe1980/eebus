# The other implementation, as its own authors ship it.
#
# `eebus-go` carries complete examples of both sides of LPC, and this image builds both:
#
#   controlbox  — an Energy Guard. It discovers, binds, subscribes and writes a limit.
#   evse        — a Controllable System. It publishes its limit and approves writes.
#
# Nothing here is written by this project, which is the entire point: a harness we wrote
# would encode our own assumptions and prove nothing. The revision is pinned — an interop
# test whose peer silently changes version reports "we broke" when the truth is "they
# changed", and telling those apart is the whole value.
ARG EEBUS_GO_REV=0aa83d264add807f8bd5f54aabd7ba82fd9e8c04

FROM golang:1.24-alpine AS build
ARG EEBUS_GO_REV
RUN apk add --no-cache git
WORKDIR /src
RUN git clone https://github.com/enbility/eebus-go . \
 && git checkout --quiet "${EEBUS_GO_REV}"

# The examples ship with their trace and debug logging commented out. Turning it back on
# changes no protocol behaviour and is the difference between an interop failure you can
# read and one you can only stare at — the peer's own view of the conversation goes into
# the panic message.
RUN for f in examples/controlbox/main.go examples/evse/main.go; do \
      sed -i 's|^\t// h.print(|\th.print(|; s|^\t// h.printFormat(|\th.printFormat(|' "$f"; \
    done

# The control box never calls `StartHeartbeat`, so its Energy Guard writes limits without
# one. LPC IG §2.11 gates every limit on a heartbeat inside the last 60 s, so a conformant
# Controllable System must refuse those — which this crate did, with `errorNumber` 7, until
# this line existed. A finding about the example rather than about either library.
RUN sed -i 's|\(\terr = h.myService.AddUseCase(h.uclpc)\)|\1\n\th.uclpc.StartHeartbeat()|' \
        examples/controlbox/main.go \
 && grep -q "StartHeartbeat" examples/controlbox/main.go

# And its heartbeat interval, from 60 s to 4 s. This is a *configuration* value the example
# passes to `NewConfiguration`, not protocol behaviour — but without it the accept path is
# unreachable, and the reason is itself the finding: the example writes its limit five
# seconds after discovery, while its heartbeat ticker runs at the configured timeout less
# two seconds. At the default that is 58 s, so the first heartbeat a subscriber can ever see
# arrives long after the write.
RUN sed -i 's|time.Second\*60|time.Second*4|' examples/controlbox/main.go \
 && grep -q "time.Second\*4" examples/controlbox/main.go

RUN go build -o /out/controlbox ./examples/controlbox \
 && go build -o /out/evse ./examples/evse

FROM alpine:3.20
COPY --from=build /out/controlbox /out/evse /usr/local/bin/
# No ENTRYPOINT: the test names which side it wants.
#
# ---------------------------------------------------------------------------------------
# Pointing the peer the other way: their `controlbox` dialling a Controllable System that
# listens *outside* the container — a running `hemsd`, say, or this crate's `Hub::listen`.
#
# That is the direction §14a actually describes: the control box dials, the household
# appliance listens. It needs one thing Docker does not give by default, and it is not the
# one people reach for first.
#
#   1. **The peer discovers; it does not dial an address.** `eebus-go`'s examples take
#      `-remoteski <SKI>` and then look for that SKI over mDNS. There is no `-remoteip`.
#      So `host.docker.internal` — which resolves to the host from inside a bridge network,
#      and which people add with `--add-host=host.docker.internal:host-gateway` on Linux —
#      does not help: there is nowhere to put it.
#
#   2. **So multicast has to reach the container**, which means the container has to be on
#      the host's own network stack:
#
#          docker run --rm --network host eebus-interop-controlbox:pinned \
#              controlbox -port 4713 -remoteski <the listener's SKI>
#
#      `--network host` is honoured by a **native Linux daemon**. On Docker Desktop
#      (macOS, Windows) the container runs inside a VM: the flag is accepted, the container
#      joins the *VM's* network, and the host's mDNS never arrives. `tests/interop.rs`
#      checks the daemon with `docker info --format {{.OSType}}/{{.OperatingSystem}}` and
#      skips with that reason rather than reporting a pass.
#
#   3. **The listener has to announce itself, with an address.** `Hub::listen` opens a
#      socket and says nothing. Announce `_ship._tcp` with `eebus::mdns::Mdns::announce`,
#      carrying the listener's SKI in the TXT record — and name the addresses it accepts
#      connections on. DNS-SD resolves an instance to a host name and the host name to an
#      address, so an announcement with none is one the peer finds and cannot dial.
#
# The two SKIs still have to be exchanged first, in both directions: pass the listener's to
# the container with `-remoteski`, and trust the container's — which it prints as
# `Local SKI:` on start-up, since it generates a fresh certificate every run — in the
# listener's trust store.
#
# `-port` above is the port the peer's *own* listener binds, not the one it dials. On the
# host network it must not collide with the listener's, which is why the test gives it
# 4713 and keeps 4712 for this crate.
# ---------------------------------------------------------------------------------------
EXPOSE 4712
