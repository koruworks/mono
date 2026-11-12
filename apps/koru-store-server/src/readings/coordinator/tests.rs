// SPDX-License-Identifier: MPL-2.0

use super::*;

/// What is worth sending, by media type: text-bearing files are read here,
/// pictures need the reader, everything else is not read at all.
#[test]
fn the_media_types_this_plan_reads() {
    for text in [
        "application/pdf",
        "text/plain; charset=utf-8",
        "text/markdown",
        "TEXT/CSV",
        "text/html",
    ] {
        assert_eq!(readable(text), Some(Readable::Text), "{text}");
    }
    for picture in ["image/png", "image/jpeg", "image/heic"] {
        assert_eq!(readable(picture), Some(Readable::Picture), "{picture}");
    }
    for other in ["application/zip", "video/mp4", ""] {
        assert_eq!(readable(other), None, "{other}");
    }
}

fn attempt(code: AttemptCode, attempts: u32, last_at: i64) -> Attempt {
    Attempt {
        code,
        attempts,
        last_at,
    }
}

/// **The attempts table's policy** (TECHNICAL §11, and the worker plan's
/// trap): one retry after a crash or a memory kill, never another after a
/// hostile file or an unsupported format, a backoff that grows for the rest,
/// and a file that needs the reader waits for one rather than failing.
#[test]
fn the_attempts_policy() {
    use AttemptCode::*;
    assert_eq!(
        verdict(&attempt(NeedsReader, 1, 0), 0),
        Verdict::NeedsReader
    );
    assert_eq!(
        verdict(&attempt(Unsupported, 1, 0), i64::MAX),
        Verdict::Failed
    );
    assert_eq!(verdict(&attempt(Hostile, 1, 0), i64::MAX), Verdict::Failed);
    assert_eq!(verdict(&attempt(Crashed, 1, 0), 0), Verdict::Go);
    assert_eq!(verdict(&attempt(Crashed, 2, 0), 0), Verdict::Failed);
    assert_eq!(verdict(&attempt(MemoryLimit, 1, 0), 0), Verdict::Go);
    assert_eq!(verdict(&attempt(MemoryLimit, 2, 0), 0), Verdict::Failed);
    assert_eq!(
        verdict(&attempt(Unreadable, 1, 0), BACKOFF_MILLIS - 1),
        Verdict::Backoff
    );
    assert_eq!(
        verdict(&attempt(Unreadable, 1, 0), BACKOFF_MILLIS),
        Verdict::Go
    );
    assert_eq!(
        verdict(&attempt(Unreadable, 2, 0), 4 * BACKOFF_MILLIS - 1),
        Verdict::Backoff
    );
    assert_eq!(
        verdict(&attempt(Unreadable, 2, 0), 4 * BACKOFF_MILLIS),
        Verdict::Go
    );
    assert_eq!(
        verdict(&attempt(Unreadable, MAX_RETRIES, 0), i64::MAX),
        Verdict::Failed
    );
    // **An internal failure — a picture the worker gave up on — is retried
    // once, after a backoff, and then left** (OCR Plan 2.4).
    assert_eq!(
        verdict(&attempt(Internal, 1, 0), BACKOFF_MILLIS - 1),
        Verdict::Backoff
    );
    assert_eq!(
        verdict(&attempt(Internal, 1, 0), BACKOFF_MILLIS),
        Verdict::Go
    );
    assert_eq!(verdict(&attempt(Internal, 2, 0), i64::MAX), Verdict::Failed);
}

/// A reading's confidence is the mean of its recognized pages', rounded.
#[test]
fn the_confidence_is_the_mean_of_the_recognized_pages() {
    assert_eq!(mean(&[]), None);
    assert_eq!(mean(&[90]), Some(90));
    assert_eq!(mean(&[90, 81]), Some(86));
    assert_eq!(mean(&[100, 100, 99]), Some(100));
}

/// The worker's codes the table remembers — and the two it must not, which
/// are about the conversation rather than the file.
#[test]
fn only_codes_about_the_file_are_remembered() {
    assert_eq!(
        local_code(FailureCode::NeedsReader),
        Some(AttemptCode::NeedsReader)
    );
    assert_eq!(
        local_code(FailureCode::Unsupported),
        Some(AttemptCode::Unsupported)
    );
    assert_eq!(local_code(FailureCode::Busy), None);
    assert_eq!(local_code(FailureCode::Cancelled), None);
}

/// A device's object is named by its hash, never by a file's name, and a
/// string that is not a hash names nothing.
#[test]
fn an_object_is_named_by_its_hash() {
    let objects = ObjectDir(PathBuf::from("objects"));
    let hash = format!("b3:{}", "ab".repeat(32));
    assert_eq!(
        objects.path(&hash),
        Some(PathBuf::from("objects").join(format!("b3-{}", "ab".repeat(32))))
    );
    assert_eq!(objects.path("../escape"), None);
}
