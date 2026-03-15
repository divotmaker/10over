# Wire Protocol Reference

Interoperability specification for the Garmin R10 launch monitor protocol
over BLE GATT.

---

## 1. Transport Overview

The R10 communicates over **Bluetooth Low Energy** (BLE). Unlike TCP-based
protocols (ironsight/Mevo+), BLE is packet-oriented: each GATT write or
notification is a discrete message, not a byte stream.

The protocol stack:

```
┌──────────────────────────────────────────┐
│  Protobuf (GDI Smart container)          │  Application messages
├──────────────────────────────────────────┤
│  Protobuf fragmentation header (14B)     │  Multi-part reassembly
├──────────────────────────────────────────┤
│  GFDI framing (length + header + CRC)    │  Message routing
├──────────────────────────────────────────┤
│  COBS encoding                           │  Zero-byte elimination
├──────────────────────────────────────────┤
│  MultiLink routing (handle byte prefix)  │  Service multiplexing
├──────────────────────────────────────────┤
│  BLE GATT (write/notify characteristics) │  Transport
└──────────────────────────────────────────┘
```

All multi-byte integers are **little-endian** unless noted.

---

## 2. BLE GATT

### 2.1 Advertisement

The R10 advertises with:

| Field | Value |
|---|---|
| Manufacturer ID | 0x0087 (135, Garmin) |
| Product ID | 3622 (0x0E26), bytes [0:1] LE in manufacturer data |
| Device name | "Approach R10" |

### 2.2 MultiLink Service

The R10 uses the Garmin **MultiLink** service for all communication, not the
dedicated GFDI GATT service used by some other Garmin devices. All Garmin
UUIDs follow the pattern `6A4E[XXXX]-667B-11E3-949A-0800200C9A66`.

| Role | UUID suffix | Properties |
|---|---|---|
| **MultiLink Service** | `2800` | — |
| **Channel 0 R/W** | `2810` | read, write, notify — inbound data + REGISTER |
| **Channel 0 Write** | `2820` | write — outbound GFDI data |
| Config | `2803` | read, write — 4-byte config register (not command channel) |

The `2810` characteristic is bidirectional: subscribe for notifications (all
inbound data) and write for REGISTER commands. The `2820` characteristic is
write-only for outbound GFDI data with a handle byte prefix.

### 2.3 MultiLink Registration

Before GFDI communication begins, the host must register for the GFDI service
within MultiLink. Send a 13-byte REGISTER command to characteristic `2810`:

```
Offset  Size  Field
0       1     reserved (0x00)
1       1     command (0x00 = REGISTER)
2       8     transaction_id (u64 LE, typically 1)
10      2     service_id (u16 LE, 1 = GFDI)
12      1     reserved (0x00)
```

Response (16 bytes, notification on `2810`):

```
Offset  Size  Field
0       1     reserved (0x00)
1       1     response (0x01 = REGISTER_RESPONSE)
2       8     transaction_id (echo)
10      2     service_id (echo)
12      1     status (0x00 = SUCCESS)
13      1     handle (assigned by device, typically 1-9)
14      2     flags (0x0000)
```

The assigned `handle` byte must be prepended to **every** BLE write to `2820`
and stripped from **every** notification on `2810` for the remainder of the
session.

### 2.4 Handle Byte Routing

After registration, all data flows through the assigned handle:

- **Writes** to `2820`: first byte is the handle, followed by GFDI/COBS data
- **Notifications** on `2810`: first byte is the handle, followed by GFDI/COBS data
- **MTU chunking**: handle byte is prepended to EVERY chunk, not just the first

With MTU 23 (the R10 default), each BLE write carries 20 bytes total: 1 handle
byte + 19 bytes of COBS/GFDI data.

### 2.5 MTU

The R10 supports MTU up to 515. Many BLE adapters (especially USB dongles)
negotiate down to MTU 23, yielding 20 usable bytes per write (23 ATT_MTU -
3 ATT header). The library handles chunking at
any MTU.

