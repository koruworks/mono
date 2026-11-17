// SPDX-License-Identifier: MPL-2.0

//! The transport: whole frames in, whole frames out.
//!
//! A **frame** is one message as bytes. Over the pipe a frame is followed by
//! one zero byte (assumption 2, "verify at P7.1"). A message is JSON, and
//! JSON never holds a raw zero byte, so the separator cannot occur inside
//! one.
//!
//! [`Transport`] is the one trait both transports implement: the pipe here,
//! and the loopback fallback if the pipe fails its qualification. Nothing
//! above it knows which is in use.
//!
//! A frame is never logged and never kept by this module. A failure is a
//! token and, for an oversize frame, a length.

use std::io::{ErrorKind, Read, Write};

/// The largest frame accepted in either direction. Provisional: the largest
/// reply Koru asks for is one page's text, which the extraction script
/// bounds well below this.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

const SEPARATOR: u8 = 0;
const READ_CHUNK: usize = 16 * 1024;

/// Why a frame could not be moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailure {
    /// The other end is gone. A frame cut short by it is dropped.
    Closed,
    /// A frame over [`MAX_FRAME_BYTES`]. On receive it was skipped whole, so
    /// the next frame is read from its start; `length` is what it held.
    Oversize { length: usize },
    /// A frame to send held the separator.
    Unframeable,
    /// Any other failure, by its kind only.
    Io(ErrorKind),
}

/// Where frames come from. Owned by the one reader thread.
pub trait FrameSource: Send {
    /// The next whole frame, without its separator. Blocks until one is
    /// complete or the transport fails.
    fn receive(&mut self) -> Result<Vec<u8>, TransportFailure>;
}

/// Where frames go. One frame is written whole or not at all.
pub trait FrameSink: Send {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportFailure>;
}

/// A transport is its two halves, each usable from its own thread.
pub trait Transport {
    fn split(self: Box<Self>) -> (Box<dyn FrameSource>, Box<dyn FrameSink>);
}

/// The pipe transport: frames separated by a zero byte.
pub struct PipeTransport<R, W> {
    source: PipeSource<R>,
    sink: PipeSink<W>,
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> PipeTransport<R, W> {
    /// `from_browser` is the end replies and events arrive on, `to_browser`
    /// the end commands are written to.
    pub fn new(from_browser: R, to_browser: W) -> Self {
        Self {
            source: PipeSource::new(from_browser),
            sink: PipeSink::new(to_browser),
        }
    }
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> Transport for PipeTransport<R, W> {
    fn split(self: Box<Self>) -> (Box<dyn FrameSource>, Box<dyn FrameSink>) {
        (Box::new(self.source), Box::new(self.sink))
    }
}

/// The reading half. Frames may arrive split across reads, or several in
/// one read.
pub struct PipeSource<R> {
    reader: R,
    /// Bytes read and not yet returned. Never more than one frame's worth
    /// over the bound.
    pending: Vec<u8>,
    /// How much of `pending` has been searched for a separator.
    searched: usize,
    /// Bytes of an oversize frame already thrown away.
    skipped: usize,
    closed: bool,
}

impl<R: Read> PipeSource<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            searched: 0,
            skipped: 0,
            closed: false,
        }
    }

    fn take_frame(&mut self) -> Option<Result<Vec<u8>, TransportFailure>> {
        let found = self.pending[self.searched..]
            .iter()
            .position(|byte| *byte == SEPARATOR)
            .map(|at| self.searched + at);
        let Some(end) = found else {
            self.searched = self.pending.len();
            if self.skipped > 0 || self.pending.len() > MAX_FRAME_BYTES {
                // Too long already: keep none of it, count all of it.
                self.skipped += self.pending.len();
                self.pending.clear();
                self.searched = 0;
            }
            return None;
        };
        let rest = self.pending.split_off(end + 1);
        let mut frame = std::mem::replace(&mut self.pending, rest);
        frame.pop();
        self.searched = 0;
        let length = self.skipped + frame.len();
        if self.skipped > 0 || length > MAX_FRAME_BYTES {
            self.skipped = 0;
            return Some(Err(TransportFailure::Oversize { length }));
        }
        Some(Ok(frame))
    }
}

