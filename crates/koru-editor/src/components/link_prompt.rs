// SPDX-License-Identifier: MPL-2.0

//! Ctrl+Shift+L: a caption and a URL.
//!
//! Two single-line fields, so both the key handling and the "which one has
//! focus" question live here rather than in the editor's key map.

use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::text_field::TextField;
use super::{Outcome, text_field};
use crate::trigger::{PillSpec, Trigger};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;

pub const FIELD_H: f32 = 26.0;
pub const PANEL_H: f32 = (20.0 + FIELD_H) * 2.0 + 16.0;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Field {
    Name,
    Url,
}

impl Field {
    pub fn other(self) -> Self {
        match self {
            Self::Name => Self::Url,
            Self::Url => Self::Name,
        }
    }

    /// Both, in the order they are drawn.
    pub const BOTH: [Self; 2] = [Self::Name, Self::Url];
}

/// Where a field's box sits inside the panel, in panel-local coordinates.
///
/// **One function, shared by drawing and hit-testing**, so a field is clickable
/// exactly where it is drawn — there is no second copy of the arithmetic to
/// drift ([DESIGN.md](../../../../docs/features/design/DESIGN.md)).
pub fn field_rect(which: Field, pw: f32) -> Rect {
    let i = match which {
        Field::Name => 0.0,
        Field::Url => 1.0,
    };
    Rect::new(
        theme::PANEL_PAD + 2.0,
        theme::PANEL_PAD + i * (20.0 + FIELD_H) + 16.0,
        pw - theme::PANEL_PAD * 2.0 - 4.0,
        FIELD_H,
    )
}

