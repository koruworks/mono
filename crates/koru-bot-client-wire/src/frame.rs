// SPDX-License-Identifier: MPL-2.0

//! The frame: a 4-byte big-endian length, then one UTF-8 JSON object.
//!
//! The same framing replication uses, with this protocol's own cap of
//! [`MAX_FRAME_BYTES`]. A longer frame is a protocol error: the reader stops
//! at the length, before it buffers the body, and the stream is reset.
//!
//! The codec performs no I/O. A carrier hands whatever bytes arrived to
//! [`FrameDecoder::push`] and takes whole messages from
//! [`FrameDecoder::next`].

use std::fmt;

use serde::Deserialize;

use crate::message::WireMessage;

/// The largest frame body, in bytes.
pub const MAX_FRAME_BYTES: usize = 1_048_576;
/// The length prefix.
pub const FRAME_HEADER_BYTES: usize = 4;

/// Why bytes could not be written or read as a frame. Every case is a
/// protocol error on the stream. No case carries the bytes or a parser's
/// description of them: a parser quotes what it read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The body is, or is announced as, longer than [`MAX_FRAME_BYTES`].
    FrameTooLarge { bytes: usize },
    /// A frame of length zero.
    EmptyFrame,
    /// Not one UTF-8 JSON object of a known message's shape.
    Malformed,
    /// A well-formed object whose `type` this build does not know.
    UnknownType,
    /// A value this build reads and never writes (an unknown command kind).
    Unsendable,
    /// The decoder already failed; the stream must be reset.
    Poisoned,
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { bytes } => write!(
                formatter,
                "a Bot client frame of {bytes} bytes is over the limit of {MAX_FRAME_BYTES}"
            ),
            Self::EmptyFrame => formatter.write_str("an empty Bot client frame"),
            Self::Malformed => formatter.write_str("a malformed Bot client frame"),
            Self::UnknownType => formatter.write_str("a Bot client message of an unknown type"),
            Self::Unsendable => formatter.write_str("a Bot client message this build never sends"),
            Self::Poisoned => formatter.write_str("the Bot client stream already failed"),
        }
    }
}

impl std::error::Error for WireError {}

/// The JSON body of `message`, without the length prefix.
pub fn encode_body(message: &WireMessage) -> Result<Vec<u8>, WireError> {
    if !message.sendable() {
        return Err(WireError::Unsendable);
    }
    let body = serde_json::to_vec(message).map_err(|_| WireError::Malformed)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge { bytes: body.len() });
    }
    Ok(body)
}

/// `message` as one frame: the length, then the body.
pub fn encode(message: &WireMessage) -> Result<Vec<u8>, WireError> {
    let body = encode_body(message)?;
    let mut framed = Vec::with_capacity(FRAME_HEADER_BYTES + body.len());
    framed.extend_from_slice(&(body.len() as u32).to_be_bytes());
    framed.extend_from_slice(&body);
    Ok(framed)
}

/// Read one frame body.
pub fn decode_body(body: &[u8]) -> Result<WireMessage, WireError> {
    if body.is_empty() {
        return Err(WireError::EmptyFrame);
    }
    if body.len() > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge { bytes: body.len() });
    }
    match serde_json::from_slice::<WireMessage>(body) {
        Ok(message) => Ok(message),
        Err(_) => {
            #[derive(Deserialize)]
            struct Probe {
                r#type: String,
            }
            match serde_json::from_slice::<Probe>(body) {
                Ok(probe) if !WireMessage::knows_type(&probe.r#type) => Err(WireError::UnknownType),
                _ => Err(WireError::Malformed),
            }
        }
    }
}

/// Turns the bytes a carrier delivers, in whatever pieces, into messages.
///
/// After any error the decoder answers [`WireError::Poisoned`]: the stream
/// has no frame boundary any more and must be reset.
#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    failed: bool,
}

impl fmt::Debug for FrameDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrameDecoder")
            .field("buffered", &self.buffer.len())
            .field("failed", &self.failed)
            .finish()
    }
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes received and not yet returned as a message.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Add received bytes. Fails as soon as a length over the cap is seen,
    /// so an oversized frame is never buffered.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), WireError> {
        if self.failed {
            return Err(WireError::Poisoned);
        }
        self.buffer.extend_from_slice(bytes);
        self.announced().map(|_| ())
    }

    fn announced(&mut self) -> Result<Option<usize>, WireError> {
        let Some(header) = self.buffer.first_chunk::<FRAME_HEADER_BYTES>() else {
            return Ok(None);
        };
        let length = u32::from_be_bytes(*header) as usize;
        if length == 0 {
            self.failed = true;
            return Err(WireError::EmptyFrame);
        }
        if length > MAX_FRAME_BYTES {
            self.failed = true;
            return Err(WireError::FrameTooLarge { bytes: length });
        }
        Ok(Some(length))
    }

    /// The next whole frame body, if one has arrived.
    pub fn next_body(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        if self.failed {
            return Err(WireError::Poisoned);
        }
        let Some(length) = self.announced()? else {
            return Ok(None);
        };
        let end = FRAME_HEADER_BYTES + length;
        if self.buffer.len() < end {
            return Ok(None);
        }
        let body = self.buffer[FRAME_HEADER_BYTES..end].to_vec();
        self.buffer.drain(..end);
        Ok(Some(body))
    }

    /// The next whole message, if one has arrived.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<WireMessage>, WireError> {
        let Some(body) = self.next_body()? else {
            return Ok(None);
        };
        match decode_body(&body) {
            Ok(message) => Ok(Some(message)),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
}
