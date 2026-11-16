// SPDX-License-Identifier: MPL-2.0

//! **A record's page**: what the centre and the properties column show for
//! the record on show, whether or not the Lab can run it.
//!
//! ```text
//! scroll  Preview                     the centre; scrolls, and takes the keyboard to do it
//!   col   Preview stage               a story's component, on a panel       (a story)
//!   Click result                      what using it did                     (a story)
//!   col   Record status               the status, and why there is nothing  (no story)
//!   col   Symbols                     every recorded symbol, as written
//!     col Symbol                      its path, its status, its option sets
//! scroll  Properties                  the right column
//!   col   Component identity          name, public name, catalog ID
//!   Component purpose
//!   col   Property                    an input the registration lists       (a story)
//!   col   Fact                        kind, source, availability, A2UI, story
//! ```
//!
//! **A record without a story is never given a stand-in.** Its page says
//! which of the four reasons applies, in words, and lists what was recorded
//! about it. Nothing is drawn in the component's place.
//!
//! **Identifiers are shown as they are written.** A symbol's path, an option
//! set's type and its members, a catalog ID and a module are the same in
//! every language. Only the words around them are translated.
//!
//! [`facts`] is the one list of what the properties column states about a
//! record: the tree draws it and the screen reader's tree reads it, so the
//! two cannot say different things.

use super::settings::PreviewWidth;
use super::*;

/// The centre's viewport: the key the wheel, the scrollbar and the keyboard
/// scroll it by.
pub(super) const PAGE: Key = Key::surface("ui-lab", "page");
/// The properties column's viewport.
pub(super) const PANEL: Key = Key::surface("ui-lab", "properties");
/// The status a record without a story states.
pub(super) const STATUS: Key = Key::surface("ui-lab", "status");
/// Why there is nothing to run.
pub(super) const REASON: Key = Key::surface("ui-lab", "status-reason");
/// The record's name at the top of the properties column.
pub(super) const NAME: Key = Key::surface("ui-lab.fact", "name");
/// The record's one-line purpose.
pub(super) const PURPOSE: Key = Key::surface("ui-lab.fact", "purpose");

/// The surface a symbol's block is identified under; its part is the symbol.
pub(super) const SYMBOL: &str = "ui-lab.symbol";

/// One thing the properties column states about a record.
pub(super) struct Fact {
    pub(super) key: Key,
    /// What the fact is called: translated, or a product name.
    pub(super) name: String,
    /// What is stated, a line each.
    pub(super) lines: Vec<String>,
}

impl Fact {
    /// The fact as one sentence, for a screen reader.
    pub(super) fn spoken(&self) -> String {
        named(&self.name, &self.lines.join(", "))
    }
}

/// `name: value`, punctuated the way the display language does it.
fn named(name: &str, value: &str) -> String {
    koru_i18n::fill(text(koru_i18n::Key::UiLabFactLine), &[name, value])
}

/// What the properties column states about `entry`, in order: what kind of
/// thing it is, where it lives, where it is available on a desktop, whether a
/// declaration can name it, and whether the Lab can run it.
pub(super) fn facts(entry: &Entry) -> Vec<Fact> {
    let fact = |part: &'static str, name: &str, lines: Vec<String>| Fact {
        key: Key::surface("ui-lab.fact", part),
        name: name.to_owned(),
        lines,
    };
    vec![
        fact(
            "kind",
            text(koru_i18n::Key::UiLabControlKind),
            vec![text(entry.kind.label()).to_owned()],
        ),
        fact(
            "source",
            text(koru_i18n::Key::UiLabFactSource),
            vec![
                entry.source.owner.to_owned(),
                entry.source.module.to_owned(),
            ],
        ),
        // The desktop platforms only: the gallery shows no phone's column.
        fact(
            "availability",
            text(koru_i18n::Key::UiLabFactAvailability),
            registry::Platform::DESKTOP
                .into_iter()
                .map(|platform| named(platform.name(), text(entry.availability(platform).label())))
                .collect(),
        ),
        fact(
            "a2ui",
            "A2UI",
            entry
                .a2ui()
                .into_iter()
                .map(registry::A2ui::sentence)
                .collect(),
        ),
        fact(
            "story",
            text(koru_i18n::Key::UiLabFactStory),
            vec![text(entry.status().label()).to_owned()],
        ),
    ]
}

