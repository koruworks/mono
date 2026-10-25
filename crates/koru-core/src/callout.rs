// SPDX-License-Identifier: MPL-2.0

//! What a [`Kind::Callout`](crate::Kind::Callout) carries beyond its text, and
//! the rule that groups callout rows into one box.
//!
//! A callout is a leaf, one row per line, like a quote. Its **icon** lives in
//! [`NodeMeta::icon`] (one emoji; absent means [`DEFAULT_ICON`]) and its
//! **tint** in [`NodeMeta::colour`] (an [`AccentColor`] token; absent means the
//! default tint). Both are keys every build already reads, so neither needed a
//! schema step of its own beyond the one `Kind::Callout` took.
//!
//! Notion and Evernote callouts are containers that hold several blocks. They
//! arrive here as a run of callout rows that carry the same icon and tint, and
//! [`continues`] is what draws such a run as one box again, while keeping two
//! *different* callouts that happen to touch as two boxes.

use crate::attributes::AccentColor;
use crate::node::NodeMeta;

/// The icon a callout shows when none is set — the one Notion and Evernote
/// both start a callout with.
pub const DEFAULT_ICON: &str = "💡";

/// The icon `meta` draws, with the default filled in.
pub fn icon(meta: &NodeMeta) -> &str {
    meta.icon.as_deref().unwrap_or(DEFAULT_ICON)
}

/// The tint `meta` draws, or `None` for the default tint. A token from a newer
/// build's palette reads as the default rather than as an error.
pub fn tint(meta: &NodeMeta) -> Option<AccentColor> {
    meta.colour.as_deref().and_then(AccentColor::parse)
}

/// Whether a callout row carrying `next` belongs to the same box as the
/// callout row above it carrying `prev`: the same icon and the same tint.
///
/// Enter copies both to the new row, so typing never splits a box; changing
/// either is applied to the whole run, so editing never splits one either.
pub fn continues(prev: &NodeMeta, next: &NodeMeta) -> bool {
    icon(prev) == icon(next) && tint(prev) == tint(next)
}

/// A colour name as another app writes it — a Koru token, or a Notion API /
/// enhanced-markdown colour (`blue`, `blue_background`, `blue_bg`) — as the
/// tint Koru stores. `Some(None)` is an explicit *default*; `None` means the
/// name is not a colour at all.
///
/// Notion's `brown` has no Koru tone and lands on `orange`, the nearest one.
pub fn tint_from_name(name: &str) -> Option<Option<AccentColor>> {
    let name = name.trim().to_ascii_lowercase();
    let hue = name
        .strip_suffix("_background")
        .or_else(|| name.strip_suffix("_bg"))
        .unwrap_or(&name);
    match hue {
        "default" | "none" | "clear" => Some(None),
        "brown" => Some(Some(AccentColor::Orange)),
        "grey" | "lightgray" | "lightgrey" => Some(Some(AccentColor::Gray)),
        other => AccentColor::parse(other).map(Some),
    }
}

/// The Notion colour name for a Koru tint, for writing back out. Koru's teal
/// has no Notion counterpart and goes out as green.
pub fn notion_colour(tint: Option<AccentColor>) -> &'static str {
    match tint {
        None => "default",
        Some(AccentColor::Teal) => "green",
        Some(colour) => colour.as_str(),
    }
}

/// The icon and tint behind a Notion built-in icon address —
/// `https://www.notion.so/icons/<name>_<colour>.svg`, which is what a Notion
/// Markdown export writes for a callout whose icon is not an emoji.
///
/// Koru stores an emoji, so the common shapes map onto the nearest one; an icon
/// with no near emoji keeps the default. The colour suffix becomes the tint.
pub fn from_notion_icon_url(url: &str) -> Option<(Option<&'static str>, Option<AccentColor>)> {
    let (_, file) = url.rsplit_once("/icons/")?;
    let stem = file.strip_suffix(".svg").unwrap_or(file);
    let (name, colour) = match stem.rsplit_once('_') {
        Some((name, colour)) => (name, tint_from_name(colour).flatten()),
        None => (stem, None),
    };
    let base = name.split('-').next().unwrap_or(name);
    let emoji = match base {
        "info" => Some("ℹ️"),
        "warning" | "alert" | "exclamation" => Some("⚠️"),
        "lightbulb" | "idea" => Some("💡"),
        "checkmark" | "check" => Some("✅"),
        "close" | "cross" | "error" | "stop" | "forbidden" => Some("⛔"),
        "question" | "help" => Some("❓"),
        "star" => Some("⭐"),
        "fire" => Some("🔥"),
        "pin" | "pushpin" => Some("📌"),
        "bookmark" => Some("🔖"),
        "book" | "library" => Some("📚"),
        "clock" | "timer" | "hourglass" => Some("⏰"),
        "calendar" => Some("📅"),
        "code" | "terminal" => Some("💻"),
        "document" | "page" | "notes" | "note" => Some("📝"),
        "link" => Some("🔗"),
        "flag" => Some("🚩"),
        "heart" => Some("❤️"),
        "rocket" => Some("🚀"),
        "target" => Some("🎯"),
        "token" | "key" | "lock" => Some("🔑"),
        "megaphone" | "announcement" => Some("📣"),
        "cursor" => Some("👉"),
        _ => None,
    };
    Some((emoji, colour))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(icon: Option<&str>, colour: Option<&str>) -> NodeMeta {
        NodeMeta {
            icon: icon.map(str::to_owned),
            colour: colour.map(str::to_owned),
            ..NodeMeta::default()
        }
    }

    #[test]
    fn rows_with_the_same_look_are_one_box_and_absent_means_default() {
        assert!(continues(&meta(None, None), &meta(Some("💡"), None)));
        assert!(continues(
            &meta(Some("⚠️"), Some("red")),
            &meta(Some("⚠️"), Some("red"))
        ));
        assert!(!continues(&meta(Some("⚠️"), None), &meta(None, None)));
        assert!(!continues(&meta(None, Some("blue")), &meta(None, None)));
        // A token from a newer palette is the default tint, not a third value.
        assert!(continues(
            &meta(None, Some("ultraviolet")),
            &meta(None, None)
        ));
    }

    #[test]
    fn notion_colour_names_map_onto_koru_tints() {
        assert_eq!(
            tint_from_name("blue_background"),
            Some(Some(AccentColor::Blue))
        );
        assert_eq!(tint_from_name("red_bg"), Some(Some(AccentColor::Red)));
        assert_eq!(tint_from_name("Brown"), Some(Some(AccentColor::Orange)));
        assert_eq!(tint_from_name("default"), Some(None));
        assert_eq!(tint_from_name("teal"), Some(Some(AccentColor::Teal)));
        assert_eq!(tint_from_name("🔥"), None);
        assert_eq!(notion_colour(Some(AccentColor::Teal)), "green");
    }

    #[test]
    fn a_notion_icon_url_becomes_an_emoji_and_a_tint() {
        assert_eq!(
            from_notion_icon_url("https://www.notion.so/icons/info-alternate_gray.svg"),
            Some((Some("ℹ️"), Some(AccentColor::Gray)))
        );
        assert_eq!(
            from_notion_icon_url("/icons/token_blue.svg"),
            Some((Some("🔑"), Some(AccentColor::Blue)))
        );
        assert_eq!(
            from_notion_icon_url("https://www.notion.so/icons/squiggle_purple.svg"),
            Some((None, Some(AccentColor::Purple)))
        );
        assert_eq!(from_notion_icon_url("https://example.com/a.png"), None);
    }
}
