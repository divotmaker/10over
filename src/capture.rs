//! Identity filtering for raw traffic captures.
//!
//! A tool that records the raw BLE chunks crossing a [`Transport`] (for a
//! bug report, a shot export or a replay) must not record the device's
//! identity or keys. [`IdentityFilter`] sits between the transport and the
//! recorder and passes on only the chunks that are safe to keep.
//!
//! [`Transport`]: crate::Transport

use crate::gfdi;

/// Longest COBS-encoded GFDI frame the filter classifies. A longer frame is
/// treated as unclassifiable and dropped.
pub const MAX_FRAME_BYTES: usize = 8 * 1024;

/// Most chunks held for one frame. A frame still open after this many chunks
/// have been held for it is treated as unclassifiable and dropped.
pub const MAX_HELD_CHUNKS: usize = 256;

/// Filters one direction of raw GFDI traffic, chunk by chunk, so that no
/// chunk holding any byte of an identity-bearing frame is released.
///
/// Classification is by GFDI message type only; protobuf payloads are not
/// inspected.
///
/// # Input
///
/// Feed every chunk of one direction (device notifications, or host
/// writes) in the order it crossed the transport, as [`Transport::read`]
/// and [`Transport::write`] carry it: the MultiLink handle byte first, then
/// part of the `0x00`-delimited COBS stream of GFDI frames. A frame may span
/// any number of chunks, and a chunk may carry the end of one frame and the
/// start of the next. Use a separate filter for each direction. Each chunk
/// comes with a caller value of type `T` (the chunk itself, or a record
/// with a timestamp); the filter hands those values back.
///
/// # Output
///
/// [`feed`](Self::feed) returns the values of the chunks cleared for
/// recording, oldest first. A chunk is cleared once every frame it carries
/// bytes of is complete and classified as safe.
///
/// - **Released at once:** a chunk that leaves no frame open (it carries
///   only whole frames, frame ends, or delimiters).
/// - **Held:** a chunk that leaves a frame open. It is released, with every
///   chunk held for the same frame, when the closing `0x00` arrives and the
///   frame is safe.
/// - **Dropped:** every chunk holding a byte of a frame that
///   [`GfdiFrame::carries_identity`](gfdi::GfdiFrame::carries_identity)
///   (device information, FIT, file transfer, authentication, or an ACK of
///   one of them), including the chunk carrying its closing delimiter.
///   Frames that cannot be classified are dropped the same way: bad COBS,
///   shorter than 6 bytes, a failed CRC, longer than [`MAX_FRAME_BYTES`], or
///   still open after [`MAX_HELD_CHUNKS`] chunks have been held for it. Once
///   a frame is past either bound, every further chunk is dropped until the
///   next `0x00`.
///
/// Dropping a chunk drops the whole chunk, so a safe frame that shares a
/// chunk with a dropped one loses that chunk too. Chunks still held when
/// the filter is dropped are never released.
///
/// [`Transport::read`]: crate::Transport::read
/// [`Transport::write`]: crate::Transport::write
///
/// # Example
///
/// ```
/// use tenover::capture::IdentityFilter;
///
/// let mut rx = IdentityFilter::new();
/// let mut recorded: Vec<Vec<u8>> = Vec::new();
/// # let chunks: Vec<Vec<u8>> = Vec::new();
/// for chunk in chunks {
///     recorded.extend(rx.feed(&chunk, chunk.clone()));
/// }
/// ```
#[derive(Debug)]
pub struct IdentityFilter<T> {
    /// COBS bytes of the frame in progress.
    frame: Vec<u8>,
    /// The frame in progress is dropped whatever its type.
    tainted: bool,
    /// Values of the chunks carrying part of the frame in progress.
    held: Vec<T>,
}

impl<T> Default for IdentityFilter<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> IdentityFilter<T> {
    /// A filter at the start of a stream, holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            frame: Vec::new(),
            tainted: false,
            held: Vec::new(),
        }
    }

    /// Feed one chunk, with the value to hand back if it is cleared.
    /// Returns the values of the chunks cleared for recording, oldest
    /// first.
    #[must_use]
    pub fn feed(&mut self, chunk: &[u8], value: T) -> Vec<T> {
        let mut out = Vec::new();
        let mut keep = true;
        for &b in chunk.get(1..).unwrap_or_default() {
            if b != 0 {
                if self.frame.len() < MAX_FRAME_BYTES {
                    self.frame.push(b);
                } else {
                    self.tainted = true;
                }
                continue;
            }
            if self.frame.is_empty() && !self.tainted {
                continue;
            }
            if self.tainted || frame_excluded(&self.frame) {
                self.held.clear();
                keep = false;
            } else {
                out.append(&mut self.held);
            }
            self.frame.clear();
            self.tainted = false;
        }
        if self.held.len() >= MAX_HELD_CHUNKS {
            self.held.clear();
            self.tainted = true;
        }
        if keep && !self.tainted {
            if self.frame.is_empty() {
                out.push(value);
            } else {
                self.held.push(value);
            }
        }
        out
    }
}