### 2.6 Bonding

Standard BLE bonding via OS APIs. XTEA authentication is NOT required for
bonded devices. Bonding is a one-time operation; the OS remembers the bond.

---

## 3. COBS Framing

All GFDI messages are wrapped in Consistent Overhead Byte Stuffing (COBS)
with `0x00` as the delimiter byte.

### 3.1 Wire Format

```
[0x00] [COBS-encoded frame] [0x00]
```

The leading and trailing `0x00` bytes delimit the message. COBS encoding
guarantees that `0x00` never appears in the encoded payload.

### 3.2 Stream Reassembly

BLE notifications may split a single COBS-framed message across multiple
notification payloads. The receiver accumulates bytes (after stripping the
handle prefix from each notification) into a buffer until a `0x00` delimiter
is encountered. The bytes between two `0x00` delimiters form one COBS-encoded
frame.

### 3.3 COBS Algorithm

Standard COBS. Reference: S. Cheshire and M. Baker, "Consistent Overhead
Byte Stuffing", IEEE/ACM Transactions on Networking, 1999.

---

## 4. CRC-16

CRC-16/ARC computed over the GFDI frame bytes (everything except the
trailing 2-byte CRC field itself).

### 4.1 Parameters

| Parameter | Value |
|---|---|
| Algorithm | CRC-16/ARC (CRC-16/LHA, CRC-IBM) |
| Polynomial | 0x8005 (reflected: 0xA001) |
| Init | 0x0000 |
| Reflect in | true |
| Reflect out | true |
| XOR out | 0x0000 |
| Check ("123456789") | 0xBB3D |

### 4.2 Implementation

Nibble-based (4-bit) lookup table:

```
TABLE = [0x0000, 0xCC01, 0xD801, 0x1400, 0xF001, 0x3C00, 0x2800, 0xE401,
         0xA001, 0x6C00, 0x7800, 0xB401, 0x5000, 0x9C01, 0x8801, 0x4400]

fn crc_byte(byte: u8, crc: u16) -> u16 {
    // Process low nibble
    let tmp = ((crc >> 4) & 0x0FFF) ^ TABLE[(crc & 0xF) as usize]
                                    ^ TABLE[(byte & 0xF) as usize];
    // Process high nibble
    ((tmp >> 4) & 0x0FFF) ^ TABLE[(tmp & 0xF) as usize]
                          ^ TABLE[((byte >> 4) & 0xF) as usize]
}

fn crc(data: &[u8]) -> u16 {
    let mut acc: u16 = 0;
    for &b in data {
        acc = crc_byte(b, acc);
    }
    acc
}
```

---

## 5. GFDI Frame Format

After COBS decoding, the frame has this structure:

```
┌────────────┬─────────────────┬───────────┬──────────┐
│ length: u16│ header: 2-4B    │ payload   │ crc: u16 │
└────────────┴─────────────────┴───────────┴──────────┘
```

The `length` field holds the total frame size in bytes, including itself
and the CRC. CRC is computed over all bytes from `length` through the end
of `payload` (everything except the final 2 CRC bytes).

### 5.1 Header: Standard (4-byte)

```
Offset  Size  Field
0       2     length (u16 LE)
2       2     message_type (u16 LE) — e.g. 5024, 5043
4       ...   payload
N-2     2     crc (u16 LE)
```

### 5.2 Header: Compressed (2-byte, protocol version >= 150)

```
Offset  Size  Field
0       2     length (u16 LE)
2       1     message_type - 5000 (u8, compressed)
3       1     transaction_id | 0x80 (u8, high bit set = has txn ID)
4       ...   payload
N-2     2     crc (u16 LE)
```

When byte [3] has the high bit set:
- `message_type = (byte[2] & 0xFF) + 5000`
- `transaction_id = byte[3] & 0x7F`

