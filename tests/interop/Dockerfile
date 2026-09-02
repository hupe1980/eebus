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
EXPOSE 4712