impl<R: Read + Send> FrameSource for PipeSource<R> {
    fn receive(&mut self) -> Result<Vec<u8>, TransportFailure> {
        loop {
            if let Some(frame) = self.take_frame() {
                return frame;
            }
            if self.closed {
                return Err(TransportFailure::Closed);
            }
            let start = self.pending.len();
            self.pending.resize(start + READ_CHUNK, 0);
            let read = loop {
                match self.reader.read(&mut self.pending[start..]) {
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    other => break other,
                }
            };
            match read {
                Ok(0) => {
                    self.closed = true;
                    self.pending.clear();
                    self.searched = 0;
                    return Err(TransportFailure::Closed);
                }
                Ok(count) => self.pending.truncate(start + count),
                Err(error) => {
                    self.pending.truncate(start);
                    return Err(match error.kind() {
                        // What a pipe reports when its writer is gone.
                        ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof => {
                            self.closed = true;
                            self.pending.clear();
                            self.searched = 0;
                            TransportFailure::Closed
                        }
                        kind => TransportFailure::Io(kind),
                    });
                }
            }
        }
    }
}

/// The writing half.
pub struct PipeSink<W> {
    writer: W,
    closed: bool,
}

impl<W: Write> PipeSink<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            closed: false,
        }
    }
}

impl<W: Write + Send> FrameSink for PipeSink<W> {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportFailure> {
        if self.closed {
            return Err(TransportFailure::Closed);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(TransportFailure::Oversize {
                length: frame.len(),
            });
        }
        if frame.contains(&SEPARATOR) {
            return Err(TransportFailure::Unframeable);
        }
        // One buffer, one write: a frame and its separator are never
        // interleaved with another thread's.
        let mut framed = Vec::with_capacity(frame.len() + 1);
        framed.extend_from_slice(frame);
        framed.push(SEPARATOR);
        let written = self
            .writer
            .write_all(&framed)
            .and_then(|()| self.writer.flush());
        written.map_err(|error| match error.kind() {
            ErrorKind::BrokenPipe | ErrorKind::WriteZero | ErrorKind::UnexpectedEof => {
                self.closed = true;
                TransportFailure::Closed
            }
            kind => TransportFailure::Io(kind),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// One end of an in-memory duplex: reads come back in the chunks they
    /// were scripted in, and writes are kept.
    #[derive(Clone, Default)]
    struct Duplex {
        incoming: Arc<Mutex<VecDeque<Result<Vec<u8>, ErrorKind>>>>,
        outgoing: Arc<Mutex<Vec<u8>>>,
        writes: Arc<Mutex<usize>>,
        refuse_writes: Arc<Mutex<Option<ErrorKind>>>,
    }

    impl Duplex {
        fn feeding(chunks: &[&[u8]]) -> Self {
            let duplex = Self::default();
            for chunk in chunks {
                duplex
                    .incoming
                    .lock()
                    .unwrap()
                    .push_back(Ok(chunk.to_vec()));
            }
            duplex
        }

        fn then_fail(self, kind: ErrorKind) -> Self {
            self.incoming.lock().unwrap().push_back(Err(kind));
            self
        }
    }

    impl Read for Duplex {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let mut incoming = self.incoming.lock().unwrap();
            let Some(next) = incoming.pop_front() else {
                // Nothing more scripted: the other end closed.
                return Ok(0);
            };
            let mut chunk = next.map_err(std::io::Error::from)?;
            let count = chunk.len().min(buffer.len());
            buffer[..count].copy_from_slice(&chunk[..count]);
            if count < chunk.len() {
                incoming.push_front(Ok(chunk.split_off(count)));
            }
            Ok(count)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(kind) = *self.refuse_writes.lock().unwrap() {
                return Err(kind.into());
            }
            *self.writes.lock().unwrap() += 1;
            self.outgoing.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn source(chunks: &[&[u8]]) -> PipeSource<Duplex> {
        PipeSource::new(Duplex::feeding(chunks))
    }

    #[test]
    fn m7_transport_framing_frames_split_and_joined_across_reads() {
        // Three frames in one read.
        let mut joined = source(&[b"{\"id\":1}\0{\"id\":2}\0{}\0"]);
        assert_eq!(joined.receive().unwrap(), b"{\"id\":1}");
        assert_eq!(joined.receive().unwrap(), b"{\"id\":2}");
        assert_eq!(joined.receive().unwrap(), b"{}");
        assert_eq!(joined.receive(), Err(TransportFailure::Closed));
        // One frame over many reads, the separator alone in the last.
        let mut split = source(&[b"{\"i", b"d\"", b":", b"1}", b"\0"]);
        assert_eq!(split.receive().unwrap(), b"{\"id\":1}");
        // A read that ends one frame and starts the next.
        let mut straddling = source(&[b"{\"a\":1", b"}\0{\"b\"", b":2}\0"]);
        assert_eq!(straddling.receive().unwrap(), b"{\"a\":1}");
        assert_eq!(straddling.receive().unwrap(), b"{\"b\":2}");
        // Byte by byte.
        let bytes = b"{\"id\":7}\0[]\0";
        let single: Vec<&[u8]> = bytes.chunks(1).collect();
        let mut trickle = source(&single);
        assert_eq!(trickle.receive().unwrap(), b"{\"id\":7}");
        assert_eq!(trickle.receive().unwrap(), b"[]");
        // An empty frame is a frame.
        let mut empty = source(&[b"\0\0{}\0"]);
        assert_eq!(empty.receive().unwrap(), b"");
        assert_eq!(empty.receive().unwrap(), b"");
        assert_eq!(empty.receive().unwrap(), b"{}");
        // A frame longer than one read's buffer.
        let long = vec![b'a'; READ_CHUNK * 3 + 5];
        let mut framed = long.clone();
        framed.push(0);
        let mut large = source(&[&framed]);
        assert_eq!(large.receive().unwrap(), long);
    }

    #[test]
    fn m7_transport_framing_a_closed_pipe() {
        // Closed at once.
        assert_eq!(source(&[]).receive(), Err(TransportFailure::Closed));
        // Closed in the middle of a frame: the part is dropped, not returned.
        let mut cut = source(&[b"{\"id\":1}\0{\"id\":"]);
        assert_eq!(cut.receive().unwrap(), b"{\"id\":1}");
        assert_eq!(cut.receive(), Err(TransportFailure::Closed));
        // And it stays closed.
        assert_eq!(cut.receive(), Err(TransportFailure::Closed));
        // A broken pipe is the same event as an end of file.
        let mut broken =
            PipeSource::new(Duplex::feeding(&[b"{}\0{"]).then_fail(ErrorKind::BrokenPipe));
        assert_eq!(broken.receive().unwrap(), b"{}");
        assert_eq!(broken.receive(), Err(TransportFailure::Closed));
        // Any other failure is reported by its kind, and reading can go on.
        let failing = Duplex::feeding(&[b"{"]).then_fail(ErrorKind::PermissionDenied);
        failing
            .incoming
            .lock()
            .unwrap()
            .push_back(Ok(b"}\0".to_vec()));
        let mut failing = PipeSource::new(failing);
        assert_eq!(
            failing.receive(),
            Err(TransportFailure::Io(ErrorKind::PermissionDenied))
        );
        assert_eq!(failing.receive().unwrap(), b"{}");

        // Sending to a closed pipe.
        let duplex = Duplex::default();
        *duplex.refuse_writes.lock().unwrap() = Some(ErrorKind::BrokenPipe);
        let mut sink = PipeSink::new(duplex.clone());
        assert_eq!(sink.send(b"{}"), Err(TransportFailure::Closed));
        *duplex.refuse_writes.lock().unwrap() = None;
        assert_eq!(sink.send(b"{}"), Err(TransportFailure::Closed));
        assert!(duplex.outgoing.lock().unwrap().is_empty());
    }

    #[test]
    fn m7_transport_framing_oversize_frames() {
        // Exactly at the bound is a frame.
        let mut at = vec![b'a'; MAX_FRAME_BYTES];
        at.push(0);
        assert_eq!(source(&[&at]).receive().unwrap().len(), MAX_FRAME_BYTES);
        // One byte over is refused with its length, and the next frame is
        // read from its start.
        let mut over = vec![b'a'; MAX_FRAME_BYTES + 1];
        over.extend_from_slice(b"\0{\"id\":2}\0");
        let mut reader = source(&[&over]);
        assert_eq!(
            reader.receive(),
            Err(TransportFailure::Oversize {
                length: MAX_FRAME_BYTES + 1
            })
        );
        assert_eq!(reader.receive().unwrap(), b"{\"id\":2}");
        // Far over, arriving in pieces: skipped without being kept.
        let piece = vec![b'b'; 3 * 1024 * 1024];
        let chunks: Vec<&[u8]> = vec![&piece, &piece, &piece, &piece, b"tail\0", b"{}\0"];
        let mut reader = source(&chunks);
        assert_eq!(
            reader.receive(),
            Err(TransportFailure::Oversize {
                length: 4 * piece.len() + 4
            })
        );
        assert!(reader.pending.is_empty());
        assert_eq!(reader.receive().unwrap(), b"{}");
        // An oversize frame that never ends is a closed pipe, not a frame.
        let mut endless = source(&[&piece, &piece, &piece]);
        assert_eq!(endless.receive(), Err(TransportFailure::Closed));

        // Sending: over the bound, or holding the separator, writes nothing.
        let duplex = Duplex::default();
        let mut sink = PipeSink::new(duplex.clone());
        assert_eq!(
            sink.send(&vec![b'a'; MAX_FRAME_BYTES + 1]),
            Err(TransportFailure::Oversize {
                length: MAX_FRAME_BYTES + 1
            })
        );
        assert_eq!(
            sink.send(b"{\"a\":\"\0\"}"),
            Err(TransportFailure::Unframeable)
        );
        assert!(duplex.outgoing.lock().unwrap().is_empty());
    }

    #[test]
    fn m7_transport_framing_a_sent_frame_is_one_write_with_its_separator() {
        let duplex = Duplex::default();
        let transport: Box<dyn Transport> = Box::new(PipeTransport::new(
            Duplex::feeding(&[b"{\"id\":1,\"result\":{}}\0"]),
            duplex.clone(),
        ));
        let (mut from, mut to) = transport.split();
        to.send(b"{\"id\":1,\"method\":\"a\"}").unwrap();
        to.send(b"{\"id\":2,\"method\":\"b\"}").unwrap();
        assert_eq!(
            *duplex.outgoing.lock().unwrap(),
            b"{\"id\":1,\"method\":\"a\"}\0{\"id\":2,\"method\":\"b\"}\0"
        );
        assert_eq!(*duplex.writes.lock().unwrap(), 2);
        assert_eq!(from.receive().unwrap(), b"{\"id\":1,\"result\":{}}");

        // What one side sends the other receives, frame for frame.
        let wire = Duplex::default();
        let mut sink = PipeSink::new(wire.clone());
        let frames: [&[u8]; 3] = [b"{\"a\":1}", b"", b"{\"text\":\"caf\\u00e9 \\u0000\"}"];
        for frame in frames {
            sink.send(frame).unwrap();
        }
        let sent = wire.outgoing.lock().unwrap().clone();
        let mut back = source(&[&sent]);
        for frame in frames {
            assert_eq!(back.receive().unwrap(), frame);
        }
        // The two halves are each `Send`, for the reader thread.
        fn sendable<T: Send>(_: &T) {}
        sendable(&from);
        sendable(&to);
    }

    #[test]
    fn m7_transport_framing_a_failure_is_a_token_and_a_length() {
        let shown: Vec<String> = [
            TransportFailure::Closed,
            TransportFailure::Oversize { length: 9 },
            TransportFailure::Unframeable,
            TransportFailure::Io(ErrorKind::TimedOut),
        ]
        .iter()
        .map(|failure| format!("{failure:?}"))
        .collect();
        assert_eq!(
            shown,
            [
                "Closed",
                "Oversize { length: 9 }",
                "Unframeable",
                "Io(TimedOut)"
            ]
        );
    }
}
