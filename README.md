# 10over

[![CI](https://github.com/divotmaker/10over/actions/workflows/ci.yml/badge.svg)](https://github.com/divotmaker/10over/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/tenover.svg)](https://crates.io/crates/tenover)
[![docs.rs](https://docs.rs/tenover/badge.svg)](https://docs.rs/tenover)

## Disclaimer

This project is not affiliated with or endorsed by Garmin Ltd.
Garmin, Approach, and R10 are trademarks of their respective owner.

## Description

Client library for the Garmin R10 launch monitor. Decodes the R10's
BLE/GFDI/protobuf protocol and exposes shot data through a synchronous,
poll-based API:

- **`Client<T>`** — poll-based client over any `Transport`. Handles MultiLink
  registration, GFDI handshake, protobuf subscribe/wakeup, shot deduplication,
  and state tracking automatically.
- **`BleTransport`** — platform-agnostic BLE transport (`ble` feature, enabled
  by default). Auto-selects BlueZ on Linux or btleplug on Windows/macOS.
- **`driver::R10`** — a [flightrelay](https://github.com/flightrelay/sdk-rust)
  `DeviceDriver` for the R10 (`driver` feature): connect, handshake, arm, and
  shots as [Flight Relay Protocol](https://github.com/flightrelay/spec) events.
  The `tenover-frp` binary (`frp` feature) serves it to FRP controllers.

## Legal Basis — DMCA Section 1201(f)

This project is an exercise of the interoperability exception under
[17 U.S.C. § 1201(f)](https://www.law.cornell.edu/uscode/text/17/1201):

> (f) Reverse Engineering.—
>
> (1) Notwithstanding the provisions of subsection (a)(1)(A), a person who has
> lawfully obtained the right to use a copy of a computer program may
> circumvent a technological measure that effectively controls access to a
> particular portion of that program for the sole purpose of identifying and
> analyzing those elements of the program that are necessary to achieve
> interoperability of an independently created computer program with other
> programs, and that have not previously been readily made available to the
> person engaging in the circumvention, to the extent any such acts of
> identification and analysis do not constitute infringement under this title.
>
> (2) Notwithstanding the provisions of subsections (a)(2) and (b), a person
> may develop and employ technological means to circumvent a technological
> measure, or to circumvent protection afforded by a technological measure, in
> order to enable the identification and analysis described in paragraph (1),
> or for the purpose of enabling interoperability of an independently created
> computer program with other programs, if such means are necessary to achieve
> such interoperability, to the extent that doing so does not constitute
> infringement under this title.

The Garmin R10 uses a proprietary protocol over Bluetooth Low Energy to
communicate shot data (ball speed, launch angle, spin, club data, etc.) to
companion software. Garmin does not publish this protocol or provide an SDK for
third-party integration. The protocol was worked out from the researcher's own
lawfully purchased device and from analysis of the official Garmin Golf app,
solely to enable interoperability with third-party golf simulation software.

No Garmin code is reproduced here; the protocol is described and implemented in
our own words, and the protobuf schema uses our own message and field names. No
access controls were circumvented — the device uses standard BLE bonding, and
all protocol data was captured from the researcher's own device.

## Acceptable Use

This project exists solely to enable interoperability between the Garmin R10
and third-party golf simulation software.

**It must not be used to:**

- Circumvent licensing or subscription requirements on Garmin products
- Unlock paid features without purchase
- Bypass any access controls on Garmin software or services

Issues or discussions proposing circumvention of licensing will be closed and
the user blocked.

## License

Licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)

at your option.

## Status

**Alpha**. The protocol is reverse-engineered and still being mapped — the API
will change as new message types are decoded and existing ones are refined.
This crate follows [SemVer](https://semver.org/): until `1.0`, breaking changes
bump the minor version.

## Migrating from 0.4

0.5 moves to flightrelay 0.5 (FRP 0.2.0). A 0.5 device and a 0.4 controller
cannot handshake, so controllers move to FRP 0.2.0 too.

| | 0.4 | 0.5 |
|---|---|---|
| FRP device API | `FrpDevice`, `ControllerCommand`, the `frp` module | `driver::R10` (a flightrelay `DeviceDriver`) |
| Shot conversions | `frp::{ball_flight, club_data}` | `driver::convert::{launch, club_data}`; shots carry `launch` |
| `tenover-frp` target | `tenover-frp <target>` | `tenover-frp --target <target>`, named flags only (see [FRP device](#frp-device-tenover-frp)); several controllers at once |
| `tenover-frp` model | `Approach R10` | `R10` |
| Shot with a total spin of 0 | neither total spin nor axis | total spin 0, no axis |
| `ready` | true at connect; false before a shot and on every device state change | true from the moment the device arms until a shot, false after it |
| `device_telemetry` | `ready`, identity | adds `source`, `device_state` and `tilt`/`roll` |
| R10 drops | `tenover-frp` exits | reconnects with a backoff |
| Handshake | waits as long as it takes | at most 10 s, then a retry |
| Ctrl-C | ends the process wherever it is | stops the scan, the connect or the session, then exits |
| BLE dependencies | always compiled | only with `ble` (default); `--no-default-features` pulls none |
| Connect API | `BleTransport::auto_connect` | adds `auto_connect_with(cancel, log)` (also on `BlueZTransport` and `BtleplugTransport`); `BlueZError` and `BtleplugError` gain `Canceled` |
| btleplug scan output | printed to stderr | progress lines go to the `log` callback |
| btleplug close | the peripheral stays connected until it next sends | dropping the transport (or `BleTransport::disconnect`) disconnects it, waiting at most 4 s |
| BlueZ `auto_connect` | D-Bus calls without a timeout | each D-Bus call times out after 3 s; a `Connect` that times out is waited for, up to 30 s; a connect it started is undone if setup fails |

## Migrating from 0.2

0.3.0 renames public items so the API says what the R10 does. Behavior and
wire bytes are unchanged; only names moved.

| 0.2 | 0.3 |
|-----|-----|
| `proto::SmartEvent` | `proto::EnvelopeEvent` |
| `proto::decode_smart` | `proto::decode_envelope` |
| `proto::ShotConfig` | `proto::Environment` |
| `proto::build_shot_config_request` | `proto::build_environment_request` |
| `Client::send_shot_config` | `Client::send_environment` |
| `SmartEvent::LaunchMonitorResponse` | `EnvelopeEvent::MonitorReply` |
| `DeviceState::InterferenceTest` | `DeviceState::NoiseCheck` |
| `PlatformTilted` (error-code variant) | `NotLevel` |
| `SpinCalcType::Ratio` | `SpinCalcType::Estimated` |
| `SpinCalcType::BallFlight` | `SpinCalcType::FlightModel` |

Also renamed:

- The reply variants of `EnvelopeEvent` for the subscribe, wake, environment
  and leveling-check exchanges are now `SubscribeResult`, `WakeResult`,
  `EnvironmentResult` and `LevelCheck { verdict, attempt }` (the last replaces
  the calibration-report variant).
- The wake and environment reply variants of `client::Event` are now
  `Event::WakeResult` and `Event::EnvironmentResult`.
- The shot-type enum with the variants `Practice` and `Normal` is now
  `ShotKind` with `Warmup` and `Regular`, and `ShotData` carries it in the field
  `kind` (it was the shot-type field).
- The tee-distance field of `Environment` is now `tee_distance_yd`.

The `proto::smart`, `proto::event_sharing` and `proto::launch_monitor` modules
(the generated protobuf types) are no longer public. Use the types above
together with `ShotData`, `DeviceState`, `DeviceError`, `build_subscribe_request`
and `build_wakeup_request`.

## Protocol Documentation

Detailed specs live in [`docs/`](docs/):

- **[WIRE.md](docs/WIRE.md)** — Transport stack (BLE GATT, MultiLink, COBS,
  CRC-16, GFDI framing), protobuf message catalog, shot data fields and units.
- **[SEQUENCE.md](docs/SEQUENCE.md)** — Connection lifecycle, MultiLink
  registration, GFDI handshake, protobuf session setup, shot data flow.

## Quick Start

The R10 must be paired at the OS level before connecting. This is a one-time
operation:

- **Linux**: `bluetoothctl pair <address>` then `bluetoothctl trust <address>`
- **Windows**: Settings > Bluetooth & devices > Add device > Bluetooth
- **macOS**: System Preferences > Bluetooth

The library has no pairing API — it connects to an already-paired device.

```rust
use tenover::ble::BleTransport;
use tenover::{Client, Event};

let transport = BleTransport::auto_connect()?;
let mut client = Client::new(transport, 20);
client.start()?;

loop {
    match client.poll()? {
        Some(Event::Ready) => println!("Waiting for shot..."),
        Some(Event::Shot(shot)) => {
            if let Some(b) = &shot.ball {
                println!("Ball: {:.1} m/s", b.ball_speed);
            }
            if let Some(c) = &shot.club {
                println!("Club: {:.1} m/s", c.club_head_speed);
            }
        }
        _ => {}
    }
}
```

See [`examples/r10.rs`](examples/r10.rs) for a complete standalone example.

```sh
# Linux (auto-selects BlueZ)
cargo run --example r10

# Windows cross-compile
cargo build --example r10 --target x86_64-pc-windows-gnu --release
```

### FRP device (`tenover-frp`)

The `driver` feature adds `driver::R10`, a flightrelay `DeviceDriver` that
makes the R10 a standard [Flight Relay Protocol](https://github.com/flightrelay/spec)
(FRP 0.2.0) device. Any FRP controller (flighthook, dashboards, etc.) receives
shot data without knowing anything about the R10 protocol. The `frp` feature
builds `tenover-frp`, which serves the driver over WebSocket:

```text
tenover-frp [--config <path>] [--target <listen-addr | ws://host:port/frp>]
            [--set <key>=<value>]... [--log <info|debug|trace>] [--help]
```

| Flag | Meaning |
|------|---------|
| `--target` | An address to listen on (default `0.0.0.0:5880`; several controllers may connect), or a `ws://` URL to dial a controller such as flighthook |
| `--config` | A TOML file with the config keys at the top level |
| `--set key=value` | Set one config key, after the file; repeatable |
| `--log` | The lowest level of log lines printed to stderr (default `info`) |
| `--help` | Usage and every config key |

Config keys:

| Key | Default | Meaning |
|-----|---------|---------|
| `range` | unset | Distance from the R10 to the tee, e.g. `"7ft"` or `"2.1m"`, sent to the device once it wakes. Unset leaves the device's own setting alone |

```sh
# Find a paired R10, serve FRP on port 5880
cargo run --features frp --bin tenover-frp

# Another port, with a tee distance
cargo run --features frp --bin tenover-frp -- --target 0.0.0.0:9000 --set range=7ft

# Dial flighthook
cargo run --features frp --bin tenover-frp -- --target ws://flighthook:5880/frp
```

The binary reconnects whenever the R10 drops (it sleeps, or goes out of
range), with a backoff that grows by 1 s per failed attempt up to 15 s and
starts over after a session of 10 s or more. Ctrl-C stops it cleanly, even
during a BLE scan.

What the device sends over FRP:

- `device_telemetry` with `source: "measured"`, manufacturer `Garmin` and
  model `R10` before the first shot; `ready`, `device_state` (`waiting`,
  `recording`, `processing`), and `tilt`/`roll` (`ok`, or the angles the
  device reports with a leveling error);
- per shot: `shot_trigger`, `launch` (launch speed, both launch angles, and
  total spin, spin axis, backspin and sidespin, all measured; a total spin of
  0 is sent as 0 with no axis), `club_path` (club speed, face, path, attack
  angle) and `shot_finished`. The R10 reports no flight result;
- device errors as alerts;
- `raw_frame`s, only to a controller that asks: one per GFDI message, labeled
  `notify <type>` or `write <type>` (the message type in hex). Messages that
  carry the device's identity or keys, and everything before the handshake
  finished, are never sent.

The R10 has no detection mode, hand or club setting, so a controller's
`set_detection_mode` is ignored.

Pre-built binaries for Linux and Windows are attached to each
[GitHub release](https://github.com/divotmaker/10over/releases).

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `ble` | Yes | Platform-agnostic BLE transport (BlueZ on Linux, btleplug on Windows/macOS) |
| `config` | No | The driver's settings (`config::Config`), with no BLE stack: builds for `wasm32-unknown-unknown` |
| `driver` | No | The flightrelay `DeviceDriver` for the R10 (`driver::R10`); implies `config` and `ble` |
| `frp` | No | The `tenover-frp` binary; implies `driver` |
| `serde` | No | Serialize/deserialize shot data types |
| `testing` | No | Frame builders with synthetic device identity, for downstream tests (`tenover::testing`) |

```sh
# Protocol-only (no BLE transport)
cargo build --no-default-features

# Settings only, e.g. for a WebAssembly settings form
cargo build --no-default-features --features config --target wasm32-unknown-unknown

# With the FRP binary
cargo build --features frp
```

## Recording raw traffic

Tools that record the raw BLE chunks a `Transport` carries (for a bug report or
a shot export) can keep the device's identity out of the recording:

- `gfdi::carries_identity(msg_type)` says whether a message type carries the
  device's identity or keys: device information (unit ID, Bluetooth names), FIT
  and file transfer (FIT files carry the device serial) and authentication.
  `GfdiFrame::carries_identity()` also covers an ACK of one of those types.
- `capture::IdentityFilter` is fed one direction's chunks in order and returns
  only the chunks cleared for recording. It holds a chunk until every frame it
  touches is complete, and drops every chunk holding a byte of an
  identity-bearing frame or of a frame it cannot classify.

```rust
use tenover::capture::IdentityFilter;

let mut rx = IdentityFilter::new();
for chunk in chunks {
    for cleared in rx.feed(&chunk, chunk.clone()) {
        record(cleared);
    }
}
```

See [WIRE.md §15](docs/WIRE.md#15-identity-in-raw-captures) for the rules.

## Dependencies

Minimal by design:

- **`prost`** — protobuf runtime
- **`thiserror`** — error enum derives
- **`zbus`** + **`libc`** (Linux, `ble` feature) — BlueZ D-Bus transport
- **`btleplug`** + **`tokio`** (Windows/macOS, `ble` feature) — cross-platform BLE
- **`flightrelay`** (optional, `config`/`driver`/`frp` features) — driver
  contract, settings, and the FRP server
- **`serde`** (optional, `serde` feature) — serialization support

No async runtime in the public API. The btleplug backend runs tokio internally
in a background thread; the caller-facing API is fully synchronous.
