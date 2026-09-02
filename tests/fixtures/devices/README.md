# Captures from devices this crate has never met

Fifteen datagrams from eight devices by seven manufacturers — Elli, evcc, Kostal, Porsche,
SMA, Spelsberg, Vaillant and Viessmann — answering the two questions a SHIP connection
opens with: *what are you?* (`nodeManagementDetailedDiscoveryData`) and *what do you do?*
(`nodeManagementUseCaseData`).

They matter because everything else in `tests/fixtures/` was produced either by this crate
or by the specification. A round trip over those proves the encoder agrees with the
decoder, which is not the question anyone is asking. These were produced by somebody
else's implementation talking to somebody else's hardware.

## Provenance

Recorded with [`eebus-go`](https://github.com/enbility/eebus-go) and published as
[`enbility/devices`](https://github.com/enbility/devices), MIT-licensed. The upstream
files are ordinary JSON — the way a Go struct serialises — and are converted here into the
EEBUS JSON-UTF8 projection of SHIP §11.4 by:

```sh
git clone --depth=1 https://github.com/enbility/devices /tmp/devices
cargo run -p xtask -- devices /tmp/devices
```

Re-running it when the corpus has not changed produces no diff. The device serial numbers
were already redacted upstream.

## What they are asserted against

`tests/real_devices.rs`. Every capture must parse, resolve into a device model, and be
stable under re-encoding — see that file for why *stable* rather than byte-for-byte, and
for the one capture that is not schema-valid.