/// Whether a piece of selected text reads as a link target rather than a
/// caption, so `Ctrl+Shift+L` knows which of its two fields the selection fills.
///
/// Deliberately narrow. Guessing wrong costs the user a Tab, but calling a
/// caption a URL puts their words in the field that has to be a working
/// address — so anything with a space in it, or without a plausible host, is a
/// caption. `normalize_url` is what turns the survivors into something that
/// opens.
pub fn looks_like_a_url(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) {
        return false;
    }
    if t.contains("://") || t.starts_with("mailto:") {
        return true;
    }
    // A bare host — `example.com`, `www.domain.tld/path`. Judge the host alone:
    // everything after the first `/`, `?` or `#` is a path and may be anything.
    let host = t.split(['/', '?', '#']).next().unwrap_or(t);
    match host.rsplit_once('.') {
        Some((before, tld)) => {
            !before.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// A bare host becomes `https://`, so typing `example.com` produces a link that
/// opens. Anything already carrying a scheme is left exactly as typed.
pub fn normalize_url(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t.contains("://") || t.starts_with("mailto:") {
        t.to_string()
    } else {
        format!("https://{t}")
    }
}

pub struct LinkPrompt {
    pub trigger: Trigger,
    pub name: TextField,
    pub url: TextField,
    pub field: Field,
}

impl LinkPrompt {
    pub fn size() -> (f32, f32) {
        (theme::PANEL_W, theme::PANEL_PAD * 2.0 + PANEL_H)
    }

    /// What the caption field holds, trimmed.
    pub fn name_text(&self) -> String {
        self.name.value()
    }

    /// What the target field holds, trimmed — before `normalize_url` has had
    /// its say, so this is what the user typed rather than what will be stored.
    pub fn url_text(&self) -> String {
        self.url.value()
    }

    /// The field with the caret.
    pub fn focused(&mut self) -> &mut TextField {
        match self.field {
            Field::Name => &mut self.name,
            Field::Url => &mut self.url,
        }
    }

    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        let named = match key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        match named {
            // Both ways of switching field start the new one from the left:
            // the scroll belongs to whichever field has the caret.
            Some(NamedKey::Tab) | Some(NamedKey::ArrowUp) | Some(NamedKey::ArrowDown) => {
                self.field = self.field.other();
                return Outcome::Consumed;
            }
            Some(NamedKey::Enter) => return self.commit(),
            _ => {}
        }

        self.focused().key(engine, key, text, mods);
        Outcome::Consumed
    }

    /// The field a panel-local point is in, and that point in the field's own
    /// text — the conversion drawing and hit-testing have to agree on.
    fn hit(&self, local_x: f32, local_y: f32, pw: f32) -> Option<(Field, f32)> {
        let which = Field::BOTH
            .into_iter()
            .find(|&f| field_rect(f, pw).contains(local_x, local_y))?;
        Some((which, self.tx(which, local_x, pw)))
    }

    /// A panel-local x as an x in `which`'s text.
    fn tx(&self, which: Field, local_x: f32, pw: f32) -> f32 {
        let field = match which {
            Field::Name => &self.name,
            Field::Url => &self.url,
        };
        local_x - field_rect(which, pw).x - text_field::PAD + field.scroll()
    }

    /// A click inside the panel, in panel-local coordinates.
    ///
    /// Clicking a box focuses it **and** puts the caret where you clicked,
    /// because the two are one gesture: nobody clicks halfway along a URL
    /// meaning "focus this, then take me to the end".
    ///
    /// Reports whether it landed in a field, so the press can decide whether
    /// the drag that follows belongs to one.
    pub fn click(&mut self, engine: &mut TextEngine, local_x: f32, local_y: f32, pw: f32) -> bool {
        let Some((which, tx)) = self.hit(local_x, local_y, pw) else {
            return false;
        };
        self.field = which;
        self.focused().click(engine, tx);
        true
    }

    /// Carry the focused field's selection to a panel-local point.
    ///
    /// Deliberately does **not** hit-test: a drag that has wandered out of the
    /// box is still that field's drag, and asking would drop the selection the
    /// moment the pointer strayed.
    pub fn drag(&mut self, engine: &mut TextEngine, local_x: f32, pw: f32) {
        let tx = self.tx(self.field, local_x, pw);
        self.focused().drag_to(engine, tx);
    }

    /// Select the word under a panel-local point.
    pub fn double_click(&mut self, engine: &mut TextEngine, local_x: f32, local_y: f32, pw: f32) {
        let Some((which, tx)) = self.hit(local_x, local_y, pw) else {
            return;
        };
        self.field = which;
        self.focused().select_word_at(engine, tx);
    }

    /// A link needs a target; the caption falls back to the URL, because a pill
    /// with no text is invisible.
    fn commit(&self) -> Outcome {
        let name = self.name.value();
        let url = normalize_url(self.url.raw_text());
        if url.is_empty() {
            return Outcome::Close;
        }
        let label = if name.is_empty() { url.clone() } else { name };
        Outcome::Commit(PillSpec::link(label, url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(name: &str, url: &str) -> (LinkPrompt, TextEngine) {
        let mut engine = TextEngine::new();
        let p = LinkPrompt {
            trigger: Trigger::at_start(),
            name: TextField::with_text(&mut engine, theme::role::body(), name),
            url: TextField::with_text(&mut engine, theme::role::body(), url),
            field: Field::Name,
        };
        (p, engine)
    }

    #[test]
    fn a_bare_host_gains_a_scheme_and_a_full_url_is_left_alone() {
        assert_eq!(normalize_url("example.com"), "https://example.com");
        assert_eq!(normalize_url("http://x.dev"), "http://x.dev");
        assert_eq!(normalize_url("mailto:a@b.c"), "mailto:a@b.c");
        assert_eq!(normalize_url("   "), "");
    }

    #[test]
    fn tab_and_vertical_arrows_swap_fields() {
        let (mut p, mut e) = prompt("", "");
        p.key(
            &mut e,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(p.field, Field::Url);
    }

    /// Which field a selection fills is read off the text itself. Guessing
    /// wrong costs a Tab; calling a caption a URL puts the user's words in the
    /// field that has to be a working address, so the doubtful cases are
    /// captions.
    #[test]
    fn a_url_is_told_from_a_caption() {
        for url in [
            "https://www.domain.tld",
            "http://example.com",
            "example.com",
            "www.domain.tld/path?q=1",
            "mailto:someone@example.com",
        ] {
            assert!(looks_like_a_url(url), "{url}");
        }
        for caption in [
            "Terms page",
            "terms",
            "",
            "   ",
            "read the docs.",
            "1.5",
            "an idea. and another",
        ] {
            assert!(!looks_like_a_url(caption), "{caption}");
        }
    }

    /// The panel width the geometry is laid out for.
    const PW: f32 = koru_draw::theme::PANEL_W;

    /// A point inside `which`'s box, `along` pixels into its text.
    fn at(which: Field, along: f32) -> (f32, f32) {
        let r = field_rect(which, PW);
        (r.x + text_field::PAD + along, r.y + r.h * 0.5)
    }

    /// Clicking a box focuses it *and* takes the caret there — one gesture.
    #[test]
    fn clicking_a_box_focuses_it() {
        let (mut p, mut e) = prompt("Terms", "example.com");
        assert_eq!(p.field, Field::Name);

        let (x, y) = at(Field::Url, 4.0);
        assert!(p.click(&mut e, x, y, PW));

        assert_eq!(p.field, Field::Url);
    }

    /// A click that misses both boxes changes nothing — and says so, so the
    /// press knows the drag that follows is not a field's.
    #[test]
    fn clicking_off_the_boxes_focuses_nothing() {
        let (mut p, mut e) = prompt("Terms", "example.com");
        assert!(!p.click(&mut e, 2.0, 2.0, PW));
        assert_eq!(p.field, Field::Name);
    }

    /// Press, move, and the text between is selected.
    #[test]
    fn a_drag_selects_between_press_and_pointer() {
        let (mut p, mut e) = prompt("", "one two three");
        let (x, y) = at(Field::Url, 0.0);
        p.click(&mut e, x, y, PW);

        p.drag(&mut e, x + 60.0, PW);

        let picked = p.url.selected_text().expect("a selection");
        assert!(
            "one two three".starts_with(picked),
            "selected from the start: {picked:?}"
        );
    }

    /// A drag that wanders out of the box keeps selecting rather than dropping
    /// what it had — which is why `drag` does not re-hit-test.
    #[test]
    fn a_drag_past_the_edge_keeps_selecting() {
        let (mut p, mut e) = prompt("", "one two three");
        let (x, y) = at(Field::Url, 0.0);
        p.click(&mut e, x, y, PW);

        p.drag(&mut e, x + 10_000.0, PW);

        assert_eq!(p.url.selected_text(), Some("one two three"));
    }

    /// Double-click takes the word under the pointer.
    #[test]
    fn a_double_click_selects_the_word_under_it() {
        let (mut p, mut e) = prompt("", "one two three");
        let (x, y) = at(Field::Url, 0.0);
        p.double_click(&mut e, x, y, PW);

        assert_eq!(p.url.selected_text(), Some("one"));
        assert_eq!(p.field, Field::Url, "and focuses the box it landed in");
    }

    /// Each field has its own history, so undo in one cannot reach the other.
    #[test]
    fn each_field_keeps_its_own_undo() {
        let (mut p, mut e) = prompt("", "");
        let key = |p: &mut LinkPrompt, e: &mut TextEngine, c: &str| {
            p.key(
                e,
                &Key::Character(c.into()),
                Some(c),
                ModifiersState::empty(),
            );
        };
        key(&mut p, &mut e, "a");
        p.key(
            &mut e,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::empty(),
        );
        key(&mut p, &mut e, "b");
        assert_eq!(p.url_text(), "b");

        p.key(
            &mut e,
            &Key::Character("z".into()),
            None,
            koru_keys::PRIMARY,
        );

        assert_eq!(p.url_text(), "", "the focused field undid");
        assert_eq!(p.name_text(), "a", "and the other one did not");
    }

    /// No URL means no link — there would be nothing to open.
    #[test]
    fn enter_without_a_url_closes_rather_than_committing() {
        let (mut p, mut e) = prompt("Terms", "");
        assert!(matches!(
            p.key(
                &mut e,
                &Key::Named(NamedKey::Enter),
                None,
                ModifiersState::empty()
            ),
            Outcome::Close
        ));
    }

    /// A pill with no caption would be invisible, so the URL stands in.
    #[test]
    fn an_empty_caption_falls_back_to_the_url() {
        let (mut p, mut e) = prompt("", "example.com");
        match p.key(
            &mut e,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ) {
            Outcome::Commit(spec) => assert_eq!(spec.label, "https://example.com"),
            other => panic!("expected a commit, got {other:?}"),
        }
    }

    #[test]
    fn a_caption_is_kept_when_there_is_one() {
        let (mut p, mut e) = prompt("Terms page", "example.com");
        match p.key(
            &mut e,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ) {
            Outcome::Commit(spec) => {
                assert_eq!(spec.label, "Terms page");
                assert_eq!(spec.url.as_deref(), Some("https://example.com"));
            }
            other => panic!("expected a commit, got {other:?}"),
        }
    }
}