Protocol version >= 150 is negotiated during the 5024 handshake.

**Note:** In practice, the R10 uses 4-byte standard headers for the initial
handshake messages (5024, 5050) and compressed headers for subsequent
protobuf messages (5043, 5044).

### 5.3 Acknowledgment (type 5000)

```
Offset  Size  Field
0       2     length (u16 LE)
2       2     original_message_type (u16 LE)
4       1     response_status (u8)
5       ...   response_payload (optional)
N-2     2     crc (u16 LE)
```

When using compressed headers, the ACK uses type 0x00 (5000 - 5000):

```
Offset  Size  Field
0       2     length (u16 LE)
2       1     0x00 (compressed type for Acknowledgment)
3       1     transaction_id | 0x80
4       2     original_message_type (u16 LE)
6       1     response_status (u8)
7       ...   response_payload (optional)
N-2     2     crc (u16 LE)
```

### 5.4 Response Status Codes

| Value | Name | Description |
|---|---|---|
| 0 | ACK | Success |
| 1 | NAK | General failure |
| 2 | UNKNOWN_OR_NOT_SUPPORTED | Message type not recognized |
| 3 | COBS_DECODER_ERROR | COBS decode failed |
| 4 | CRC_ERROR | CRC mismatch |
| 5 | LENGTH_ERROR | Frame length invalid |

---

## 6. Message Types

| ID | Name | Direction | Notes |
|---|---|---|---|
| 5000 | Acknowledgment | both | Response wrapper (Section 5.3) |
| 5011 | FIT Definition | device → host | FIT protocol |
| 5012 | FIT Data | device → host | FIT capabilities |
| 5024 | Device Information | device → host | Binary, first handshake message |
| 5043 | Protobuf Request | both | Protobuf payload (Section 8) |
| 5044 | Protobuf Response | both | Protobuf payload (Section 8) |
| 5050 | Configuration | both | Capability bitmap exchange |

---

## 7. Handshake Messages

### 7.1 Device Information (5024)

The device sends this immediately after MultiLink registration. Contains
device identity, protocol version, and transport parameters.

**Device → Host payload:**

```
Offset  Size  Field                    Type
0       2     protocol_version         u16 LE
2       2     product_number           u16 LE      (R10 = 3622)
4       4     unit_id                  u32 LE      (unique device serial)
8       2     software_version         u16 LE
10      2     max_packet_size          u16 LE
12      1     bt_friendly_name_len     u8
13      N     bt_friendly_name         UTF-8 string
13+N    1     device_name_len          u8
14+N    M     device_name              UTF-8 string
...     1     model_name_len           u8
...     K     model_name               UTF-8 string
```

Strings are length-prefixed: 1 byte length, then UTF-8 bytes. Empty
string = `[0x00]`.

**Host → Device response** (ACK wrapper, type 5000, with host info payload):

```
Offset  Size  Field                    Type         Value
0       2     host_protocol_version    u16 LE       150 (if device >= 150) or 113
2       2     product_number           u16 LE       0xFFFF (65535)
4       4     unit_id                  u32 LE       0xFFFFFFFF
8       2     app_version              u16 LE       (application version)
10      2     max_packet_size          u16 LE       0xFFFF (65535)
12      1+N   friendly_name            length-prefixed UTF-8
...     1+M   manufacturer             length-prefixed UTF-8
...     1+K   model                    length-prefixed UTF-8
...     1     unknown_flag             u8           1
```

Protocol version >= 150 enables compressed header messaging for subsequent
messages.

### 7.2 FIT Capabilities (5011/5012)

Device may send FIT Definition (5011) and FIT Data (5012) containing a
`CapabilitiesMesg`. These messages are **independent** of the 5050
handshake.

**For R10 implementation**: ACK both with status byte `[0x00]` (success).
No FIT parsing required.

### 7.3 Configuration (5050)

