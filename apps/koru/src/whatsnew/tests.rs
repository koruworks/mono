// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_core::{Heading, ListStyle, Mark};
use koru_workspace_host::{RichListSpec, RichTextKind};

#[test]
fn github_release_links_match_published_tags_and_local_builds_use_the_list() {
    use koru_channel::ReleaseChannel;
    for (channel, version, expected) in [
        (
            ReleaseChannel::Stable,
            "0.1.4",
            "https://github.com/koruworks/mono/releases/tag/v0.1.4",
        ),
        (
            ReleaseChannel::Beta,
            "0.1.5-beta.2",
            "https://github.com/koruworks/mono/releases/tag/v0.1.5-beta.2",
        ),
        (
            ReleaseChannel::Nightly,
            "0.1.5-nightly.7",
            "https://github.com/koruworks/mono/releases/tag/nightly",
        ),
        (
            ReleaseChannel::Internal,
            "0.0.3",
            "https://github.com/koruworks/mono/releases",
        ),
    ] {
        assert_eq!(release_url(version, channel), expected);
    }
}

fn text_blocks(blocks: &[RichBlock]) -> Vec<&koru_workspace_host::RichTextBlock> {
    blocks
        .iter()
        .filter_map(|block| match block {
            RichBlock::Text(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// The one fact the greeting exists to carry.
#[test]
fn the_greeting_names_the_version_it_is_about() {
    let markdown = greeting("0.0.4-nightly.20260816+3b71911");
    assert!(markdown.contains("0.0.4-nightly.20260816+3b71911"));
    assert!(markdown.contains("https://www.koru.do/releases"));
}

/// **One paragraph, not a heading.** The document's own title row already names
/// the version; a second heading here would give the page two of them, and the
/// way that happens is a stray `#` or a line that Markdown reads as a setext
/// underline.
#[test]
fn the_greeting_is_a_single_paragraph() {
    let blocks = blocks("0.0.4", None);
    let texts = text_blocks(&blocks);
    assert_eq!(texts.len(), 1, "the greeting is one block");
    assert_eq!(texts[0].heading, None, "and not a heading");
    assert_eq!(texts[0].list, None);
    assert_eq!(texts[0].kind, RichTextKind::Paragraph);
    assert!(texts[0].text.contains("0.0.4"));
}

/// The releases page is a real link, so clicking it in the page opens the site.
#[test]
fn the_greeting_links_to_the_releases_page() {
    let blocks = blocks("0.0.4", None);
    let texts = text_blocks(&blocks);
    let link = texts[0]
        .links
        .first()
        .expect("the releases page is a link, not bare text");
    assert_eq!(link.url, "https://www.koru.do/releases");
    assert_eq!(&texts[0].text[link.start..link.end], "releases page");
}

/// A build that shipped without notes still says what it is. This is the
/// ordinary case for `cargo run` and for most `dist-local` bundles, so it must
/// be a whole page rather than a degraded one.
#[test]
fn a_build_with_no_notes_still_greets() {
    let blocks = blocks("0.0.4", None);
    assert!(!blocks.is_empty());
}

/// The notes keep their shape on the way through — this is what makes the tab
/// worth having rather than a wall of unformatted text.
#[test]
fn release_notes_arrive_as_real_structure() {
    let notes = "## Find and replace\n\n\
                 Press **Ctrl+F** to find text.\n\n\
                 - a bullet\n\
                 - another\n";
    let blocks = blocks("0.0.4", Some(notes));
    let texts = text_blocks(&blocks);

    assert!(
        texts
            .iter()
            .any(|t| t.heading == Some(Heading::H2) && t.text.contains("Find and replace")),
        "a heading stays a heading"
    );
    assert!(
        texts.iter().any(|t| matches!(
            t.list,
            Some(RichListSpec {
                style: ListStyle::Bullet,
                ..
            })
        )),
        "bullets stay bullets"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.runs.iter().any(|r| r.marks.has(Mark::Bold))),
        "bold stays bold"
    );
}

/// **Why [`blocks`] parses twice instead of joining two strings.**
///
/// A setext underline turns the paragraph *above* it into a heading. Joined into
/// one document, a release note that opened with `===` would reach back and
/// promote our greeting — silently, and only for the releases whose notes happen
/// to start that way. Somebody will propose the one-string version as a
/// simplification; this is the answer.
#[test]
fn notes_cannot_reach_back_and_reformat_the_greeting() {
    let blocks = blocks("0.0.4", Some("===\n\nordinary prose\n"));
    let texts = text_blocks(&blocks);
    assert_eq!(
        texts[0].heading, None,
        "the greeting is still a paragraph, whatever the notes open with"
    );
    assert!(texts[0].text.contains("0.0.4"));
}

/// A fence left open by the author must not swallow the rest of the page, and
/// must not swallow the greeting either — the greeting is parsed before the
/// notes are seen at all.
#[test]
fn an_unclosed_fence_in_the_notes_does_not_eat_the_greeting() {
    let blocks = blocks("0.0.4", Some("```\nnever closed\n"));
    let texts = text_blocks(&blocks);
    assert_eq!(texts[0].kind, RichTextKind::Paragraph);
    assert!(texts[0].text.contains("Updated to Koru 0.0.4"));
}

/// The headlines are the `## ` sections, in order, trimmed; a `#` title, a
/// `###` sub-heading and a fenced line that looks like one are not.
#[test]
fn headlines_are_the_second_level_sections_in_order() {
    let notes = "# Koru 0.1.3\n\nIntro.\n\n## Welcome  \n\ntext\n\n### Detail\n\n##   Agents in a tab\n\n##\n\n## Sync\n";
    assert_eq!(
        super::headlines(notes),
        vec![
            "Welcome".to_owned(),
            "Agents in a tab".to_owned(),
            "Sync".to_owned()
        ]
    );
    assert!(super::headlines("no headings here").is_empty());
}

/// **The actual releases, nightly first** (2026-11-10): the shape of
/// GitHub's real answer that day, plus what must never become a row.
#[test]
fn published_releases_put_the_nightly_first_then_stable_newest_first() {
    let body = r#"[
      {"tag_name":"v0.1.5","html_url":"https://github.com/koruworks/mono/releases/tag/v0.1.5","draft":false,"prerelease":false,"published_at":"2026-11-07T23:56:17Z"},
      {"tag_name":"nightly","html_url":"https://github.com/koruworks/mono/releases/tag/nightly","draft":false,"prerelease":true,"published_at":"2026-10-25T21:30:32Z"},
      {"tag_name":"v0.1.4","html_url":"https://github.com/koruworks/mono/releases/tag/v0.1.4","draft":false,"prerelease":false,"published_at":"2026-10-31T00:53:40Z"},
      {"tag_name":"v0.1.6","html_url":"https://github.com/koruworks/mono/releases/tag/v0.1.6","draft":true,"prerelease":false,"published_at":null},
      {"tag_name":"v0.2.0-rc1","html_url":"https://github.com/koruworks/mono/releases/tag/v0.2.0-rc1","draft":false,"prerelease":true,"published_at":"2026-11-08T00:00:00Z"},
      {"tag_name":"v0.1.3","html_url":"https://example.com/elsewhere","draft":false,"prerelease":false,"published_at":"2026-10-01T00:00:00Z"}
    ]"#;
    let releases = parse_releases(body).expect("a releases list");
    let versions: Vec<_> = releases.iter().map(|r| r.version.as_deref()).collect();
    assert_eq!(versions, [None, Some("0.1.5"), Some("0.1.4")]);
    assert_eq!(
        releases[0].published, None,
        "the nightly's date is not its build's"
    );
    assert_eq!(releases[1].published, Some((2026, 11, 7)));
    assert!(releases.iter().all(|r| r.url.starts_with(RELEASES_URL)));
    assert_eq!(
        parse_releases(r#"{"message":"API rate limit exceeded"}"#),
        None
    );
}

/// Four stable releases show the newest three; the rest are on the list.
#[test]
fn only_the_three_newest_stable_releases_follow_the_nightly() {
    let entry = |v: &str| {
        format!(
            r#"{{"tag_name":"v{v}","html_url":"https://github.com/koruworks/mono/releases/tag/v{v}","published_at":"2026-01-01T00:00:00Z"}}"#
        )
    };
    let body = format!(
        "[{},{},{},{}]",
        entry("0.1.2"),
        entry("0.1.10"),
        entry("0.1.9"),
        entry("0.1.1")
    );
    let releases = parse_releases(&body).unwrap();
    let versions: Vec<_> = releases
        .iter()
        .filter_map(|r| r.version.as_deref())
        .collect();
    assert_eq!(
        versions,
        ["0.1.10", "0.1.9", "0.1.2"],
        "by version, not by text"
    );
}
