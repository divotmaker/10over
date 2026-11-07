//! The driver's raw-frame capture: a [`Transport`] wrapper that splits each
//! direction into whole messages and classifies them.

use std::collections::VecDeque;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use flightrelay::RawDir;

use crate::capture::MAX_FRAME_BYTES;
use crate::client::Transport;
use crate::{cobs, gfdi, multilink};

/// One message as it crossed the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Captured {
    pub dir: RawDir,
    /// `"<channel> <message type, hex>"`, e.g. `"write 13a0"`.
    pub label: String,
    /// A GFDI message: the frame without its COBS encoding and delimiters.
    /// A MultiLink registration: the chunk as sent or received.
    pub data: Vec<u8>,
    /// When its last byte crossed, in ms since the Unix epoch.
    pub at_ms: Option<u64>,
    /// It carries identity or keys, or could not be classified: it may only
    /// be logged locally, never sent.
    pub private: bool,
}

/// Reassembles one direction's `0x00`-delimited COBS stream into frames.
#[derive(Debug, Default)]
struct Splitter {
    /// COBS bytes of the frame in progress.
    frame: Vec<u8>,
    /// The frame in progress outgrew [`MAX_FRAME_BYTES`].
    oversize: bool,
}

impl Splitter {
    /// Feed one chunk (MultiLink handle byte first); push each frame it
    /// completes. With `capture` off it only keeps track of where frames
    /// end, so capturing can start again at the next whole frame.
    fn feed(
        &mut self,
        dir: RawDir,
        channel: &str,
        chunk: &[u8],
        capture: bool,
        out: &mut VecDeque<Captured>,
    ) {
        for &b in chunk.get(1..).unwrap_or_default() {
            if b != 0 {
                if self.frame.len() < MAX_FRAME_BYTES {
                    self.frame.push(b);
                } else {
                    self.oversize = true;
                }
                continue;
            }
            if self.frame.is_empty() && !self.oversize {
                continue;
            }
            if capture {
                out.push_back(self.complete(dir, channel));
            }
            self.frame.clear();
            self.oversize = false;
        }
    }

    fn complete(&self, dir: RawDir, channel: &str) -> Captured {
        let parsed = if self.oversize {
            None
        } else {
            cobs::decode(&self.frame)
                .ok()
                .and_then(|data| gfdi::parse_frame(&data).ok().map(|frame| (frame, data)))
        };
        match parsed {
            Some((frame, data)) => Captured {
                dir,
                label: format!("{channel} {:04x}", frame.msg_type()),
                data,
                at_ms: now_ms(),
                private: frame.carries_identity(),
            },
            None => Captured {
                dir,
                label: format!("{channel} unparsed"),
                data: self.frame.clone(),
                at_ms: now_ms(),
                private: true,
            },
        }
    }
}

/// A [`Transport`] that records every message crossing it into a queue the
/// session drains.
pub(crate) struct Tap<T> {
    inner: T,
    rx: Splitter,
    tx: Splitter,
    /// No registration response has been seen yet; until one has, a
    /// notification that parses as one is not part of the GFDI stream.
    registering: bool,
    /// Messages are captured; off, nothing is decoded or queued.
    enabled: bool,
    queue: VecDeque<Captured>,
}

impl<T> Tap<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self {
            inner,
            rx: Splitter::default(),
            tx: Splitter::default(),
            registering: true,
            enabled: true,
            queue: VecDeque::new(),
        }
    }

    /// Capture messages only while someone can receive them: the host wants
    /// raw frames or shows audit lines.
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Whether messages are captured.
    #[cfg(test)]
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// The wrapped transport.
    pub(crate) fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// The messages captured since the last call, oldest first.
    pub(crate) fn drain(&mut self) -> std::collections::vec_deque::Drain<'_, Captured> {
        self.queue.drain(..)
    }

    fn register(&mut self, dir: RawDir, data: &[u8]) {
        if !self.enabled {
            return;
        }
        self.queue.push_back(Captured {
            dir,
            label: "register".to_owned(),
            data: data.to_vec(),
            at_ms: now_ms(),
            private: false,
        });
    }
}

impl<T: Transport> Transport for Tap<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        let n = self.inner.read(buf)?;
        let chunk = &buf[..n];
        if n == 0 {
            return Ok(0);
        }
        if self.registering
            && let Some((status, _, _)) = multilink::parse_register_response(chunk)
        {
            self.registering = status != multilink::RegisterStatus::Success;
            self.register(RawDir::Rx, chunk);
            return Ok(n);
        }
        self.rx
            .feed(RawDir::Rx, "notify", chunk, self.enabled, &mut self.queue);
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), io::Error> {
        self.inner.write(data)?;
        self.tx
            .feed(RawDir::Tx, "write", data, self.enabled, &mut self.queue);
        Ok(())
    }

    fn write_register(&mut self, data: &[u8]) -> Result<(), io::Error> {
        self.inner.write_register(data)?;
        self.register(RawDir::Tx, data);
        Ok(())
    }
}

