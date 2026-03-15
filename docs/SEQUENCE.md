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
| 5. Shot reception | AlertNotification (push) | Continuous |

**Key difference from ironsight**: The R10 auto-returns to WAITING after each
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
(response) with a 14-byte protobuf fragmentation header. Payloads are
`GDI.Proto.Smart.Smart` protobuf messages.

```
Host                                  R10
  |                                   |
  |--- 5043 Subscribe alerts -------->|  (11)
  |    Smart { [30] = EventSharing {  |
  |      subscribe_request = {        |
  |        alerts = [{                |
  |          type: LAUNCH_MONITOR     |
  |        }]                         |
  |      }                            |
  |    }}                             |
  |<-- 5044 Subscribe response -------|  (12)
  |                                   |
  |--- 5043 Wake up ----------------->|  (13)
  |    Smart { [38] = LaunchMonitor { |
  |      wake_up_request = {}         |
  |    }}                             |
  |<-- 5044 Wake up response ---------|  (14)
  |    status: SUCCESS|ALREADY_AWAKE  |
  |                                   |
  |--- 5043 ShotConfig (optional) --->|  (15)
  |    Smart { [38] = LaunchMonitor { |
  |      shot_config_request = {      |
  |        temperature, humidity,     |
  |        altitude, air_density,     |
  |        tee_range                  |
  |      }                            |
  |    }}                             |
  |<-- 5044 ShotConfigResponse -------|  (16)
```

### 5.1 Alert Subscription (Steps 11-12)

Subscribe to `LAUNCH_MONITOR` alerts (AlertType 8) via EventSharing extension
(field 30). The device will push shot data and state changes as
`AlertNotification` messages.

### 5.2 Wake Up (Steps 13-14)

Send `WakeUpRequest` via LaunchMonitor extension (field 38). Device responds
with SUCCESS (0) or ALREADY_AWAKE (1). After wake-up, the device enters the
WAITING state and begins monitoring for shots.

ALREADY_AWAKE (status 1) occurs when the device was armed from a prior
session and never went to sleep. The device won't send a WAITING state
transition in this case — it's already there.

### 5.3 Shot Configuration (Steps 15-16, optional)

Send environmental data: temperature (°C), humidity (0.0-1.0), altitude (m),
air density (kg/m³), tee range (yards). Not required for shot detection —
provides context for the device's internal flight model.

---

## 6. Shot Data Flow (Phase 5)

The device pushes state changes and shot data via `AlertNotification`
(GFDI 5043, EventSharing extension field 30, AlertDetails extension field 1001).

```
Host                                  R10
  |                                   |
  |<-- AlertNotification -------------|   state: WAITING
  |    (device armed, ready)          |
  |                                   |
  |         ... user swings ...       |
  |                                   |
  |<-- AlertNotification -------------|   state: RECORDING
  |<-- AlertNotification -------------|   state: PROCESSING
  |                                   |
  |<-- AlertNotification -------------|   ★ SHOT DATA
  |    alert_type: [LAUNCH_MONITOR]   |
  |    [1001] = AlertDetails {        |
  |      metrics: {                   |
  |        shot_id: N,                |
  |        shot_type: NORMAL,         |
  |        ball_metrics: {            |
  |          launch_angle,            |
  |          launch_direction,        |
  |          ball_speed,              |
  |          spin_axis,               |
  |          total_spin,              |
  |          spin_calculation_type    |
  |        },                         |
  |        club_metrics: {            |
  |          club_head_speed,         |
  |          club_angle_face,         |
  |          club_angle_path,         |
  |          attack_angle             |
  |        },                         |
  |        swing_metrics: {           |
  |          back_swing_start_time,   |
  |          down_swing_start_time,   |
  |          impact_time,             |
  |          follow_through_end_time  |
  |        }                          |
  |      }                            |
  |    }                              |
  |                                   |
  |<-- AlertNotification -------------|   state: WAITING
  |    (device returns to WAITING)    |
  |    (repeat on next shot)          |
```

### 6.1 State Machine

```
STANDBY → INTERFERENCE_TEST → WAITING → RECORDING → PROCESSING → WAITING (loop)
                                                         ↓
                                                       ERROR
```

| State | Duration | Description |
|---|---|---|
| STANDBY | Until wake-up | Device powered on, not monitoring |
| INTERFERENCE_TEST | ~1-2s | Radar self-test on first arm |
| WAITING | Indefinite | Armed, waiting for ball/club motion |
| RECORDING | ~1-2s | Radar capturing shot data |
| PROCESSING | ~1-2s | Computing ball/club metrics |
| ERROR | Until resolved | Hardware/environmental error |

### 6.2 Shot Delivery

Shot data arrives in a dedicated `AlertNotification` containing `metrics`.
A separate `AlertNotification` with `state: WAITING` follows as the device
re-arms. Total protobuf size per shot is ~60-80 bytes — always a single
GFDI frame.

### 6.3 Device Retransmissions

The R10 retransmits unACK'd alerts. If the host's ACK is delayed (e.g. due
to slow processing), the same shot may arrive multiple times.
**Deduplicate by `shot_id`.**

State changes should also be deduplicated — only act on transitions, not
repeated notifications of the same state.

### 6.4 No Re-Arm Required

Unlike the Mevo+ (which requires a manual re-arm sequence after each shot),
the R10 automatically returns to WAITING after processing. The client simply
waits for the next `AlertNotification`.

---

## 7. Error Handling

### 7.1 Device Errors

Error events arrive as `AlertDetails` with an `error` field:

| ErrorCode | Description | Typical recovery |
|---|---|---|
| OVERHEATING | Device temperature too high | Wait for cooldown |
| RADAR_SATURATION | Interference or reflective surface | Move device, clear area |
| PLATFORM_TILTED | Device not level | Re-level, recalibrate tilt |

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
| 8 | Protobuf | Subscribe to LAUNCH_MONITOR alerts |
| 9 | Protobuf | Send WakeUpRequest |
| 10 | Protobuf | Receive AlertNotification with Metrics → extract shot |

Steps 1-7 are the connection + handshake (~1s after BLE connect). Steps 8-9
are one-time session setup. Step 10 repeats for each shot.

### 8.1 What Can Be Skipped

| Feature | Status | Notes |
|---|---|---|
| FIT parsing (5011/5012) | Skip | ACK with `[0x00]`, no parsing needed |
| ShotConfigRequest | Skip | Not required for shot detection |
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
