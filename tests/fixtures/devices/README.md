# Device discovery captures

These 15 datagrams contain detailed discovery and use-case replies from eight devices.
The source corpus was recorded with eebus-go and published under the MIT license with
device serial numbers redacted. [Upstream captures](https://github.com/enbility/devices)

To regenerate the EEBUS JSON-UTF8 fixtures from the upstream JSON files, run:

```sh
git clone --depth=1 https://github.com/enbility/devices /tmp/devices
cargo xtask devices /tmp/devices
```

`cargo test --test real_devices` verifies parsing, discovery and stable re-encoding.
