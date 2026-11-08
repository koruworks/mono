// SPDX-License-Identifier: MPL-2.0

use super::*;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
const PE: &[u8] = b"MZ\x90\0\x03\0\0\0\x04\0\0\0\xFF\xFF";

#[test]
fn tokens_round_trip() {
    for kind in [AttachmentKind::Image, AttachmentKind::File] {
        assert_eq!(AttachmentKind::parse(kind.as_token()), Some(kind));
    }
    for source in [
        AttachmentSource::Clipboard,
        AttachmentSource::Drop,
        AttachmentSource::FilePicker,
        AttachmentSource::Workspace,
    ] {
        assert_eq!(AttachmentSource::parse(source.as_token()), Some(source));
    }
    assert_eq!(AttachmentSource::parse("paperclip"), None);
}

#[test]
fn every_listed_extension_is_allowed_and_case_does_not_matter() {
    for (extension, family, media_type) in ALLOWED_EXTENSIONS {
        let allowed = allowed_type(&format!("file.{extension}")).unwrap();
        assert_eq!(allowed.family, *family, "{extension}");
        assert_eq!(allowed.media_type, *media_type);
        assert_eq!(
            allowed_type(&format!("FILE.{}", extension.to_ascii_uppercase())),
            Some(allowed)
        );
    }
    assert_eq!(
        allowed_type("REPORT.PDF").map(|allowed| allowed.family),
        Some(AttachmentFamily::Pdf)
    );
    for name in ALLOWED_NAMES {
        assert_eq!(
            allowed_type(name).map(|allowed| allowed.family),
            Some(AttachmentFamily::SourceCode)
        );
    }
}

#[test]
fn executables_scripts_and_unlisted_types_are_refused_by_name() {
    for extension in REFUSED_EXTENSIONS {
        assert_eq!(
            allowed_type(&format!("setup.{extension}")),
            None,
            "{extension}"
        );
    }
    for name in [
        "archive.zip",
        "photo.heic",
        "scan.tiff",
        "image.bmp",
        ".env",
        "noextension",
    ] {
        assert_eq!(allowed_type(name), None, "{name}");
    }
}

#[test]
fn programming_language_files_are_source_code_opened_as_text() {
    for name in ["main.py", "app.js", "index.html", "lib.rs"] {
        assert!(allowed_type(name).unwrap().family.opens_as_text(), "{name}");
    }
    assert!(!AttachmentFamily::Pdf.opens_as_text());
}

#[test]
fn content_must_agree_with_the_claimed_family() {
    assert!(content_agrees(AttachmentFamily::Picture, PNG));
    assert!(content_agrees(
        AttachmentFamily::Picture,
        b"\xFF\xD8\xFF\xE0"
    ));
    assert!(content_agrees(AttachmentFamily::Picture, b"GIF89a\x01\0"));
    assert!(content_agrees(
        AttachmentFamily::Picture,
        b"RIFF\0\0\0\0WEBPVP8 "
    ));
    assert!(!content_agrees(AttachmentFamily::Picture, b"%PDF-1.7"));
    assert!(content_agrees(AttachmentFamily::Pdf, b"%PDF-1.7\n"));
    assert!(content_agrees(
        AttachmentFamily::OfficeOpenXml,
        b"PK\x03\x04rest"
    ));
    assert!(content_agrees(AttachmentFamily::RichText, b"{\\rtf1\\ansi"));
    assert!(content_agrees(
        AttachmentFamily::Video,
        b"\0\0\0\x20ftypisom"
    ));
    assert!(!content_agrees(AttachmentFamily::Video, b"\0\0\0\x20moov"));
    assert!(content_agrees(
        AttachmentFamily::Text,
        "plain text, é".as_bytes()
    ));
    assert!(content_agrees(AttachmentFamily::Text, b"\xFF\xFEh\0i\0"));
    assert!(content_agrees(AttachmentFamily::Text, b"\xFE\xFF\0h\0i"));
}

#[test]
fn an_executable_is_refused_whatever_its_name_claims() {
    assert!(!content_agrees(AttachmentFamily::Picture, PE));
    assert!(!content_agrees(AttachmentFamily::Text, PE));
    assert!(!content_agrees(
        AttachmentFamily::SourceCode,
        b"\x7FELF\x02\x01"
    ));
    assert!(!content_agrees(AttachmentFamily::Text, b"\xCA\xFE\xBA\xBE"));
    let mut renamed_pe = b"MZ".to_vec();
    renamed_pe.extend_from_slice(PNG);
    assert!(!content_agrees(AttachmentFamily::Picture, &renamed_pe));
}

#[test]
fn the_text_rule_refuses_nul_and_invalid_utf8_but_not_a_cut_character() {
    assert!(!content_agrees(AttachmentFamily::Text, b"# notes\0\0\0"));
    assert!(!content_agrees(AttachmentFamily::Text, b"\xC3\x28 broken"));
    // "é" is two bytes; the head may end between them.
    assert!(content_agrees(AttachmentFamily::Text, b"caf\xC3"));
    assert!(!content_agrees(AttachmentFamily::Text, b"\xFF\xFEh\0\0\0"));
}

#[test]
fn picture_media_type_is_sniffed_from_the_bytes() {
    assert_eq!(picture_media_type(PNG), Some("image/png"));
    assert_eq!(picture_media_type(b"\xFF\xD8\xFF\xDB"), Some("image/jpeg"));
    assert_eq!(picture_media_type(b"BM\0\0"), None);
}

#[test]
fn file_names_are_sanitized_for_every_platform() {
    assert_eq!(sanitize_file_name("report.pdf"), "report.pdf");
    assert_eq!(sanitize_file_name("../../etc/passwd"), "etcpasswd");
    assert_eq!(sanitize_file_name("a<b>c:d\"e|f?g*h.txt"), "abcdefgh.txt");
    assert_eq!(sanitize_file_name("  .hidden. "), "hidden");
    assert_eq!(sanitize_file_name("tab\there.md"), "tabhere.md");
    assert_eq!(sanitize_file_name("CON.txt"), "CON_.txt");
    assert_eq!(sanitize_file_name("nul"), "nul_");
    assert_eq!(sanitize_file_name("..."), "attachment");
    assert_eq!(sanitize_file_name(""), "attachment");
    let long = format!("{}.png", "x".repeat(300));
    let cut = sanitize_file_name(&long);
    assert_eq!(cut.chars().count(), MAX_ATTACHMENT_NAME_CHARS);
    assert!(cut.ends_with(".png"));
}

#[test]
fn the_limits_hold_ten_sent_copies_inside_the_prompt_frame() {
    let base64 = MAX_SENT_COPY_BYTES.div_ceil(3) * 4;
    assert!(base64 * MAX_ATTACHMENTS_PER_MESSAGE as u64 <= 16 * 1024 * 1024);
    assert!(MAX_ATTACHMENT_BYTES <= MAX_MESSAGE_ATTACHMENT_BYTES);
}
