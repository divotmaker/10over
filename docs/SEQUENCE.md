# Connection Lifecycle and Message Sequencing

Companion to [WIRE.md](WIRE.md), which covers framing, field encodings, and the
message catalog. This document covers *when* to send *what*: BLE connection,
MultiLink registration, GFDI handshake, protobuf session setup, and shot data flow.

All message formats, protobuf types, and field encodings referenced here are
defined in WIRE.md.

---

## 1. Overview

The R10 communicates over a single BLE GATT service (MultiLink). Unlike the
Mevo+ (three TCP services on ports 5100/1258/8080 with distinct protocols),
the R10 uses one transport for all communication.

**Protocol phases**:

| Phase | Messages | Duration |
|---|---|---|
| 1. BLE connection | Scan, connect, bond, GATT discover | Variable (~2-5s) |
| 2. MultiLink registration | REGISTER on char 2810 | ~50ms |
| 3. GFDI handshake | 5024, 5050 | ~1s |
| 4. Protobuf session | Subscribe alerts, wake up | ~100ms |
| 5. Shot reception | Alert notification (push) | Continuous |

**Key difference from ironsight**: The R10 auto-returns to ARMED after each
shot. No re-arm sequence is needed. The device pushes shot data as event alerts
without polling.

---

## 2. BLE Connection (Phase 1)

### 2.1 Scan

Scan for BLE peripherals matching either:

- Device name: "Approach R10"
- Manufacturer ID: 0x0087 (135, Garmin) in advertisement data

### 2.2 Connect and Bond

```
Host                                  R10
  |--- BLE connect ------------------>|
  |--- BLE bond (createBond) -------->|   one-time, OS remembers
  |<-- Bond complete -----------------|
  |                                   |
  |--- GATT service discovery ------->|
  |<-- Services: 6A4E2800-... --------|   MultiLink service
  |                                   |
  |--- Enable notifications --------->|   CCCD write on 6A4E2810
  |<-- Notification enabled ----------|
```

After enabling notifications on characteristic `6A4E2810`, the transport is
ready for MultiLink registration.

---

## 3. MultiLink Registration (Phase 2)

```
Host                                  R10
  |                                   |
  |--- REGISTER (13B on 2810) ------->|
  |    svc_id=1 (GFDI)               |
  |<-- REGISTER_RESPONSE (16B) ------|
  |    status=SUCCESS, handle=N      |
```

The device assigns a handle byte (typically 1-9, increments per session).
This handle must be prepended to every subsequent write to `2820` and
stripped from every notification on `2810`.

The device immediately pushes GFDI 5024 (device information) after
registration — no need to request it.

---

## 4. GFDI Handshake (Phase 3)

All messages are COBS-framed with CRC-16/ARC. Handle byte on every BLE chunk.
See WIRE.md for binary formats.

```
Host                                  R10
  |                                   |
  |<-- 5024 Device Information -------|  (1)
  |--- ACK 5024 + host info --------->|  (2)
  |                                   |
  |<-- 5011 FIT Definition ----------|  (3)  optional
  |--- ACK 5011 [0x00] ------------->|  (4)
  |                                   |
  |<-- 5012 FIT Data ----------------|  (5)  optional
  |--- ACK 5012 [0x00] ------------->|  (6)
  |                                   |
  |<-- 5050 Configuration (caps) ----|  (7)
  |--- ACK 5050 (empty) ------------>|  (8)
  |--- 5050 Configuration (host) --->|  (9)
  |<-- ACK 5050 --------------------|  (10)
```

### 4.1 Steps 1-2: Device Information (5024)

The device pushes its identity immediately after MultiLink registration.
Host parses: protocol version (150), product number (3622 = R10), unit ID,
software version, max packet size, device name strings.

Host responds with ACK wrapper (type 5000) containing host info payload:
protocol version 150, product 0xFFFF, unit 0xFFFFFFFF, app version,
friendly name, manufacturer, model.

**Important**: The 5024 response uses the 4-byte standard header format
(no compressed header), wrapped as an ACK (type 5000) of the device's 5024.

### 4.2 Steps 3-6: FIT Messages (ACK and skip)

The device may send FIT Definition (5011) and FIT Data (5012). These are
independent of the 5050 capability exchange.

**ACK both with status byte `[0x00]`.** No FIT parsing required.

### 4.3 Steps 7-10: Configuration (5050)

Device sends its capability bitmap. Host ACKs with empty payload, then sends
its own 5050 with the host capability bitmap.

**The host bitmap must include bit 30 (SwingSensor)** for launch monitor
support. Minimum host bitmap: 4 bytes with bit 30 set:
`[0x00, 0x00, 0x00, 0x40]`.