/// A symbol's status under its path: whether the Lab runs it, and whether a
/// declaration can name it.
pub(super) fn symbol_status(symbol: &registry::Symbol) -> String {
    format!(
        "{} · {}",
        text(symbol.story.label()),
        symbol.a2ui.sentence()
    )
}

/// An option set as one line: the type the values belong to, then the values
/// as the source spells them.
pub(super) fn option_line(set: &registry::OptionSet) -> String {
    let members: Vec<&str> = set.members.iter().map(|member| member.ident).collect();
    named(set.source, &members.join(", "))
}

/// `identifier` with a place to break after each separator. A path has no
/// spaces, so without one a long symbol could not wrap and would leave its
/// column. A zero-width space draws nothing: the identifier reads exactly as
/// it is written.
pub(super) fn breakable(identifier: &str) -> String {
    let mut out = String::with_capacity(identifier.len() + 16);
    let mut characters = identifier.chars().peekable();
    while let Some(character) = characters.next() {
        out.push(character);
        // After a path's own separators, never inside a name: `S0_5` and
        // `koru-ui` stay whole.
        let separator =
            matches!(character, '.' | '/') || (character == ':' && characters.peek() != Some(&':'));
        if separator && characters.peek().is_some() {
            out.push('\u{200B}');
        }
    }
    out
}

impl Lab {
    /// How wide the centre's content may be: the window less the two side
    /// columns, the lines between them and the page's own inset.
    pub(super) fn page_room(&self, width: f32) -> f32 {
        let profile = Profile::Desktop;
        (width
            - NAVIGATOR_WIDTH.value
            - PROPERTIES_WIDTH.value
            - 2.0 * theme::BORDER_W as f32
            - 2.0 * PREVIEW_INSET.token.px(profile))
        .max(0.0)
    }

    /// The centre: the story when the record has one, its status when it has
    /// not, and under either every symbol the record names.
    pub(super) fn page(&self, engine: &mut TextEngine, width: f32) -> El {
        let entry = self.entry;
        // The preview is as wide as its setting says, and never wider than
        // the column: what would not fit is not drawn over a neighbour.
        let room = self.width.within(self.page_room(width));
        let mut page = el::scroll_y("Preview")
            .id(PAGE)
            .fill()
            .pad(PREVIEW_INSET)
            .gap(PAGE_GAP)
            .radius(Radius::M)
            .states(&FRAME_STATES)
            .scrollbar()
            .offset(self.page_scroll)
            .focusable()
            .semantics(Role::Group)
            .name(koru_i18n::Key::UiLabDetails)
            .action(el::Action::Focus);
        match &self.story {
            Some(story) => {
                page = page
                    .child(self.stage(engine, story, room))
                    .child(
                        el::text(self.result_line())
                            .named("Click result")
                            .role(roles::ROW)
                            .ink(Ink::Secondary)
                            .wrap()
                            .max_w(Length::Runtime(
                                room.max(button::MIN_W),
                                "preview column less its inset",
                            ))
                            .align_self(Align::Centre),
                    )
                    .child(el::divider(Line::Divider));
            }
            None => {
                let status = entry.status();
                let mut block = el::col("Record status").gap(SYMBOLS_GAP).child(
                    el::text(text(status.label()))
                        .named("Story status")
                        .id(STATUS)
                        .role(roles::SUBTITLE)
                        .wrap(),
                );
                if let Some(reason) = status.reason() {
                    block = block.child(
                        el::text(text(reason))
                            .named("Story status reason")
                            .id(REASON)
                            .role(roles::ROW)
                            .ink(Ink::Secondary)
                            .wrap(),
                    );
                }
                page = page.child(block).child(el::divider(Line::Divider));
            }
        }
        page.child(symbols(entry))
    }

    /// The panel a story's component stands on.
    fn stage(&self, engine: &mut TextEngine, story: &ButtonStory, room: f32) -> El {
        let profile = Profile::Desktop;
        // What the stage's own inset leaves the component. A label typed
        // past it is clipped at the stage rather than drawn over a neighbour.
        let inside = room - 2.0 * STAGE_INSET.token.px(profile);
        let subject = el::leaf(
            "Component under test",
            Slot {
                width: button::button_width(engine, story.label()),
                height: button::H,
            },
        )
        .semantic()
        .id(SUBJECT)
        .align_self(Align::Centre)
        .max_w(Length::Runtime(
            inside.max(button::MIN_W),
            "preview column less its insets",
        ))
        .hit()
        .focusable()
        .semantics(Role::Button)
        .name_text(story.label())
        .action(el::Action::Click)
        .action(el::Action::Focus);
        let stage = el::col("Preview stage")
            .id(STAGE)
            .align_self(Align::Centre)
            .pad(STAGE_INSET)
            .bg(Surface::Raised)
            .radius(Radius::L)
            .child(subject);
        match self.width {
            // As wide as the component needs.
            PreviewWidth::Fit => stage,
            // Exactly the width asked for, so the component is seen in it.
            PreviewWidth::Fixed(_) => stage.w(Length::Runtime(room, "the preview width setting")),
        }
    }

