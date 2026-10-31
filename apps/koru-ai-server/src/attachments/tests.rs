// SPDX-License-Identifier: MPL-2.0

use std::collections::HashSet;

use image::{ImageBuffer, Rgb, Rgba};

use super::*;

/// A picture whose noisy rows (one in eight) do not compress, so its size
/// is real without passing the upload limit.
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    let image = ImageBuffer::from_fn(width, height, |_, y| {
        if y % 8 != 0 {
            return Rgba([200, 200, 200, 255]);
        }
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let [r, g, b, _] = state.to_le_bytes();
        Rgba([r, g, b, 255])
    });
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(image)
        .write_to(Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    bytes
}

fn smooth_jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = ImageBuffer::from_fn(width, height, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    });
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
        .encode_image(&DynamicImage::ImageRgb8(image))
        .unwrap();
    bytes
}

fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut bytes));
        for (name, content) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
    }
    bytes
}

fn store() -> (tempfile::TempDir, AttachmentStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(directory.path());
    (directory, store)
}

fn prepare(store: &AttachmentStore, name: &str, bytes: &[u8]) -> Result<Prepared, Failure> {
    store.prepare(
        ChatId::new(),
        ChatAttachmentId::new(),
        name,
        AttachmentSource::Clipboard,
        bytes,
    )
}

#[test]
fn a_large_screenshot_gets_a_sent_copy_within_the_edge_and_the_bytes() {
    let (_directory, store) = store();
    let original = noise_png(4000, 2000);
    assert!(original.len() as u64 > MAX_SENT_COPY_BYTES);
    let prepared = prepare(&store, "screenshot.png", &original).unwrap();
    let attachment = &prepared.record.attachment;
    assert_eq!(attachment.kind, AttachmentKind::Image);
    assert_eq!(attachment.dimensions, Some((4000, 2000)));
    assert_eq!(attachment.media_type, "image/png");
    assert!(prepared.record.has_sent_copy);
    store.publish(&prepared).unwrap();

    let original_path = store.original(&prepared.record);
    assert_eq!(fs::read(&original_path).unwrap(), original, "byte for byte");
    let sent = native_copy(&original_path);
    assert_ne!(sent, original_path);
    let sent_bytes = fs::read(&sent).unwrap();
    assert!(sent_bytes.len() as u64 <= MAX_SENT_COPY_BYTES);
    let decoded = image::load_from_memory(&sent_bytes).unwrap();
    assert_eq!(decoded.width().max(decoded.height()), SENT_COPY_EDGE);
    let thumbnail = image::load_from_memory(
        &fs::read(store.thumbnail(prepared.record.chat_id, attachment.id)).unwrap(),
    )
    .unwrap();
    assert_eq!(thumbnail.width().max(thumbnail.height()), THUMBNAIL_EDGE);
}

#[test]
fn a_small_photo_goes_as_it_is() {
    let (_directory, store) = store();
    let photo = smooth_jpeg(1200, 800);
    assert!(photo.len() as u64 <= MAX_SENT_COPY_BYTES);
    let prepared = prepare(&store, "photo.jpg", &photo).unwrap();
    assert!(!prepared.record.has_sent_copy);
    assert_eq!(prepared.record.attachment.media_type, "image/jpeg");
    store.publish(&prepared).unwrap();
    let original = store.original(&prepared.record);
    assert_eq!(native_copy(&original), original);
}

#[test]
fn a_picture_beyond_the_preview_limits_is_attached_as_a_file() {
    let (_directory, store) = store();
    let wide = noise_png(MAX_PREVIEW_EDGE + 1, 1);
    let prepared = prepare(&store, "panorama.png", &wide).unwrap();
    assert_eq!(prepared.record.attachment.kind, AttachmentKind::File);
    assert_eq!(prepared.record.attachment.dimensions, None);
    assert!(!prepared.record.has_sent_copy);
}