Handshake completes when the device ACKs the host's 5050.

---

## 5. Protobuf Session (Phase 4)

After handshake, all application messages use GFDI 5043 (request) / 5044
(response) with a 14-byte protobuf fragmentation header. Each payload is one
`Envelope` protobuf message (WIRE.md §8.2).

```
Host                                  R10
  |                                   |
  |--- 5043 Subscribe alerts -------->|  (11)
  |    Envelope { [30] alerts {       |
  |      subscribe = {                |
  |        topics = [{                |
  |          topic: TOPIC_SHOT_DATA   |
  |        }]                         |
  |      }                            |
  |    }}                             |
  |<-- 5044 Subscribe result ---------|  (12)
  |                                   |
  |--- 5043 Wake up ----------------->|  (13)
  |    Envelope { [38] monitor {      |
  |      wake = {}                    |
  |    }}                             |
  |<-- 5044 Wake result --------------|  (14)
  |    outcome: WOKEN|ALREADY_ACTIVE  |
  |                                   |
  |--- 5043 Environment (optional) -->|  (15)
  |    Envelope { [38] monitor {      |
  |      environment = {              |
  |        air_temperature_c,         |
  |        relative_humidity,         |
  |        elevation_m,               |
  |        air_density_kg_m3,         |
  |        tee_distance_yd            |
  |      }                            |
  |    }}                             |
  |<-- 5044 Environment result -------|  (16)
```

### 5.1 Alert Subscription (Steps 11-12)

Subscribe to `TOPIC_SHOT_DATA` (topic 8) on the alert channel (envelope
field 30). The device will push shot data and state changes as alert-channel
`Notification` messages.

### 5.2 Wake Up (Steps 13-14)

Send `WakeCommand` on the launch monitor channel (envelope field 38). The
device responds with a `WakeResult`: `OUTCOME_WOKEN` (0) or
`OUTCOME_ALREADY_ACTIVE` (1). After wake-up, the device enters the ARMED
state and begins monitoring for shots.

`OUTCOME_ALREADY_ACTIVE` (1) occurs when the device was armed from a prior
session and never went to sleep. The device won't send an ARMED state
transition in this case — it's already there.

### 5.3 Environment Settings (Steps 15-16, optional)

Send `EnvironmentSettings`: air temperature (°C), relative humidity
(0.0-1.0), elevation (m), air density (kg/m³), tee distance (yards). Not required for shot detection —
provides context for the device's internal flight model.

---

## 6. Shot Data Flow (Phase 5)

The device pushes state changes and shot data as alert-channel
`Notification` messages (GFDI 5043, envelope field 30), each carrying a
`MonitorReport` in field 1001.

```
Host                                  R10
  |                                   |
  |<-- Notification ------------------|   phase: ARMED
  |    (device armed, ready)          |
  |                                   |
  |         ... user swings ...       |
  |                                   |
  |<-- Notification ------------------|   phase: CAPTURING
  |<-- Notification ------------------|   phase: ANALYZING
  |                                   |
  |<-- Notification ------------------|   ★ SHOT DATA
  |    topics: [TOPIC_SHOT_DATA]      |
  |    [1001] monitor = {             |
  |      shot: {                      |
  |        shot_number: N,            |
  |        kind: KIND_REGULAR,        |
  |        ball: {                    |
  |          vertical_angle_deg,      |
  |          horizontal_angle_deg,    |
  |          speed_mps,               |
  |          spin_tilt_deg,           |
  |          spin_rpm,                |
  |          spin_source              |
  |        },                         |
  |        club: {                    |
  |          head_speed_mps,          |
  |          face_deg,                |
  |          path_deg,                |
  |          attack_deg               |
  |        },                         |
  |        timing: {                  |
  |          takeaway,                |
  |          transition,              |
  |          impact,                  |
  |          finish                   |
  |        }                          |
  |      }                            |
  |    }                              |
  |                                   |
  |<-- Notification ------------------|   phase: ARMED
  |    (device returns to ARMED)      |
  |    (repeat on next shot)          |
```

### 6.1 State Machine

```
IDLE → NOISE_CHECK → ARMED → CAPTURING → ANALYZING → ARMED (loop)
                                             ↓
                                          FAULTED
```

| State | Duration | Description |
|---|---|---|
| IDLE (0) | Until wake-up | Device powered on, not monitoring |
| NOISE_CHECK (1) | ~1-2s | Radar self-test on first arm |
| ARMED (2) | Indefinite | Waiting for ball/club motion |
| CAPTURING (3) | ~1-2s | Radar capturing shot data |
| ANALYZING (4) | ~1-2s | Computing ball/club metrics |
| FAULTED (5) | Until resolved | Hardware/environmental error |

