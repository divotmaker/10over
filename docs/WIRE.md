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
│  Protobuf (envelope message)             │  Application messages
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
| 5003 | File Transfer | both | Not used by the R10 client |
| 5004 | File Data | both | Not used by the R10 client |
| 5011 | FIT Definition | device → host | FIT protocol |
| 5012 | FIT Data | device → host | FIT capabilities |
| 5024 | Device Information | device → host | Binary, first handshake message |
| 5043 | Protobuf Request | both | Protobuf payload (Section 8) |
| 5044 | Protobuf Response | both | Protobuf payload (Section 8) |
| 5050 | Configuration | both | Capability bitmap exchange |
| 5101-5112 | Authentication | both | Pairing and session keys; not used by the R10 client |

`tenover::gfdi` exports these as `MSG_*` constants (`MSG_AUTH` is the
5101-5112 range).

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
FIT capabilities message. These messages are **independent** of the 5050
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

### 8.2 Envelope

Every protobuf payload is one `Envelope` message (`proto/envelope.proto`),
which carries exactly one channel:

```protobuf
message Envelope {
  optional AlertChannel   alerts  = 30;
  optional MonitorChannel monitor = 38;
}
```

| Field | Channel | Purpose |
|---|---|---|
| 30 | `AlertChannel` | Alert subscription and notification |
| 38 | `MonitorChannel` | R10 wake-up, config, status |

Fields this schema does not declare are skipped when decoding.

### 8.3 Alert Channel (envelope field 30)

```protobuf
message AlertChannel {
  optional SubscribeCommand subscribe        = 1;
  optional SubscribeResult  subscribe_result = 2;
  optional Notification     notification     = 3;
}

message SubscribeCommand {
  repeated TopicSelection topics = 1;
}

message TopicSelection {
  optional Topic topic = 1;
}

enum Topic {
  TOPIC_SHOT_DATA = 8;   // R10 shot data
}

message Notification {
  repeated Topic         topics  = 1;
  optional MonitorReport monitor = 1001;
}
```

The schema is in `proto/alerts.proto`. Topic values other than
`TOPIC_SHOT_DATA` decode as plain integers.

### 8.4 Launch Monitor Channel (envelope field 38)

```protobuf
message MonitorChannel {
  optional StateQuery            state_query            = 1;
  optional StateReply            state_reply            = 2;
  optional WakeCommand           wake                   = 3;
  optional WakeResult            wake_result            = 4;
  optional LevelQuery            level_query            = 5;
  optional LevelReply            level_reply            = 6;
  optional LevelCalibrateCommand level_calibrate        = 7;
  optional LevelCalibrateResult  level_calibrate_result = 8;
  optional LevelClearCommand     level_clear            = 9;
  optional LevelClearResult      level_clear_result     = 10;
  optional EnvironmentSettings   environment            = 11;
  optional EnvironmentResult     environment_result     = 12;
}

message WakeResult {
  enum Outcome {
    OUTCOME_WOKEN          = 0;
    OUTCOME_ALREADY_ACTIVE = 1;
    OUTCOME_FAILED         = 2;
  }
  optional Outcome outcome = 1;
}
```

### 8.5 Monitor Report (field 1001 of Notification)

Shot data and state changes are delivered as a `MonitorReport` inside
alert-channel `Notification` messages:

```protobuf
message MonitorReport {
  optional DeviceStatus status = 1;
  optional ShotReadout  shot   = 2;
  optional Fault        fault  = 3;
  optional LevelCheck   level  = 4;
}
```

---

## 9. Shot Data

Shot data arrives in `MonitorReport.shot` within a `Notification` pushed by
the device (GFDI 5043).

### 9.1 ShotReadout

```protobuf
message ShotReadout {
  enum Kind {
    KIND_WARMUP  = 0;
    KIND_REGULAR = 1;
  }
  optional uint32       shot_number = 1;
  optional Kind         kind        = 2;
  optional BallLaunch   ball        = 3;
  optional ClubDelivery club        = 4;
  optional SwingTiming  timing      = 5;
}
```