#[test]
fn renamed_executables_and_mismatched_content_are_refused() {
    let (_directory, store) = store();
    let mut pe = b"MZ\x90\0".to_vec();
    pe.extend_from_slice(&[0; 256]);
    for (name, bytes) in [
        ("photo.png", pe.as_slice()),
        ("notes.txt", pe.as_slice()),
        ("notes.md", b"# heading\0\0\0".as_slice()),
        ("report.pdf", b"not a pdf".as_slice()),
        ("setup.exe", pe.as_slice()),
        ("run.ps1", b"Write-Host hi".as_slice()),
    ] {
        let refused = prepare(&store, name, bytes).unwrap_err();
        assert_eq!(refused.code, attachment_codes::TYPE_NOT_ALLOWED, "{name}");
    }
    let refused = prepare(&store, "setup.exe", &pe).unwrap_err();
    assert_eq!(refused.details["extension"], "exe");
    assert!(prepare(&store, "empty.txt", b"").is_err());
}

#[test]
fn an_office_file_must_hold_what_its_type_promises() {
    let (_directory, store) = store();
    let fake = zip_with(&[("word/document.xml", b"<w/>")]);
    assert_eq!(
        prepare(&store, "letter.docx", &fake).unwrap_err().code,
        attachment_codes::TYPE_NOT_ALLOWED
    );
    let docx = zip_with(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("word/document.xml", b"<w/>"),
    ]);
    assert!(prepare(&store, "letter.docx", &docx).is_ok());
    let epub = zip_with(&[("mimetype", b"application/epub+zip")]);
    assert!(prepare(&store, "book.epub", &epub).is_ok());
    assert!(prepare(&store, "book.odt", &epub).is_err());
    assert!(prepare(&store, "clip.MP4", b"\0\0\0\x20ftypisom\0\0\0\0").is_ok());
}

#[test]
fn an_upload_over_the_limit_is_refused_before_it_is_looked_at() {
    let (_directory, store) = store();
    let huge = vec![b'a'; MAX_ATTACHMENT_BYTES as usize + 1];
    assert_eq!(
        prepare(&store, "big.txt", &huge).unwrap_err().code,
        attachment_codes::TOO_LARGE
    );
}

#[test]
fn a_name_is_sanitized_before_it_names_a_file() {
    let (_directory, store) = store();
    let prepared = prepare(&store, "../../evil<>.txt", b"hello").unwrap();
    assert_eq!(prepared.record.attachment.file_name, "evil.txt");
    store.publish(&prepared).unwrap();
    assert!(store.original(&prepared.record).is_file());
    assert!(
        store.original(&prepared.record).starts_with(
            store.attachment_dir(prepared.record.chat_id, prepared.record.attachment.id)
        )
    );
}

#[test]
fn the_sweep_removes_orphans_and_staging_and_leaves_anything_else() {
    let (directory, store) = store();
    let kept = prepare(&store, "kept.txt", b"kept").unwrap();
    store.publish(&kept).unwrap();
    let orphan = store
        .prepare(
            kept.record.chat_id,
            ChatAttachmentId::new(),
            "orphan.txt",
            AttachmentSource::Drop,
            b"orphan",
        )
        .unwrap();
    store.publish(&orphan).unwrap();
    let gone_chat = prepare(&store, "gone.txt", b"gone").unwrap();
    store.publish(&gone_chat).unwrap();
    let in_flight = prepare(&store, "flight.txt", b"in flight").unwrap();
    let foreign = directory.path().join(ATTACHMENTS_DIR).join("keep-me");
    fs::create_dir_all(&foreign).unwrap();

    let known = KnownAttachmentDirs {
        chats: HashSet::from([kept.record.chat_id]),
        attachments: HashSet::from([kept.record.attachment.id]),
    };
    std::thread::sleep(std::time::Duration::from_millis(20));
    let counts = store.sweep(&known, SystemTime::now());
    assert_eq!(
        counts,
        SweepCounts {
            chats: 1,
            attachments: 1,
            staging: 1,
        }
    );
    assert!(store.original(&kept.record).is_file());
    assert!(!store.original(&orphan.record).exists());
    assert!(!store.chat_dir(gone_chat.record.chat_id).exists());
    assert!(!in_flight.staging.exists());
    assert!(
        foreign.is_dir(),
        "a name that is not an id is never touched"
    );
}