Names are the `DeviceStatus.Phase` values (WIRE.md §10) without the `PHASE_`
prefix.

### 6.2 Shot Delivery

Shot data arrives in a dedicated `Notification` whose report carries `shot`.
A separate `Notification` with `phase: ARMED` follows as the device
re-arms. Total protobuf size per shot is ~60-80 bytes — always a single
GFDI frame.

### 6.3 Device Retransmissions

The R10 retransmits unACK'd alerts. If the host's ACK is delayed (e.g. due
to slow processing), the same shot may arrive multiple times.
**Deduplicate by `shot_number`.**

State changes should also be deduplicated — only act on transitions, not
repeated notifications of the same state.

### 6.4 No Re-Arm Required

Unlike the Mevo+ (which requires a manual re-arm sequence after each shot),
the R10 automatically returns to ARMED after analyzing. The client simply
waits for the next `Notification`.

---

## 7. Error Handling

### 7.1 Device Errors

Error events arrive as a `MonitorReport` with a `fault` field:

| Cause | Description | Typical recovery |
|---|---|---|
| CAUSE_TOO_HOT | Device temperature too high | Wait for cooldown |
| CAUSE_SIGNAL_OVERLOAD | Interference or reflective surface | Move device, clear area |
| CAUSE_NOT_LEVEL | Device not level | Re-level, recalibrate tilt |

### 7.2 GFDI Errors

Response status codes in the GFDI acknowledgment (see WIRE.md §5.4).
LENGTH_ERROR (5) typically indicates a malformed response frame — check
header format and length field calculation.

### 7.3 BLE Disconnection

BLE connection supervision timeout (OS-level, typically 4-6s) handles link
loss detection. Reconnect by re-scanning and repeating the full connection
sequence from Phase 1.

---

## 8. Minimum Viable Implementation

For a working R10 client, the minimum required steps:

| Step | Phase | Description |
|---|---|---|
| 1 | BLE | Scan for R10 (name "Approach R10" or manufacturer ID 0x0087) |
| 2 | BLE | Connect + bond (one-time) |
| 3 | BLE | GATT discover service `6A4E2800`, enable notifications on `6A4E2810` |
| 4 | MultiLink | Send REGISTER (svc_id=1) on `2810`, receive handle |
| 5 | GFDI | Receive 5024, respond with ACK + host info |
| 6 | GFDI | ACK 5011/5012 if received (no parsing) |
| 7 | GFDI | Receive 5050, ACK, send host 5050 (with bit 30 set) |
| 8 | Protobuf | Subscribe to `TOPIC_SHOT_DATA` alerts |
| 9 | Protobuf | Send `WakeCommand` |
| 10 | Protobuf | Receive `Notification` with `ShotReadout` → extract shot |

Steps 1-7 are the connection + handshake (~1s after BLE connect). Steps 8-9
are one-time session setup. Step 10 repeats for each shot.

### 8.1 What Can Be Skipped

| Feature | Status | Notes |
|---|---|---|
| FIT parsing (5011/5012) | Skip | ACK with `[0x00]`, no parsing needed |
| `EnvironmentSettings` | Skip | Not required for shot detection |
| Tilt calibration | Skip | Enhances accuracy but not required |
| Status polling | Skip | Shot data pushed automatically |
| XTEA authentication | Skip | Not used for bonded devices |

---

## 9. Comparison with ironsight/Mevo+ Lifecycle

| Aspect | R10 (10over) | Mevo+ (ironsight) |
|---|---|---|
| Connection | BLE scan + bond | WiFi join + TCP connect |
| Handshake phases | 3 (MultiLink + GFDI + protobuf) | 6 (DSP + AVR + PI + config + cam + arm) |
| Handshake time | ~1s | ~1.2s (no retries) to ~3.8s |
| Shot model | Push (device sends alerts) | Push (device sends D4/ED/EF burst) |
| Re-arm | Automatic | Manual (B0 [01 01] + drain + IDLE) |
| Post-shot work | ACK the alert | 0x69 × 2, drain, config query, re-arm |
| Keepalive | BLE supervision (OS) | Status poll every 1s to all 3 nodes |
| Mode change | N/A (single mode) | Disarm + config + re-arm |
| Shot data size | ~60-80B protobuf | ~158B + 172B + 138B binary |
| Protocol complexity | Moderate (MultiLink + COBS + CRC + proto) | High (3 buses, 30+ msg types) |