    /// The right column: what the record is, what is recorded about it, and
    /// an input for each thing its story's recipe takes.
    pub(super) fn properties(&self, engine: &mut TextEngine) -> El {
        let entry = self.entry;
        let mut identity = el::col("Component identity")
            .gap(IDENTITY_GAP)
            .child(
                el::text(text(entry.name))
                    .named("Component name")
                    .id(NAME)
                    .role(roles::PANE_TITLE)
                    .wrap(),
            )
            // Technical identifiers: the same in every language.
            .child(
                el::text(entry.public_name)
                    .named("Public name")
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .truncate(),
            )
            .child(
                el::text(entry.catalog_id)
                    .named("Catalog ID")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .truncate(),
            );
        if let Some((_, recipe)) = entry.story_id().and_then(registry::story) {
            identity = identity.child(
                el::text(breakable(recipe.symbol))
                    .named("Source symbol")
                    .role(roles::META)
                    .ink(Ink::Hint)
                    .wrap(),
            );
        }
        let mut column = el::scroll_y("Properties")
            .id(PANEL)
            .w(PROPERTIES_WIDTH)
            .pad(PANEL_INSET)
            .gap(PANEL_GAP)
            .scrollbar()
            .offset(self.panel_scroll)
            .semantics(Role::Group)
            .name(koru_i18n::Key::UiLabProperties)
            .child(identity)
            .child(
                el::text(text(entry.purpose))
                    .named("Component purpose")
                    .id(PURPOSE)
                    .role(roles::ROW)
                    .ink(Ink::Secondary)
                    .wrap(),
            )
            .child(el::divider(Line::Divider));
        // A story's inputs come before what is recorded: they are what the
        // column is opened for, and must not need scrolling to reach.
        if let Some(story) = &self.story {
            for control in story.controls() {
                column = column.child(
                    el::col("Property")
                        .gap(PROPERTY_GAP)
                        .child(
                            el::text(text(control.name()))
                                .named("Property name")
                                .role(roles::META)
                                .ink(Ink::Secondary)
                                .truncate(),
                        )
                        .child(self.control(engine, *control)),
                );
            }
            column = column.child(el::divider(Line::Divider));
        }
        column.children(facts(entry).into_iter().map(|fact| {
            el::col("Fact")
                .id(fact.key)
                .gap(FACT_GAP)
                .child(
                    el::text(fact.name)
                        .named("Fact name")
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .truncate(),
                )
                .children(fact.lines.into_iter().map(|line| {
                    el::text(breakable(&line))
                        .named("Fact value")
                        .role(roles::ROW)
                        .wrap()
                }))
        }))
    }
}

/// Every symbol the record names, as the source writes it, with what is
/// recorded beside each.
fn symbols(entry: &Entry) -> El {
    el::col("Symbols")
        .gap(SYMBOLS_GAP)
        .child(
            el::text(text(koru_i18n::Key::UiLabSymbols))
                .named("Symbols heading")
                .role(roles::POPOVER_HEADING)
                .truncate(),
        )
        .children(entry.symbols().map(|symbol| {
            el::col("Symbol")
                .id(Key::surface(SYMBOL, symbol.symbol))
                .gap(SYMBOL_GAP)
                .child(
                    el::text(breakable(symbol.symbol))
                        .named("Symbol path")
                        .role(roles::ROW)
                        .wrap(),
                )
                .child(
                    el::text(symbol_status(&symbol))
                        .named("Symbol status")
                        .role(roles::META)
                        .ink(Ink::Hint)
                        .wrap(),
                )
                .children(symbol.options.iter().map(|set| {
                    el::text(breakable(&option_line(set)))
                        .named("Option set")
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .wrap()
                }))
        }))
}

#[cfg(test)]
mod tests;
