// SPDX-License-Identifier: MPL-2.0

//! Decode workspace pictures for the existing viewer, on the open-file worker.
use koru_ui::media_viewer::ViewerImage;

pub(super) fn decode(bytes: &[u8]) -> Result<Option<ViewerImage>, String> {
    // Sniff bytes rather than trusting an extension. Other kinds of files keep
    // their system association; an identified but broken picture reports failure.
    if image::guess_format(bytes).is_err() {
        return Ok(None);
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits.max_image_width = Some(32_768);
    limits.max_image_height = Some(32_768);
    ViewerImage::decode(bytes, limits)
        .map(Some)
        .ok_or_else(|| "The picture could not be decoded within the viewer's limits.".into())
}

#[cfg(test)]
mod tests;