Capability bitmap exchange. Determines which features the host and device
mutually support.

**Device → Host payload:**

```
Offset  Size  Field
0       1     bitmap_size (u8) — number of bytes in bitmap
1       N     capability_bitmap (N bytes)
```

Each capability bit `i` is encoded as: `bitmap[i / 8] & (1 << (i % 8))`

**Host response**: ACK with empty payload `[]`.

**Host → Device** (host sends its own 5050):

```
Offset  Size  Field
0       1     bitmap_size (u8)
1       N     host_capability_bitmap (N bytes)
```

The host bitmap **must** include bit 30 (SwingSensor) for launch monitor
support. Minimum host bitmap: 4 bytes with bit 30 set: `[0x00, 0x00, 0x00, 0x40]`.

### 7.4 Capability Bits

| Bit | Name | Notes |
|---|---|---|
| 1 | GolfFitLink | |
| 3 | Sync | |
| 4 | DeviceInitiatesSync | |
| 5 | HostInitiatedSyncRequests | |
| **30** | **SwingSensor** | **Required for R10 launch monitor** |
| 71 | CurrentTimeRequest | |
| 76 | MultiLinkService | |

---

## 8. Protobuf Messages

After the GFDI handshake (5024 + 5050), all application messages use GFDI
types 5043 (request) and 5044 (response) carrying serialized protobuf
payloads.

### 8.1 Fragmentation Header

Every protobuf payload within a 5043/5044 message is prefixed with a
14-byte fragmentation header:

```
Offset  Size  Field
0       2     request_id (u16 LE) — unique per multi-part message
2       4     packet_offset (u32 LE) — byte offset in reassembled message
6       4     total_length (u32 LE) — total protobuf message size
10      4     chunk_size (u32 LE) — payload bytes in this GFDI message
14      ...   protobuf_payload_chunk
```

R10 shot data is ~60-80 bytes — always fits in a single frame
(`offset=0`, `total_length=chunk_size`).

### 8.2 Smart Container

All protobuf payloads are wrapped in a `GDI.Proto.Smart.Smart` message:

```protobuf
message Smart {
  extensions 1 to 55;
}
```

R10-relevant extensions:

| Field | Service | Purpose |
|---|---|---|
| 30 | EventSharingService | Alert subscription and notification |
| 38 | LaunchMonitorService | R10 wake-up, config, status |

### 8.3 EventSharing Service (extension field 30)

```protobuf
message EventSharingService {
  optional SubscribeRequest     subscribe_request  = 1;
  optional SubscribeResponse    subscribe_response = 2;
  optional AlertNotification    alert_notification = 3;
}

message SubscribeRequest {
  repeated AlertMessage alerts = 1;
}

message AlertMessage {
  optional AlertType type = 1;
}

enum AlertType {
  ACTIVITY_START              = 0;
  ACTIVITY_STOP               = 1;
  ACTIVITY_DISTANCE           = 2;
  ACTIVITY_TIME               = 3;
  ACTIVITY_AUTO_LAP           = 4;
  ACTIVITY_MANUAL_LAP         = 5;
  ACTIVITY_TRANSITION         = 6;
  SWITCHER_STATUS_CHANGE      = 7;
  LAUNCH_MONITOR              = 8;   // R10 shot data
  // 9-17: navigation, driving, golf, rangefinder, etc.
}

message AlertNotification {
  repeated AlertType alert_type = 1;
  extensions 1000 to max;
  // AlertDetails registered at field 1001
}
```

### 8.4 LaunchMonitor Service (extension field 38)

