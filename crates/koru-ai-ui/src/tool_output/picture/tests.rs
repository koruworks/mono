// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    ChatId, InferenceAttemptId, MessageId, Timestamp, ToolActivity, ToolActivityStatus,
    ToolApproval, ToolInvocationId,
};
use koru_i18n::Lang;

use super::*;
use crate::tool_output::open_path::resolve;

fn read(tool: &str, label: &str, input: Option<&str>, output: Option<&str>) -> ToolActivity {
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: InferenceAttemptId::new(),
        chat_id: ChatId::new(),
        assistant_message_id: MessageId::new(),
        tool_name: tool.into(),
        read_only: false,
        status: ToolActivityStatus::Complete,
        approval: ToolApproval::NotRequired,
        proposed_at: Timestamp::from_millis(2_000),
        started_at: Some(Timestamp::from_millis(2_000)),
        completed_at: Some(Timestamp::from_millis(3_000)),
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: Some(label.into()),
        agent_summary: None,
        agent_input: input.map(str::to_owned),
        agent_output: output.map(str::to_owned),
        text_offset: None,
        display_target: None,
        live: None,
    }
}

/// The pictures in a call's detail, by the path each names.
fn pictures(activity: &ToolActivity) -> Vec<String> {
    crate::tool_output::pieces(activity, Lang::En, 10_000, false)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Picture { path, size } => {
                assert_eq!(size, None, "a picture is sized only once it has loaded");
                Some(path)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn only_the_formats_the_decoder_reads_are_pictures() {
    for path in [
        "a.png",
        "a.jpg",
        "a.jpeg",
        "a.gif",
        "a.webp",
        "a.bmp",
        "D:\\shots\\Apple.JPEG",
        "shots/pear.PNG",
        " spaced.png ",
    ] {
        assert!(is_picture(path), "{path} is a picture");
    }
    for path in [
        "src/lib.rs",
        "logo.svg",
        "notes.txt",
        "scan.pdf",
        "picture",
        "a.png.txt",
        "png",
        "",
    ] {
        assert!(!is_picture(path), "{path} is not a picture");
    }
}

/// **Which steps get a picture** (D38): Claude's read of a picture, by its
/// input or by its title alone; Codex's `view_image`, by its input. A read
/// of source, a command that names a picture and an edit of one do not.
#[test]
fn a_read_of_a_picture_carries_it_under_its_path_and_nothing_else_does() {
    let claude = read(
        "agent.read",
        "Read D:\\shots\\apple.jpeg",
        Some(r#"{"file_path":"D:\\shots\\apple.jpeg"}"#),
        None,
    );
    assert_eq!(pictures(&claude), ["D:\\shots\\apple.jpeg"]);
    let titled = read("agent.other", "Read shots/pear.PNG", None, None);
    assert_eq!(pictures(&titled), ["shots/pear.PNG"]);
    let codex = read(
        "agent.other",
        "view_image",
        Some(r#"{"path":"C:\\work\\plum.webp"}"#),
        None,
    );
    assert_eq!(pictures(&codex), ["C:\\work\\plum.webp"]);
    // Its line in the opened step names the file, not the tool.
    assert_eq!(
        crate::tool_groups::call_line(&codex, Lang::En),
        "C:\\work\\plum.webp"
    );

    // The picture sits under the path, above anything the agent sent.
    let with_text = read(
        "agent.read",
        "Read a.png",
        Some(r#"{"file_path":"a.png"}"#),
        Some("1\tnot really text"),
    );
    let pieces = crate::tool_output::pieces(&with_text, Lang::En, 10_000, false);
    assert!(matches!(&pieces[0], Piece::Header { text, .. } if text == "a.png"));
    assert!(matches!(&pieces[1], Piece::Picture { path, .. } if path == "a.png"));
    assert!(matches!(&pieces[2], Piece::Code(_)));

    for other in [
        read(
            "agent.read",
            "Read src/lib.rs",
            Some(r#"{"file_path":"src/lib.rs"}"#),
            Some("1\tfn main() {}"),
        ),
        read("agent.command", "cat a.png", None, Some("\u{89}PNG")),
        read(
            "agent.file_change",
            "Write a.png",
            Some(r#"{"file_path":"a.png"}"#),
            None,
        ),
    ] {
        assert!(pictures(&other).is_empty(), "{:?}", other.agent_label);
    }
}

fn detail(path: &str) -> Vec<Piece> {
    vec![
        Piece::Header {
            text: path.into(),
            mono: false,
        },
        Piece::Picture {
            path: path.into(),
            size: None,
        },
        Piece::Code(vec![crate::tool_output::CodeLine::plain(
            "text",
            crate::tool_output::Ink::Text,
        )]),
    ]
}

/// **Path resolution**: a relative path is read against the chat's working
/// folder, as a changed file's path is (D62); an absolute one is as it is;
/// a relative one with nowhere to read it against is left out.
#[test]
fn a_pictures_path_is_read_against_the_chats_working_folder() {
    let folder = std::env::temp_dir().join("koru-work");
    let folder = folder.to_string_lossy().into_owned();
    let mut wanted = Vec::new();
    settle(
        detail("shots/pear.png"),
        |path| resolve(path, Some(folder.as_str()), None),
        |_| None,
        &mut wanted,
    );
    assert_eq!(wanted, [Path::new(&folder).join("shots/pear.png")]);

    let absolute = std::env::temp_dir().join("apple.png");
    let mut wanted = Vec::new();
    settle(
        detail(&absolute.to_string_lossy()),
        |path| resolve(path, Some(folder.as_str()), None),
        |_| None,
        &mut wanted,
    );
    assert_eq!(wanted, [absolute]);

    let mut wanted = Vec::new();
    let settled = settle(
        detail("shots/pear.png"),
        |path| resolve(path, None, None),
        |_| None,
        &mut wanted,
    );
    assert!(wanted.is_empty());
    assert_eq!(settled.len(), 2, "the path and the text, as before");
}

/// **A picture is drawn only once it has loaded**; one asked for is asked
/// once, and one missing from the disk or undecodable leaves the detail as
/// it was before pictures: its path and its text, with no box and no error.
#[test]
fn a_picture_not_loaded_takes_no_room_and_a_missing_one_falls_back() {
    let path = std::env::temp_dir().join("apple.png");
    let key = path.to_string_lossy().into_owned();
    let before = vec![detail(&key)[0].clone(), detail(&key)[2].clone()];
    for (state, asked) in [
        (None, true),
        (Some(Loaded::Loading), false),
        (Some(Loaded::Failed), false),
    ] {
        let mut wanted = Vec::new();
        let settled = settle(
            detail(&key),
            |path| Some(PathBuf::from(path)),
            |asked_key| {
                assert_eq!(asked_key, key);
                state
            },
            &mut wanted,
        );
        assert_eq!(settled, before, "{state:?}");
        assert_eq!(!wanted.is_empty(), asked, "{state:?}");
    }
    let settled = settle(
        detail(&key),
        |path| Some(PathBuf::from(path)),
        |_| Some(Loaded::Ready(800, 400)),
        &mut Vec::new(),
    );
    assert_eq!(
        settled[1],
        Piece::Picture {
            path: key.clone(),
            size: Some((800, 400)),
        }
    );

    // Laid out, only the loaded picture takes a line.
    let mut engine = koru_draw::text::TextEngine::new();
    let lines = crate::tool_output::layout::lay_out(&mut engine, &settled, 300.0);
    let shown = lines
        .iter()
        .filter_map(|line| line.picture.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].key, key);
    assert_eq!((shown[0].width, shown[0].height), (300.0, 150.0));
    let lines = crate::tool_output::layout::lay_out(&mut engine, &detail(&key), 300.0);
    assert!(lines.iter().all(|line| line.picture.is_none()));
}

/// Fitted to the column and no taller than a reply's picture box, keeping
/// its shape — and never made larger than it is.
#[test]
fn a_picture_fits_the_column_and_the_replys_box_and_never_grows() {
    assert_eq!(fitted(1200, 600, 300.0), (300.0, 150.0));
    let (width, height) = fitted(100, 1000, 600.0);
    assert!((height - MAX_HEIGHT).abs() < 0.01);
    assert!((width - 26.0).abs() < 0.01);
    assert_eq!(fitted(40, 20, 300.0), (40.0, 20.0));
    assert_eq!(fitted(0, 0, 300.0), (1.0, 1.0));
}

/// **A reply's picture from a file** (2026-11-03): a `file:` URL or an
/// absolute path naming a picture is read from the disk; a relative path,
/// another host, a web address or a file that is not a picture is not.
#[test]
fn a_replys_file_url_or_absolute_path_names_a_picture() {
    let path = |url: &str| reply_picture_path(url).map(|path| path.to_string_lossy().into_owned());
    if cfg!(windows) {
        assert_eq!(
            path("file:///D:/apple.jpeg").as_deref(),
            Some("D:/apple.jpeg")
        );
        assert_eq!(
            path("FILE://localhost/D:/My%20Pictures/apple.PNG?x=1#y").as_deref(),
            Some("D:/My Pictures/apple.PNG")
        );
        assert_eq!(
            path(r"D:\pictures\apple.webp").as_deref(),
            Some(r"D:\pictures\apple.webp")
        );
        assert_eq!(path("D:/apple.jpeg").as_deref(), Some("D:/apple.jpeg"));
    } else {
        assert_eq!(
            path("file:///home/me/apple.jpeg").as_deref(),
            Some("/home/me/apple.jpeg")
        );
        assert_eq!(
            path("file://localhost/home/me/My%20Pictures/apple.PNG").as_deref(),
            Some("/home/me/My Pictures/apple.PNG")
        );
        assert_eq!(
            path("/home/me/apple.webp").as_deref(),
            Some("/home/me/apple.webp")
        );
    }
    for refused in [
        "apple.jpeg",
        "./pictures/apple.jpeg",
        "pictures/apple.jpeg",
        "file://server/share/apple.jpeg",
        "file:apple.jpeg",
        "file:///D:/notes.txt",
        "https://example.com/apple.jpeg",
        "file:///D:/bad%zzname.png",
    ] {
        assert_eq!(path(refused), None, "{refused}");
    }
}