/// The current UTC time in milliseconds since the Unix epoch.
fn now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfdi::{MSG_DEVICE_INFO, MSG_PROTOBUF_RESPONSE};
    use crate::testing;

    const HANDLE: u8 = 0x05;

    fn split(chunks: &[Vec<u8>]) -> Vec<Captured> {
        let mut s = Splitter::default();
        let mut out = VecDeque::new();
        for c in chunks {
            s.feed(RawDir::Rx, "notify", c, true, &mut out);
        }
        out.into()
    }

    #[test]
    fn one_capture_per_message_at_any_mtu() {
        let shot = testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x11; 40]);
        let info = testing::device_info_frame();
        let stream = [shot.clone(), info, shot.clone()].concat();
        for mtu in 2..=stream.len() + 1 {
            let got = split(&testing::chunks(HANDLE, &stream, mtu));
            let labels: Vec<_> = got.iter().map(|c| (c.label.as_str(), c.private)).collect();
            assert_eq!(
                labels,
                [
                    ("notify 13b4", false),
                    ("notify 13a0", true),
                    ("notify 13b4", false)
                ],
                "MTU {mtu}"
            );
            let expected = cobs::decode(&shot[1..shot.len() - 1]).unwrap();
            assert_eq!(got[0].data, expected);
            assert_eq!(got[2].data, expected);
        }
    }

    #[test]
    fn bad_and_oversize_frames_are_private() {
        let mut bad = testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"shot");
        let n = bad.len();
        bad[n - 2] ^= 0x01; // corrupt the CRC
        let big = testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x55; MAX_FRAME_BYTES]);
        let ok = testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"ok");
        let stream = [bad, big, ok].concat();
        let got = split(&testing::chunks(HANDLE, &stream, 512));
        let flags: Vec<_> = got.iter().map(|c| (c.label.as_str(), c.private)).collect();
        assert_eq!(
            flags,
            [
                ("notify unparsed", true),
                ("notify unparsed", true),
                ("notify 13b4", false)
            ]
        );
    }

    /// A device that has `chunks` to say.
    struct Chunks(VecDeque<Vec<u8>>);

    impl Transport for Chunks {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
            let c = self.0.pop_front().ok_or(io::ErrorKind::WouldBlock)?;
            buf[..c.len()].copy_from_slice(&c);
            Ok(c.len())
        }

        fn write(&mut self, _data: &[u8]) -> Result<(), io::Error> {
            Ok(())
        }

        fn write_register(&mut self, _data: &[u8]) -> Result<(), io::Error> {
            Ok(())
        }
    }

    #[test]
    fn a_disabled_tap_captures_nothing_and_keeps_message_boundaries() {
        let shot = testing::encode_frame(MSG_PROTOBUF_RESPONSE, &[0x11; 40]);
        let other = testing::encode_frame(MSG_PROTOBUF_RESPONSE, b"ok");
        let shot_chunks = testing::chunks(HANDLE, &shot, 20);
        let n = shot_chunks.len();
        assert!(n > 2, "the shot spans several chunks");
        let stream = [
            shot_chunks.clone(),
            testing::chunks(HANDLE, &other, 20),
            shot_chunks,
        ]
        .concat();
        let mut tap = Tap::new(Chunks(stream.into()));
        tap.registering = false;
        let mut buf = [0u8; 64];

        // Off for a whole message and a registration: nothing is captured.
        tap.set_enabled(false);
        for _ in 0..n {
            tap.read(&mut buf).expect("chunk");
        }
        tap.write_register(&[0x00]).expect("register");
        assert_eq!(tap.drain().count(), 0);

        // On again: the next message is captured whole.
        tap.set_enabled(true);
        while tap.read(&mut buf).is_ok() && tap.queue.is_empty() {}
        let got: Vec<_> = tap.drain().collect();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].label, "notify 13b4");
        assert_eq!(
            got[0].data,
            cobs::decode(&other[1..other.len() - 1]).unwrap()
        );

        // Off during the first chunk of a message, on before its end: it is
        // still captured whole.
        tap.set_enabled(false);
        tap.read(&mut buf).expect("chunk");
        tap.set_enabled(true);
        while tap.read(&mut buf).is_ok() {}
        let got: Vec<_> = tap.drain().collect();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].data, cobs::decode(&shot[1..shot.len() - 1]).unwrap());
    }

    #[test]
    fn device_info_ack_is_private() {
        let mut s = Splitter::default();
        let mut out = VecDeque::new();
        let ack = testing::ack_frame(MSG_DEVICE_INFO, 0, &[0xAA]);
        s.feed(
            RawDir::Tx,
            "write",
            &testing::with_handle(HANDLE, &ack),
            true,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].private);
        assert_eq!(out[0].label, "write 1388");
    }
}