```protobuf
message Service {
  optional StatusRequest                 status_request          = 1;
  optional StatusResponse                status_response         = 2;
  optional WakeUpRequest                 wake_up_request         = 3;
  optional WakeUpResponse                wake_up_response        = 4;
  optional TiltRequest                   tilt_request            = 5;
  optional TiltResponse                  tilt_response           = 6;
  optional StartTiltCalibrationRequest   start_tilt_cal_request  = 7;
  optional StartTiltCalibrationResponse  start_tilt_cal_response = 8;
  optional ResetTiltCalibrationRequest   reset_tilt_cal_request  = 9;
  optional ResetTiltCalibrationResponse  reset_tilt_cal_response = 10;
  optional ShotConfigRequest             shot_config_request     = 11;
  optional ShotConfigResponse            shot_config_response    = 12;
}

message WakeUpResponse {
  enum ResponseStatus {
    SUCCESS       = 0;
    ALREADY_AWAKE = 1;
    UNKNOWN_ERROR = 2;
  }
  optional ResponseStatus status = 1;
}
```

### 8.5 AlertDetails (extension field 1001 on AlertNotification)

Shot data and state changes are delivered as `AlertDetails` within
`AlertNotification` messages:

```protobuf
message AlertDetails {
  optional State             state            = 1;
  optional Metrics           metrics          = 2;
  optional Error             error            = 3;
  optional CalibrationStatus tilt_calibration = 4;

  extend AlertNotification {
    optional AlertDetails details = 1001;
  }
}
```

---

## 9. Shot Data

Shot data arrives via `AlertDetails.metrics` within an `AlertNotification`
pushed by the device (GFDI 5043).

### 9.1 Metrics

```protobuf
message Metrics {
  enum ShotType {
    PRACTICE = 0;
    NORMAL   = 1;
  }
  optional uint32       shot_id       = 1;
  optional ShotType     shot_type     = 2;
  optional BallMetrics  ball_metrics  = 3;
  optional ClubMetrics  club_metrics  = 4;
  optional SwingMetrics swing_metrics = 5;
}
```

### 9.2 BallMetrics

```protobuf
message BallMetrics {
  enum SpinCalculationType {
    RATIO       = 0;
    BALL_FLIGHT = 1;
    OTHER       = 2;
    MEASURED    = 3;   // requires marked ball
  }
  enum GolfBallType {
    UNKNOWN      = 0;
    CONVENTIONAL = 1;  // standard unmarked ball
    MARKED       = 2;  // marked ball (enables measured spin)
  }
  optional float                launch_angle          = 1;  // degrees (VLA)
  optional float                launch_direction      = 2;  // degrees (HLA)
  optional float                ball_speed            = 3;  // m/s
  optional float                spin_axis             = 4;  // degrees
  optional float                total_spin            = 5;  // RPM
  optional SpinCalculationType  spin_calculation_type = 6;
  optional GolfBallType         golf_ball_type        = 7;
}
```

### 9.3 ClubMetrics

```protobuf
message ClubMetrics {
  optional float club_head_speed = 1;  // m/s
  optional float club_angle_face = 2;  // degrees
  optional float club_angle_path = 3;  // degrees
  optional float attack_angle    = 4;  // degrees
}
```

### 9.4 SwingMetrics

```protobuf
message SwingMetrics {
  optional uint32 back_swing_start_time   = 1;  // ms (absolute device time)
  optional uint32 down_swing_start_time   = 2;  // ms (absolute device time)
  optional uint32 impact_time             = 3;  // ms (absolute device time)
  optional uint32 follow_through_end_time = 4;  // ms (absolute device time)
  optional uint32 end_recording_time      = 5;  // ms (absolute device time)
}
```

### 9.5 Units Summary

| Field | Unit | Notes |
|---|---|---|
| ball_speed | m/s | Multiply by 2.237 for mph |
| club_head_speed | m/s | Same conversion |
| launch_angle | degrees | Vertical launch angle |
| launch_direction | degrees | Horizontal launch angle |
| spin_axis | degrees | Decompose: `backspin = total_spin * cos(spin_axis)`, `sidespin = total_spin * sin(spin_axis)` |
| total_spin | RPM | |
| club_angle_face | degrees | Face angle at impact |
| club_angle_path | degrees | Club path |
| attack_angle | degrees | Angle of attack |
| swing timing | ms | Absolute timestamps (ms since device boot) |

