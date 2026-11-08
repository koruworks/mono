// SPDX-License-Identifier: MPL-2.0

use std::fmt;
use std::io::{BufRead, Write};

use koru_session_protocol::ErrorCode;
use serde::Serialize;
use serde_json::Value;

use super::{ProviderDiagnostic, ProviderFailure, ProviderStage};

/// The most one frame an agent sends Koru may carry. It protects Koru from
/// a misbehaving agent, so it stays small whatever Koru sends.
pub const DEFAULT_PROVIDER_FRAME_BYTES: usize = 1024 * 1024;

/// The most one frame Koru sends an ACP agent may carry: a prompt with its
/// pictures inline as base64. Ten 1 MiB sent copies and the text fit, under
/// the 32 MB a vendor takes (chat-attachments D6, ADR-480).
pub const DEFAULT_PROVIDER_PROMPT_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// One private provider JSON value. Debug output exposes only its encoded size.
pub struct ProviderFrame {
    value: Value,
    byte_count: usize,
}

impl ProviderFrame {
    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    pub fn byte_count(&self) -> usize {
        self.byte_count
    }
}

impl fmt::Debug for ProviderFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderFrame")
            .field("byte_count", &self.byte_count)
            .finish()
    }
}

/// Incremental bounded JSONL reader. It never waits for a complete frame in an
/// unbounded allocation, so provider fragmentation and missing newlines cannot
/// grow memory without limit.
pub struct JsonLineReader<R> {
    reader: R,
    pending: Vec<u8>,
    maximum_frame_bytes: usize,
}

impl<R: BufRead> JsonLineReader<R> {
    pub fn new(reader: R, maximum_frame_bytes: usize) -> Result<Self, ProviderFailure> {
        if maximum_frame_bytes == 0 {
            return Err(failure(ProviderDiagnostic::InvalidInput));
        }
        Ok(Self {
            reader,
            pending: Vec::with_capacity(maximum_frame_bytes.min(8 * 1024)),
            maximum_frame_bytes,
        })
    }

    pub fn next_frame(&mut self) -> Result<Option<ProviderFrame>, ProviderFailure> {
        loop {
            let available = self
                .reader
                .fill_buf()
                .map_err(|error| ProviderFailure::io(ProviderStage::Read, &error))?;
            if available.is_empty() {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    self.finish_frame()
                };
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |index| index + 1);
            let content = &available[..take];
            let content_without_cr = if newline.is_some() {
                let content_without_newline = content.strip_suffix(b"\n").unwrap_or(content);
                content_without_newline
                    .strip_suffix(b"\r")
                    .unwrap_or(content_without_newline)
            } else {
                content
            };
            if self.pending.len() + content_without_cr.len() > self.maximum_frame_bytes {
                self.reader.consume(take);
                self.pending.clear();
                if newline.is_none() {
                    self.discard_oversized_tail()?;
                }
                return Err(failure(ProviderDiagnostic::FrameTooLarge));
            }
            self.pending.extend_from_slice(content_without_cr);
            self.reader.consume(take);
            if newline.is_some() {
                if self.pending.is_empty() {
                    continue;
                }
                return self.finish_frame();
            }
        }
    }

    fn finish_frame(&mut self) -> Result<Option<ProviderFrame>, ProviderFailure> {
        let byte_count = self.pending.len();
        let value = serde_json::from_slice(&self.pending);
        self.pending.clear();
        value
            .map(|value| Some(ProviderFrame { value, byte_count }))
            .map_err(|_| failure(ProviderDiagnostic::MalformedFrame))
    }

    fn discard_oversized_tail(&mut self) -> Result<(), ProviderFailure> {
        loop {
            let available = self
                .reader
                .fill_buf()
                .map_err(|error| ProviderFailure::io(ProviderStage::Read, &error))?;
            if available.is_empty() {
                return Ok(());
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |index| index + 1);
            self.reader.consume(take);
            if newline.is_some() {
                return Ok(());
            }
        }
    }
}

/// Writes one JSONL message without ever formatting it into a diagnostic.
pub fn write_json_line(
    writer: &mut impl Write,
    message: &impl Serialize,
    maximum_frame_bytes: usize,
) -> Result<usize, ProviderFailure> {
    let bytes =
        serde_json::to_vec(message).map_err(|_| failure(ProviderDiagnostic::InvalidInput))?;
    if bytes.len() > maximum_frame_bytes {
        return Err(failure(ProviderDiagnostic::FrameTooLarge));
    }
    writer
        .write_all(&bytes)
        .and_then(|()| writer.write_all(b"\n"))
        .map_err(|error| ProviderFailure::io(ProviderStage::Write, &error))?;
    Ok(bytes.len() + 1)
}

fn failure(diagnostic: ProviderDiagnostic) -> ProviderFailure {
    ProviderFailure::new(
        ErrorCode::ProviderIncompatible,
        ProviderStage::Read,
        diagnostic,
    )
}