#[test]
fn nothing_newer_than_the_sweep_is_swept() {
    let (_directory, store) = store();
    let started = SystemTime::now() - std::time::Duration::from_secs(60);
    let fresh = prepare(&store, "fresh.txt", b"fresh").unwrap();
    store.publish(&fresh).unwrap();
    let counts = store.sweep(&KnownAttachmentDirs::default(), started);
    assert_eq!(counts, SweepCounts::default());
    assert!(store.original(&fresh.record).is_file());
}

#[test]
fn a_branch_links_every_folder_or_none() {
    let (_directory, store) = store();
    let first = prepare(&store, "first.txt", b"first").unwrap();
    store.publish(&first).unwrap();
    let branch = ChatId::new();
    let link = BranchLink {
        from: (first.record.chat_id, first.record.attachment.id),
        to: (branch, ChatAttachmentId::new()),
    };
    store.link_for_branch(std::slice::from_ref(&link)).unwrap();
    let linked = store
        .attachment_dir(link.to.0, link.to.1)
        .join(ORIGINAL_DIR)
        .join("first.txt");
    assert_eq!(fs::read(&linked).unwrap(), b"first");

    // A second link onto an existing folder fails, and takes back the one
    // it made before it.
    let fresh = BranchLink {
        from: link.from,
        to: (branch, ChatAttachmentId::new()),
    };
    assert!(store.link_for_branch(&[fresh.clone(), link]).is_err());
    assert!(!store.attachment_dir(fresh.to.0, fresh.to.1).exists());
}

#[test]
fn a_sent_copy_of_pure_noise_still_fits_its_bytes() {
    let mut state = 0x9e37_79b9_u32;
    let noise = ImageBuffer::from_fn(3_000, 3_000, |_, _| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let [r, g, b, _] = state.to_le_bytes();
        Rgb([r, g, b])
    });
    let sent = sent_copy(&DynamicImage::ImageRgb8(noise)).unwrap();
    assert!(sent.bytes.len() as u64 <= MAX_SENT_COPY_BYTES);
    let decoded = image::load_from_memory(&sent.bytes).unwrap();
    assert!(decoded.width() <= SENT_COPY_EDGE);
}

#[test]
fn a_picture_too_big_to_decode_is_attached_as_a_file() {
    // A 16-bit picture spends eight bytes a pixel; a bound below that is
    // what a 48 MP one meets at the real limit.
    let image = image::ImageBuffer::<image::Rgba<u16>, _>::from_pixel(
        64,
        64,
        image::Rgba([1, 2, 3, 65_535]),
    );
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba16(image)
        .write_to(Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    let picture = prepare_picture_within(&bytes, "deep.png", 64 * 64 * 4).unwrap();
    assert!(matches!(
        picture,
        Picture::TooLargeToPreview {
            media_type: "image/png"
        }
    ));
    assert!(matches!(
        prepare_picture(&bytes, "deep.png").unwrap(),
        Picture::Previewed { .. }
    ));
}

#[test]
fn an_id_is_stored_by_one_upload_at_a_time() {
    let (_directory, store) = store();
    let id = ChatAttachmentId::new();
    let claim = store.claim(id).unwrap();
    assert!(
        store.claim(id).is_none(),
        "a second upload of the id waits its turn"
    );
    assert!(store.claim(ChatAttachmentId::new()).is_some());
    drop(claim);
    assert!(store.claim(id).is_some());
}
