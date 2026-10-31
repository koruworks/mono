// SPDX-License-Identifier: MPL-2.0

use std::io::Read;
use std::path::Path;

use base64::Engine;
use koru_session_protocol::AttachmentKind;

use super::*;

/// Disk access belongs to the provider worker. This seam lets every adapter
/// test supply bytes without opening a file or a store.
pub(super) trait ImageReader: Send {
    fn read(&self, path: &Path, maximum: usize) -> Result<Vec<u8>, ProviderFailure>;
}

pub(super) struct FileImageReader;

impl ImageReader for FileImageReader {
    fn read(&self, path: &Path, maximum: usize) -> Result<Vec<u8>, ProviderFailure> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(maximum as u64 + 1).read_to_end(&mut bytes))
            .map_err(|error| ProviderFailure::io(ProviderStage::Submit, &error))?;
        if bytes.is_empty() || bytes.len() > maximum {
            return Err(failure(
                ErrorCode::InputTooLarge,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        Ok(bytes)
    }
}

impl AcpControl {
    pub(super) fn prompt(&self, input: &AgentInput) -> Result<Vec<Value>, ProviderFailure> {
        let mut prompt = Vec::new();
        if !input.text.is_empty() {
            prompt.push(json!({"type":"text","text":input.text}));
        }
        for attachment in &input.attachments {
            if attachment.kind != AttachmentKind::Image
                || !attachment.media_type.starts_with("image/")
                || !self.capabilities.as_ref().is_some_and(|caps| {
                    caps.capabilities
                        .supports(capability::NATIVE_ATTACHMENT_DELIVERY)
                })
            {
                return Err(failure(
                    ErrorCode::UnsupportedMedia,
                    ProviderStage::Submit,
                    ProviderDiagnostic::InvalidInput,
                ));
            }
            let path = self
                .config
                .attachment_paths
                .resolve(
                    self.session_id
                        .ok_or_else(|| invalid_state(ProviderStage::Submit))?,
                    attachment.id,
                )
                .map_err(|code| {
                    failure(
                        code,
                        ProviderStage::Submit,
                        ProviderDiagnostic::InvalidInput,
                    )
                })?;
            if !path.is_absolute() {
                return Err(failure(
                    ErrorCode::DeliveryFailed,
                    ProviderStage::Submit,
                    ProviderDiagnostic::InvalidInput,
                ));
            }
            let bytes = self
                .image_reader
                .read(&path, self.config.maximum_prompt_frame_bytes / 4 * 3)?;
            prompt.push(json!({"type":"image","mimeType":attachment.media_type,
                "data":base64::engine::general_purpose::STANDARD.encode(bytes)}));
        }
        // Include the envelope in the bound, so combined images cannot create
        // an oversized accepted request and an uncertain submit outcome.
        let frame = json!({"jsonrpc":"2.0","id":self.next_request_id,"method":"session/prompt",
            "params":{"sessionId":self.native_session,"prompt":prompt}});
        let length = serde_json::to_vec(&frame).map_err(|_| malformed())?.len() + 1;
        if length > self.config.maximum_prompt_frame_bytes {
            return Err(failure(
                ErrorCode::InputTooLarge,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        Ok(prompt)
    }
}

pub(super) fn fingerprint(input: &AgentInput) -> blake3::Hash {
    let mut hash = blake3::Hasher::new();
    hash.update(b"koru-acp-input-v1\0");
    hash.update(&(input.text.len() as u64).to_le_bytes());
    hash.update(input.text.as_bytes());
    for attachment in &input.attachments {
        hash.update(attachment.id.as_uuid().as_bytes());
        hash.update(&[match attachment.kind {
            AttachmentKind::Image => 1,
            AttachmentKind::File => 2,
        }]);
        hash.update(&(attachment.media_type.len() as u64).to_le_bytes());
        hash.update(attachment.media_type.as_bytes());
    }
    hash.finalize()
}