/// Whether a COBS-encoded frame stays out of the capture: it carries
/// identity, or it cannot be parsed and so cannot be classified.
fn frame_excluded(cobs_bytes: &[u8]) -> bool {
    gfdi::parse_cobs_frame(cobs_bytes).map_or(true, |f| f.carries_identity())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfdi::{MSG_ACK, MSG_DEVICE_INFO, MSG_PROTOBUF_REQUEST, MSG_PROTOBUF_RESPONSE};
    use crate::testing::{self, DEVICE_INFO_NAME, DEVICE_INFO_UNIT_ID};

    /// MultiLink handle the tests use.
    const HANDLE: u8 = 0x05;

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    /// Feed `chunks` in order; return the chunks released.
    fn run(filter: &mut IdentityFilter<Vec<u8>>, chunks: &[Vec<u8>]) -> Vec<Vec<u8>> {
        chunks
            .iter()
            .flat_map(|c| filter.feed(c, c.clone()))
            .collect()
    }

    #[test]
    fn device_info_is_never_released() {
        // A shot message, a repeated 5024 and another shot message, packed
        // back to back and split at MTU 20 so chunks straddle frames.
        let (shot1, info, shot2) = (
            testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x11; 30]),
            testing::device_info_frame(),
            testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x22; 30]),
        );
        let stream = [shot1.clone(), info.clone(), shot2].concat();
        let rx = testing::chunks(HANDLE, &stream, 20);
        let got = run(&mut IdentityFilter::new(), &rx);

        // Exactly the chunks holding no byte of the 5024 frame (its COBS
        // bytes and closing delimiter) are kept, in order.
        let info_bytes = shot1.len() + 1..shot1.len() + info.len();
        let expected: Vec<Vec<u8>> = rx
            .iter()
            .enumerate()
            .filter(|(k, _)| {
                let (lo, hi) = (k * 19, (k + 1) * 19);
                hi <= info_bytes.start || lo >= info_bytes.end
            })
            .map(|(_, c)| c.clone())
            .collect();
        assert!(expected.len() >= 4 && expected.len() < rx.len());
        assert_eq!(got, expected);

        // Our reply to it is dropped; the ACK of a shot message is kept.
        let reply = testing::ack_frame(MSG_DEVICE_INFO, 0, &[0xAA, 0xBB]);
        let shot_ack =
            testing::with_handle(HANDLE, &testing::ack_frame(MSG_PROTOBUF_RESPONSE, 0, &[]));
        let mut tx = testing::chunks(HANDLE, &reply, 20);
        tx.push(shot_ack.clone());
        let sent = run(&mut IdentityFilter::new(), &tx);
        assert_eq!(sent, [shot_ack], "ACK of 5024 dropped");

        // The unit ID never leaks, nor the device's name.
        let all: String = got.iter().chain(&sent).map(|c| hex(c)).collect();
        assert!(
            !all.contains(&hex(&DEVICE_INFO_UNIT_ID.to_le_bytes())),
            "unit ID leaked"
        );
        assert!(
            !all.contains(&hex(DEVICE_INFO_NAME.as_bytes())),
            "name leaked"
        );
    }

    #[test]
    fn unit_id_never_leaks_at_any_mtu() {
        let stream = [
            testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"before"),
            testing::device_info_frame(),
            testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"after"),
        ]
        .concat();
        let unit_id = hex(&DEVICE_INFO_UNIT_ID.to_le_bytes());
        for mtu in 2..=stream.len() + 1 {
            let got = run(
                &mut IdentityFilter::new(),
                &testing::chunks(HANDLE, &stream, mtu),
            );
            let all: String = got.iter().map(|c| hex(&c[1..])).collect();
            assert!(!all.contains(&unit_id), "unit ID leaked at MTU {mtu}");
            assert!(
                !all.contains(&hex(DEVICE_INFO_NAME.as_bytes())),
                "name leaked at MTU {mtu}"
            );
        }
    }

    #[test]
    fn unclassifiable_frames_are_excluded() {
        let mut bad = testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"shot");
        let n = bad.len();
        bad[n - 2] ^= 0x01; // corrupt the CRC
        let got = run(
            &mut IdentityFilter::new(),
            &[testing::with_handle(HANDLE, &bad)],
        );
        assert!(got.is_empty());
    }

    #[test]
    fn whole_frames_are_released_at_once() {
        let mut f = IdentityFilter::new();
        let chunk = testing::with_handle(
            HANDLE,
            &testing::encode_frame(MSG_PROTOBUF_REQUEST, &[0x30]),
        );
        assert_eq!(f.feed(&chunk, 1), [1]);
        // A chunk with no frame bytes (handle only, or empty) passes.
        assert_eq!(f.feed(&[HANDLE], 2), [2]);
        assert_eq!(f.feed(&[], 3), [3]);
    }

    #[test]
    fn split_frames_are_held_until_complete() {
        let frame = testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x44; 40]);
        let chunks = testing::chunks(HANDLE, &frame, 20);
        assert!(chunks.len() >= 3);
        let mut f = IdentityFilter::new();
        let last = chunks.len() - 1;
        for (i, c) in chunks[..last].iter().enumerate() {
            assert!(f.feed(c, i).is_empty(), "chunk {i} released early");
        }
        assert_eq!(f.feed(&chunks[last], last), (0..=last).collect::<Vec<_>>());
    }

    #[test]
    fn compressed_header_frames_are_classified() {
        // [length u16] [type - 5000] [txn_id | 0x80] [payload] [crc u16]
        let frame = |t: u16, payload: &[u8]| {
            let mut f = u16::try_from(6 + payload.len())
                .unwrap()
                .to_le_bytes()
                .to_vec();
            f.push(u8::try_from(t - 5000).unwrap());
            f.push(0x81);
            f.extend_from_slice(payload);
            let crc = crate::crc::crc16(&f);
            f.extend_from_slice(&crc.to_le_bytes());
            testing::with_handle(HANDLE, &crate::gfdi::wrap_cobs(&f))
        };
        let mut f = IdentityFilter::new();
        assert_eq!(f.feed(&frame(MSG_PROTOBUF_RESPONSE, b"shot"), 1), [1]);
        assert!(f.feed(&frame(MSG_DEVICE_INFO, b"info"), 2).is_empty());
        let ack_of_info = [MSG_DEVICE_INFO.to_le_bytes().as_slice(), &[0]].concat();
        assert!(f.feed(&frame(MSG_ACK, &ack_of_info), 3).is_empty());
    }

    #[test]
    fn ack_without_acked_type_is_excluded() {
        let mut f = IdentityFilter::new();
        for payload in [&[][..], &[0x2C]] {
            let chunk = testing::with_handle(HANDLE, &testing::encode_frame(MSG_ACK, payload));
            assert!(f.feed(&chunk, ()).is_empty(), "{payload:?}");
        }
    }

    #[test]
    fn oversize_frame_is_dropped_until_the_next_delimiter() {
        let big = testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x55; MAX_FRAME_BYTES]);
        let next = testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"next");
        let stream = [big, next.clone()].concat();
        let chunks = testing::chunks(HANDLE, &stream, 1024);
        let got = run(&mut IdentityFilter::new(), &chunks);
        // Only the chunks wholly inside the following frame survive.
        let last = chunks.last().unwrap();
        assert!(got.iter().all(|c| c == last));
        // The filter recovers for the frame after.
        let mut f = IdentityFilter::new();
        let _ = run(&mut f, &chunks);
        let ok = testing::with_handle(HANDLE, &next);
        assert_eq!(f.feed(&ok, ok.clone()), [ok]);
    }

    #[test]
    fn frame_over_too_many_chunks_is_dropped() {
        // One frame byte per chunk, more chunks than the filter holds.
        let frame = testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x66; MAX_HELD_CHUNKS + 10]);
        let chunks = testing::chunks(HANDLE, &frame, 2);
        let mut f = IdentityFilter::new();
        // Only the first chunk, the leading delimiter, is released.
        assert_eq!(run(&mut f, &chunks), [vec![HANDLE, 0x00]]);
        let ok = testing::with_handle(HANDLE, &testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"ok"));
        assert_eq!(f.feed(&ok, ok.clone()), [ok]);

        // Just under the bound, the same frame is released whole.
        let frame = testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x66; MAX_HELD_CHUNKS - 10]);
        let chunks = testing::chunks(HANDLE, &frame, 2);
        assert!(chunks.len() <= MAX_HELD_CHUNKS);
        assert_eq!(run(&mut IdentityFilter::new(), &chunks), chunks);
    }

    #[test]
    fn garbage_is_dropped() {
        let mut f = IdentityFilter::new();
        // Valid COBS, but too short to be a frame.
        assert!(f.feed(&[HANDLE, 0x00, 0x02, 0x41, 0x00], 1).is_empty());
        // Truncated COBS.
        assert!(f.feed(&[HANDLE, 0x00, 0x09, 0x41, 0x00], 2).is_empty());
        // The filter still releases a good frame afterward.
        let ok = testing::with_handle(HANDLE, &testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"x"));
        assert_eq!(f.feed(&ok, 3), [3]);
    }
}