### 9.2 BallLaunch

```protobuf
message BallLaunch {
  enum SpinSource {
    SPIN_SOURCE_ESTIMATED    = 0;
    SPIN_SOURCE_FLIGHT_MODEL = 1;
    SPIN_SOURCE_UNCLASSIFIED = 2;
    SPIN_SOURCE_DIRECT       = 3;  // read off a marked ball
  }
  enum Marking {
    MARKING_UNSPECIFIED = 0;
    MARKING_PLAIN       = 1;  // unmarked ball
    MARKING_DOTTED      = 2;  // marked ball (enables measured spin)
  }
  optional float      vertical_angle_deg   = 1;  // degrees (VLA)
  optional float      horizontal_angle_deg = 2;  // degrees (HLA)
  optional float      speed_mps            = 3;  // m/s
  optional float      spin_tilt_deg        = 4;  // degrees (spin axis)
  optional float      spin_rpm             = 5;  // RPM (total spin)
  optional SpinSource spin_source          = 6;
  optional Marking    marking              = 7;
}
```

### 9.3 ClubDelivery

```protobuf
message ClubDelivery {
  optional float head_speed_mps = 1;  // m/s
  optional float face_deg       = 2;  // degrees
  optional float path_deg       = 3;  // degrees
  optional float attack_deg     = 4;  // degrees
}
```

### 9.4 SwingTiming

```protobuf
message SwingTiming {
  optional uint32 takeaway    = 1;  // ms (absolute device time)
  optional uint32 transition  = 2;  // ms (absolute device time)
  optional uint32 impact      = 3;  // ms (absolute device time)
  optional uint32 finish      = 4;  // ms (absolute device time)
  optional uint32 capture_end = 5;  // ms (absolute device time)
}
```

### 9.5 Units Summary

| Field | Unit | Notes |
|---|---|---|
| speed_mps | m/s | Ball speed. Multiply by 2.237 for mph |
| head_speed_mps | m/s | Club head speed. Same conversion |
| vertical_angle_deg | degrees | Vertical launch angle |
| horizontal_angle_deg | degrees | Horizontal launch angle |
| spin_tilt_deg | degrees | Spin axis. Decompose: `backspin = spin_rpm * cos(spin_tilt)`, `sidespin = spin_rpm * sin(spin_tilt)` |
| spin_rpm | RPM | Total spin |
| face_deg | degrees | Face angle at impact |
| path_deg | degrees | Club path |
| attack_deg | degrees | Angle of attack |
| swing timing | ms | Absolute timestamps (ms since device boot) |

### 9.6 Observations from Live Device

- Club face/path/attack angle all 0.0 on slow swings — R10 needs minimum club
  speed to track club data
- `spin_source` is always `SPIN_SOURCE_ESTIMATED` with an unmarked ball
- `finish` occasionally reports sentinel values (~4.3 billion ms) when
  follow-through was not tracked
- Device retransmits unACK'd alerts; deduplicate by `shot_number`

---

## 10. Device State

```protobuf
message DeviceStatus {
  enum Phase {
    PHASE_IDLE        = 0;
    PHASE_NOISE_CHECK = 1;
    PHASE_ARMED       = 2;  // waiting for a shot
    PHASE_CAPTURING   = 3;  // recording a shot
    PHASE_ANALYZING   = 4;  // computing metrics
    PHASE_FAULTED     = 5;
  }
  optional Phase phase = 1;
}
```

State transitions during normal operation:

```
IDLE (0) → NOISE_CHECK (1) → ARMED (2) → CAPTURING (3) → ANALYZING (4) → ARMED (loop)
```

The R10 auto-returns to ARMED after each shot — no re-arm required.

---

## 11. Faults