### 9.6 Observations from Live Device

- Club face/path/attack_angle all 0.0 on slow swings — R10 needs minimum club
  speed to track club data
- `spin_calculation_type` is always RATIO with an unmarked ball
- `follow_through_end_time` occasionally reports sentinel values (~4.3 billion ms)
  when follow-through was not tracked
- Device retransmits unACK'd alerts; deduplicate by `shot_id`

---

## 10. Device State

```protobuf
message State {
  enum StateType {
    STANDBY           = 0;
    INTERFERENCE_TEST = 1;
    WAITING           = 2;  // armed, waiting for shot
    RECORDING         = 3;  // capturing shot data
    PROCESSING        = 4;  // computing metrics
    ERROR             = 5;
  }
  optional StateType state = 1;
}
```

State transitions during normal operation:

```
STANDBY → INTERFERENCE_TEST → WAITING → RECORDING → PROCESSING → WAITING (loop)
```

The R10 auto-returns to WAITING after each shot — no re-arm required.

---

## 11. Error Messages

```protobuf
message Error {
  enum ErrorCode {
    UNKNOWN          = 0;
    OVERHEATING      = 1;
    RADAR_SATURATION = 2;
    PLATFORM_TILTED  = 3;
  }
  enum Severity {
    WARNING = 0;
    SERIOUS = 1;
    FATAL   = 2;
  }
  optional ErrorCode code       = 1;
  optional Severity  severity   = 2;
  optional Tilt      deviceTilt = 3;
}
```

---

## 12. Shot Configuration (Optional)

Environmental data sent to the device before or during a session:

```protobuf
message ShotConfigRequest {
  optional float temperature = 1;  // Celsius
  optional float humidity    = 2;  // 0.0-1.0 (normalized, not percent)
  optional float altitude    = 3;  // meters
  optional float air_density = 4;  // kg/m^3
  optional float tee_range   = 5;  // yards
}
```

Not required for shot detection. Provides environmental context for the
device's internal flight model.

---

## 13. Tilt and Calibration

```protobuf
message Tilt {
  optional float roll  = 1;  // degrees
  optional float pitch = 2;  // degrees
}

message CalibrationStatus {
  enum StatusType {
    IN_BOUNDS               = 1;
    RECALIBRATION_SUGGESTED = 2;
    RECALIBRATION_REQUIRED  = 3;
  }
  enum CalibrationResult {
    SUCCESS     = 0;
    ERROR       = 1;
    UNIT_MOVING = 2;
  }
  optional StatusType        status = 1;
  optional CalibrationResult result = 2;
}
```

---

## 14. Comparison with ironsight/Mevo+

| Aspect | R10 (10over) | Mevo+ (ironsight) |
|---|---|---|
| Transport | BLE GATT (MultiLink) | TCP (WiFi, port 5100) |
| Byte order | Little-endian | Big-endian |
| Framing | COBS + 0x00 delimiters | 0xF0/0xF1 + byte stuffing |
| Integrity | CRC-16/ARC | 16-bit sum |
| Payload encoding | Protobuf (proto2) | Custom binary (INT24, FLOAT40) |
| Shot delivery | Push (AlertNotification) | Push (D4/ED/EF burst) |
| Handshake | MultiLink + 5024 + 5050 (~1s) | DSP + AVR + PI sync (~1.2s) |
| State machine | STANDBY → WAITING → RECORDING → PROCESSING | IDLE → ARMED → tracking |
| Re-arm | Automatic (returns to WAITING) | Manual (0xB0 [01 01] after IDLE) |
| Encryption | None (bonded devices) | None |
| Keepalive | BLE supervision timeout (OS) | Status poll every ~1s |