```protobuf
message Fault {
  enum Cause {
    CAUSE_UNSPECIFIED     = 0;
    CAUSE_TOO_HOT         = 1;
    CAUSE_SIGNAL_OVERLOAD = 2;  // radar saturated
    CAUSE_NOT_LEVEL       = 3;  // device tilted
  }
  enum Urgency {
    URGENCY_LOW      = 0;
    URGENCY_HIGH     = 1;
    URGENCY_CRITICAL = 2;
  }
  optional Cause       cause       = 1;
  optional Urgency     urgency     = 2;
  optional Orientation orientation = 3;
}
```

---

## 12. Environment Settings (Optional)

Environmental data sent to the device before or during a session:

```protobuf
message EnvironmentSettings {
  optional float air_temperature_c = 1;  // Celsius
  optional float relative_humidity = 2;  // 0.0-1.0 (fraction, not percent)
  optional float elevation_m       = 3;  // meters
  optional float air_density_kg_m3 = 4;  // kg/m^3
  optional float tee_distance_yd   = 5;  // yards from device to tee
}
```

Not required for shot detection. Provides environmental context for the
device's internal flight model.

---

## 13. Leveling

```protobuf
message Orientation {
  optional float roll_deg  = 1;  // degrees
  optional float pitch_deg = 2;  // degrees
}

message LevelCheck {
  enum Verdict {
    VERDICT_LEVEL           = 1;
    VERDICT_RELEVEL_ADVISED = 2;
    VERDICT_RELEVEL_NEEDED  = 3;
  }
  enum Attempt {
    ATTEMPT_OK           = 0;
    ATTEMPT_FAILED       = 1;
    ATTEMPT_DEVICE_MOVED = 2;
  }
  optional Verdict verdict = 1;
  optional Attempt attempt = 2;
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
| Shot delivery | Push (alert notification) | Push (D4/ED/EF burst) |
| Handshake | MultiLink + 5024 + 5050 (~1s) | DSP + AVR + PI sync (~1.2s) |
| State machine | idle → armed → capturing → analyzing | IDLE → ARMED → tracking |
| Re-arm | Automatic (returns to armed) | Manual (0xB0 [01 01] after IDLE) |
| Encryption | None (bonded devices) | None |
| Keepalive | BLE supervision timeout (OS) | Status poll every ~1s |

---

## 15. Identity in Raw Captures

Some messages carry the device's identity or keys. A raw capture meant to
leave the machine (a bug report, a shot export) must not contain them.
`gfdi::carries_identity` and `capture::IdentityFilter` keep them out.

| Messages | Why they are excluded |
|---|---|
| Device Information (5024) | Unit ID, product, Bluetooth friendly and device names |
| FIT Definition / FIT Data (5011, 5012) | FIT files carry the device serial |
| File Transfer / File Data (5003, 5004) | File contents, FIT files included |
| Authentication (5101-5112) | Pairing material and session keys |
| ACK (5000) of any of the above | Dropped with the exchange it answers; a reply's response payload belongs to that exchange |
| ACK too short to name the type it acknowledges | Cannot be classified |

`IdentityFilter` works on BLE chunks, not frames, because that is what a
capture records. Classification is by GFDI message type only; protobuf
payloads are not inspected.

- Every chunk starts with the MultiLink handle byte; the rest is the
  `0x00`-delimited COBS stream (Section 3).
- A chunk that leaves no frame open is released at once. A chunk that leaves a
  frame open is held until the frame's closing `0x00` arrives.
- When the frame is complete it is COBS-decoded, CRC-checked and classified.
  If it is safe, the held chunks are released in order. If it carries
  identity, every chunk holding any of its bytes is dropped, including the
  chunk carrying its closing delimiter.
- A frame that cannot be classified is dropped the same way: bad COBS, under
  6 bytes, a failed CRC, more than 8 KiB of COBS bytes, or still open after
  256 chunks have been held for it. Past either bound the filter drops
  every chunk until the next `0x00`, then starts afresh.

A whole chunk is the unit, so a safe frame sharing a chunk with a dropped one
loses that chunk. Use one filter per direction.
