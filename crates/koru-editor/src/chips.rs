// SPDX-License-Identifier: MPL-2.0

//! Which attribute chips a node earns, and where they sit on its row.
//!
//! The one seam R0's pill ships through, and the one A3/A4 extend: a node's
//! set attributes become [`ChipSpec`]s here ([`attribute_chips`]), and one
//! geometry function ([`chip_boxes`]) places them for drawing and hit-testing
//! alike. The drawable itself — cached label layout, recipe,
//! metrics — is [`koru_draw::chip`], which names no node; this module is the
//! half that reads the document, the same split `pillinfo.rs` documents.
//!
//! **The hit-test half arrived with [ADR-188]**, and it is [`chip_at`] — one
//! function that hover and press both ask, so a chip's hover treatment, its
//! `×` and the picker a click opens can never be over three different
//! rectangles. Until then `chip_boxes` claimed to be "shared by draw and
//! (future) hit-test" while having exactly two callers, `draw` and the suite —
//! which is why the claim is now a fact rather than a plan.
//!
//! [ADR-188]: ../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
//!
//! **Unset is invisible** (Foundation invariant: unset attributes are hidden
//! in UI): a node with no `koru.tag` and no `koru.color` yields no chips and
//! this module draws nothing at all.
//!
//! The one thing that is *set* without holding a value is the **task marker**:
//! `koru.task` is an attribute like any other, and SPEC §4 gives it the due
//! chip to wear — dated when there is a date, dashed when there is not. That
//! is the invariant kept rather than bent; what would break it is a task
//! drawing nothing.

use std::collections::HashMap;

use koru_core::ids::ListId;

use chrono::NaiveDate;
use koru_core::attributes::{KEY_COLOR, KEY_DUE, KEY_TAG, Lifecycle, Priority};
use koru_core::ids::TagId;
use koru_core::node::Node;
use koru_core::tasks::{Bucket, TaskFields, is_task};
use koru_core::time::Temporal;
use koru_draw::chip::{CHIP_GAP, CHIP_H, ChipKind};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_ui::label_chip::{LabelChip, Lead, State, Trail};

use crate::paragraph::Paragraph;
use crate::utils::dates;
use crate::utils::zone::SystemZone;

/// One chip a row's node has earned. Derived from the node's attrs at
/// rebuild/refresh time and cached on the [`Paragraph`], so drawing a frame
/// never reads the document.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChipSpec {
    /// A `koru.tag` value. The id, never the name: the name lives in the
    /// workspace tag table and is resolved at draw time through
    /// [`TagLookup`], which is what makes rename touch one record and zero
    /// documents.
    Tag(TagId),
    /// The node's `koru.due`, as stored — **the value, never a formatted
    /// label**: the text ("Today", "Thu", "Aug 31") depends on what day it
    /// is, so it is derived at draw time against the pass's `today` rather
    /// than cached here, where midnight would strand it.
    ///
    /// And **the value in its own type**, not folded to a number: a chip that
    /// cached an epoch would have thrown away whether the deadline was a day,
    /// a wall clock or a zoned moment, which is the one thing its label needs
    /// to know ([ADR-183]).
    ///
    /// **`None` is a task with no date** (SPEC §4): the due chip *is* the task
    /// indicator, so a bare `/task` earns the same chip drawn with a dash. An
    /// `Option` inside the variant rather than a fifth variant, deliberately —
    /// the dash chip is the due chip in one more state, and every `match` in
    /// the tree that routes by chip kind (the press arm's
    /// `ChipSpec::Due(_) => open_due_chooser`, `chip_boxes`, `chip_at`, the
    /// draw pass) keeps working unchanged, which is exactly the property
    /// "clicking it opens the due picker" needs.
    ///
    /// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
    Due(Option<Temporal>),
    /// The node's `koru.priority`. The label is the level's name.
    Priority(Priority),
    /// The node's `koru.color`, as its stable storage token.
    Color(String),
}

/// The chips `node`'s attributes earn, in display order: tags first (attr
/// order, which is id order — stable across peers), then due, then
/// priority, then the color swatch.
///
/// **The due chip is also the task indicator** (SPEC §4, owner correction
/// 2026-08-31: there is no separate task chip). A due-bearing row that becomes
/// a task keeps the chip it already had, and a task with no date earns the
/// same chip with a dash — so "this is a task" and "this is when it is due"
/// are one mark on the row rather than two competing for the same lane. The
/// legacy [`Kind::Task`](koru_core::node::Kind::Task) is *not* special-cased
/// anywhere here: a reused outline kind carries no rendering of its own, so it
/// earns this chip on the same derivation as a paragraph, an image or a file.
pub fn attribute_chips(node: &Node) -> Vec<ChipSpec> {
    let mut chips: Vec<ChipSpec> = node
        .content
        .attrs()
        .iter()
        .filter(|a| a.role == KEY_TAG)
        .filter_map(|a| a.as_text().and_then(TagId::parse))
        .map(ChipSpec::Tag)
        .collect();
    // `KEY_DUE` only — deliberately *not* `Node::due_value`, whose legacy
    // fallback also reads the bare `"due"` role a **date pill** stores. A
    // pill is inline content already on the row; giving it a trailing chip
    // as well would print the same date twice.
    match node
        .content
        .attrs()
        .iter()
        .find(|a| a.role == KEY_DUE)
        .and_then(|a| a.value.temporal())
    {
        Some(due) => chips.push(ChipSpec::Due(Some(due))),
        // No `koru.due` — but a **task** still shows the chip, dashed. The
        // derivation is `is_task` and nothing else (marker OR due), so
        // reaching here with it true means the *marker* is what made this a
        // task: the bare `/task`.
        //
        // `TaskFields::due` is [`Node::due`], legacy role included, and the
        // dash is refused when it answers `Some` — a row whose only date is a
        // **date pill** already prints that date inline, and the reason this
        // function reads `KEY_DUE` rather than `due_value` above is precisely
        // that a pill must not be echoed by a chip. A dash beside it would be
        // the same mistake wearing a different label.
        //
        // Paid only by rows with no `koru.due`, and `TaskFields::of` is pure
        // — three lookups over an attr list that is empty on the common row.
        None => {
            let fields = TaskFields::of(node);
            if is_task(&fields) && fields.due.is_none() {
                chips.push(ChipSpec::Due(None));
            }
        }
    }
    if let Some(priority) = node.priority() {
        chips.push(ChipSpec::Priority(priority));
    }
    if let Some(token) = node
        .content
        .attrs()
        .iter()
        .find(|a| a.role == KEY_COLOR)
        .and_then(|a| a.as_text())
    {
        chips.push(ChipSpec::Color(token.to_owned()));
    }
    chips
}

/// A resolved tag as the display layer needs it: name and accent token.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TagFace {
    /// The **whole path** — `fun/home`. Kept whole because a hover, a picker
    /// row and a group heading each want a different slice of it; a chip takes
    /// [`leaf`](Self::leaf).
    pub name: String,
    pub color: Option<String>,
}

impl TagFace {
    /// The last path segment, which is what a chip draws.
    pub fn leaf(&self) -> &str {
        koru_core::tags::leaf(&self.name)
    }
}

/// The workspace tag table, as pushed down by the host and read at draw time.
///
/// A cache, not an authority: the table lives in the workspace system
/// document, and the host replaces this snapshot whenever that document
/// changes (create, rename, merge, a peer's edit) — which is what makes a
/// rename repaint every chip without touching a document. An id the table
/// has forgotten resolves to `None` and the chip draws a quiet placeholder;
/// a peer can merge or lose a tag record concurrently, so that is an
/// ordinary state.
#[derive(Default)]
pub struct TagLookup {
    map: HashMap<TagId, TagFace>,
    /// Bumped on every replace, so a view can cheaply ask "did anything move".
    pub generation: u64,
}

impl TagLookup {
    pub fn replace(&mut self, tags: impl IntoIterator<Item = (TagId, TagFace)>) {
        self.map = tags.into_iter().collect();
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn resolve(&self, id: TagId) -> Option<&TagFace> {
        self.map.get(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Every known tag, sorted by name then id — the picker's listing order,
    /// the same two-key sort the system document's own `tags()` uses.
    pub fn sorted(&self) -> Vec<(TagId, TagFace)> {
        let mut out: Vec<(TagId, TagFace)> = self
            .map
            .iter()
            .map(|(id, face)| (*id, face.clone()))
            .collect();
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name).then(a.0.cmp(&b.0)));
        out
    }
}

/// The workspace list table, as pushed down by the host and read at draw time.
///
/// **[`TagLookup`]'s shape, and for its reason.** The lists live in the
/// workspace system document, which this document cannot see —
/// `WorkspaceDoc::set_list` says so in as many words — so the host replaces
/// this snapshot whenever that document changes. An id the table has forgotten
/// resolves to `None` and the chip draws neutral, exactly as a forgotten tag
/// does: a peer can delete a list concurrently, and that is an ordinary state
/// rather than an error.
///
/// It holds the **accent token** and nothing else. A list has a name too, and
/// the Tasks page draws it — but in a document the membership is carried by
/// the due chip's colour, so a name here would be a field nobody reads.
#[derive(Default)]
pub struct ListLookup {
    map: HashMap<ListId, String>,
    /// The table in the rail's order, name and swatch — what `/task_list`
    /// lists. Kept beside the map rather than instead of it: the chip's
    /// question is *what colour*, asked per row, and the picker's is *what
    /// is there*, asked once.
    listed: Vec<(ListId, String, Option<String>)>,
    /// Bumped on every replace, so a view can cheaply ask "did anything move".
    pub generation: u64,
}

impl ListLookup {
    pub fn replace(&mut self, lists: impl IntoIterator<Item = (ListId, Option<String>)>) {
        self.replace_named(
            lists
                .into_iter()
                .map(|(id, color)| (id, String::new(), color)),
        );
    }

    /// [`replace`](Self::replace), with the names the picker lists.
    pub fn replace_named(
        &mut self,
        lists: impl IntoIterator<Item = (ListId, String, Option<String>)>,
    ) {
        self.listed = lists.into_iter().collect();
        self.map = self
            .listed
            .iter()
            .filter_map(|(id, _, color)| color.clone().map(|c| (*id, c)))
            .collect();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Every list, in the rail's order: id, name, swatch.
    pub fn listed(&self) -> &[(ListId, String, Option<String>)] {
        &self.listed
    }

    /// This list's accent token, or `None` for a list with no colour **and**
    /// for one the table has forgotten.
    ///
    /// The two are deliberately one answer: both mean *draw the chip neutral*,
    /// and a caller that told them apart would have nothing different to do
    /// about it.
    pub fn resolve(&self, id: ListId) -> Option<&str> {
        self.map.get(&id).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// What one chip resolves to on screen: its label and its accent token.
/// `today` grounds the due chip's compressed text ("Today", "Thu") — passed
/// in so the rules stay pure functions, resolved once per pass above.
fn face_of<'a>(
    spec: &'a ChipSpec,
    tags: &'a TagLookup,
    lists: &'a ListLookup,
    list: Option<ListId>,
    queue: Option<Bucket>,
    today: NaiveDate,
) -> (ChipKind, String, Option<String>, Option<char>) {
    match spec {
        // **The leaf, not the path** ([ADR-185]): a block tagged `#fun/home`
        // draws `home`. The branch is how the tag is *filed*, which the picker
        // and the tag views show; on a row it would cost width on every chip to
        // repeat what the reader already chose. The whole path stays in
        // `TagFace::name` for a hover to print.
        //
        // Cheap by construction: the label is derived from a string the lookup
        // already holds, and `measure_attr_chip` keys the engine's chip cache
        // on it — so a shorter label is a *smaller* cache key, never a
        // per-frame measurement.
        ChipSpec::Tag(id) => match tags.resolve(*id) {
            Some(face) => (
                ChipKind::Tag,
                face.leaf().to_owned(),
                face.color.clone(),
                None,
            ),
            // A tag the table has forgotten: a quiet, unnamed chip rather
            // than nothing — the attribute is still set, and drawing nothing
            // would break the "set is visible" half of the invariant.
            None => (ChipKind::Tag, "…".to_owned(), None, None),
        },
        // Neutral tone, deliberately: no overdue tinting, no derived state —
        // a chip says what is set, and views own what that *means* today.
        // **The due chip wears its list's colour.** Membership is *set* state,
        // not derived — somebody wrote `koru.list` — so this is the chip
        // saying what is set, which is the rule it has always followed. The
        // accent lane was already in the cache key and already answers a
        // `(fill, ink)` pair from a token, so nothing about the recipe changes:
        // a task in a list draws the lozenge in that list's tone.
        //
        // A list with no colour, or one the table has forgotten, resolves to
        // `None` and the chip draws neutral — the same answer a forgotten tag
        // gets, for the same reason.
        //
        // **And it carries its queue's glyph.** A dated task is in exactly one
        // of the five, `bucket` says which, and the rail draws the same picture
        // beside the same word — so the chip stops being a date the reader has
        // to place for themselves and starts saying where the task *is*.
        ChipSpec::Due(Some(due)) => (
            ChipKind::Due,
            due_label(due, today),
            list.and_then(|id| lists.resolve(id)).map(str::to_owned),
            queue.map(bucket_glyph),
        ),
        // A task with no date: the same neutral lozenge, [`DUE_NONE`] where
        // the date would be.
        // The dateless chip too: membership does not depend on a date.
        //
        // **The lead, though, depends on the lifecycle and not on the date.**
        // An *open* dateless task is in Next or Later purely by the priority
        // chip sitting beside it, and an icon there would restate a neighbour —
        // so it gets none. A *completed* one gets the check-square, because
        // `bucket` returns `Completed` before it looks at the due at all:
        // `if lifecycle == Lifecycle::Completed { return Bucket::Completed; }`.
        //
        // That is not a nicety. `struck` is **not** in the cache key (§5 A), so
        // a completed dateless chip and an open one would share a key and differ
        // only in the strike — and the cache answers whichever was measured
        // first. Giving the completed one the check-square is what keeps the two
        // keys apart, and it is why this arm asks the queue rather than the date.
        ChipSpec::Due(None) => (
            ChipKind::Due,
            DUE_NONE.to_owned(),
            list.and_then(|id| lists.resolve(id)).map(str::to_owned),
            match queue {
                Some(Bucket::Completed) => Some(bucket_glyph(Bucket::Completed)),
                _ => None,
            },
        ),
        ChipSpec::Priority(p) => (
            ChipKind::Priority,
            priority_label(*p).to_owned(),
            None,
            None,
        ),
        ChipSpec::Color(token) => (ChipKind::Swatch, String::new(), Some(token.clone()), None),
    }
}

/// What a dateless task's due chip prints where a date would be.
///
/// **It was an en dash and it read as an empty box.** The reasoning behind the
/// dash was sound — the chip's job there is to say *this is a task* while
/// saying honestly that no deadline is set, and `docs/features/design/DESIGN.md` asks
/// the quiet reading of every chip — but one en dash inside a lozenge at chip
/// size is not quiet, it is blank. Acceptance reported it as *an empty pill
/// stayed* after clearing a due, and reading a state as *nothing happened* is
/// worse than reading it loudly.
///
/// So it says what it means. The words rather than a glyph, because this chip
/// already speaks: the ones beside it read *Today* and *Thu*, and a symbol here
/// would be the one due chip a reader has to learn.
///
/// **Not localized, and consistently so.** Every word this chip family prints
/// is an English literal today — `due_date_label`'s *Today*, `weekday_abbr`,
/// `month_abbr`, `priority_label`'s levels. Translating this one string alone
/// would be a second mechanism for one word; the gap is the vocabulary's, and
/// it is uniform.
///
/// A constant rather than a literal at the one call site, because the tests
/// assert on it and a label that drifted from what they name would pass.
pub const DUE_NONE: &str = "No date";

/// The due chip's text, **one rule per temporal type** ([ADR-183]).
///
/// - A **day** prints as a day. No clock, because there is none.
/// - A **floating** wall clock prints its own reading. It is the same clock
///   reading wherever it is read, so no conversion happens and none should:
///   converting it would be inventing a place the value does not name.
/// - A **zoned** deadline prints in *this machine's* zone — a **display**
///   choice, which is a different thing from the zone the value means. The
///   value's own zone is untouched; a reader who flies changes what this chip
///   says and changes nothing about the deadline (invariant 24).
///
/// A display-zone preference, when one exists, replaces the zone passed here
/// and nothing else.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
pub fn due_label(due: &Temporal, today: NaiveDate) -> String {
    due_label_in(due, today, &crate::utils::zone::SystemZone::detect())
}

/// [`due_label`] against an explicit display zone — the seam a preference
/// would use, and what the tests drive.
pub fn due_label_in(due: &Temporal, today: NaiveDate, display: &SystemZone) -> String {
    match due {
        Temporal::Date(d) => dates::due_date_label(d.date(), today),
        Temporal::Floating(wall) => dates::due_instant_label(wall.wall(), today),
        Temporal::Zoned(zoned) => {
            dates::due_instant_label(display.display_wall(zoned).wall(), today)
        }
    }
}

/// **A queue's glyph, named once for the whole app.**
///
/// The rail draws it beside the queue's word; a task's due chip draws it inside
/// the lozenge. They are one picture for one queue, and the moment they were
/// two tables they were free to disagree — which is the failure this exists to
/// make impossible, not a tidiness argument.
///
/// **It lives here rather than in `koru-draw` or in `koru-core`.** The
/// codepoints are `koru-draw`'s and the enum is `koru-core`'s, and neither
/// crate can see the other: `koru-draw` has no `koru-core` dependency, and
/// the domain crate knows nothing about Phosphor and must not learn. This
/// module is the first place above both that holds them at once, and it is
/// also the place with the reason to — the chip is what forced the question.
///
/// The set is the owner's: a sun for today, an arrow for what is next, a double
/// caret for what is coming, a countdown for what waits, and the check-square
/// every completed thing in this app already wears.
pub fn bucket_glyph(bucket: Bucket) -> char {
    use koru_draw::icons;
    match bucket {
        Bucket::Today => icons::SUN,
        Bucket::Next => icons::ARROW_RIGHT,
        Bucket::Upcoming => icons::CARET_DOUBLE_RIGHT,
        Bucket::Later => icons::CLOCK_COUNTDOWN,
        Bucket::Completed => icons::CHECK_SQUARE,
    }
}

/// Which queue this row's task falls in, or `None` when the row is not a task.
///
/// **Asked of `koru_core::tasks::bucket`, never worked out here.** That
/// function is SPEC {S}7's rule and the projection store's placement is held
/// equal to it by `bucket_placement_agrees_with_the_domain_function`; a second
/// comparison against today written in a drawing pass is a second answer, and
/// the copy that used to live in `taskcounts.rs` had already drifted by four
/// days before anybody noticed.
///
/// Resolved **once per row** rather than once per chip: lifecycle is a row-level
/// fact and priority is a sibling chip's, so every chip on the row would get the
/// same answer and asking four times would call `bucket` four times for it.
fn row_bucket(row: &Paragraph, today: NaiveDate) -> Option<Bucket> {
    // The due chip is the task indicator (SPEC {S}4), so its presence is the
    // question "is this a task" already answered by `attribute_chips`.
    let due = row.chips.iter().find_map(|spec| match spec {
        ChipSpec::Due(due) => Some(due.as_ref()),
        _ => None,
    })?;
    let priority = row.chips.iter().find_map(|spec| match spec {
        ChipSpec::Priority(p) => Some(*p),
        _ => None,
    });
    // The **canonical** lifecycle, not the `done` mirror — SPEC {S}5 lets the two
    // disagree on a task with children, and `bucket` is written against the
    // canonical one.
    let lifecycle = match row.task {
        Some(task) if task.completed => Lifecycle::Completed,
        _ => Lifecycle::Open,
    };
    let today = koru_core::time::DateOnly::new(today);
    Some(koru_core::tasks::bucket(
        due,
        lifecycle,
        priority,
        today,
        &SystemZone::detect(),
    ))
}

/// The level's display name — the chip's whole text. The `P1`-style
/// abbreviation is a tracked-view concern (R2), not this chip's.
pub fn priority_label(p: Priority) -> &'static str {
    match p {
        Priority::Urgent => "Urgent",
        Priority::High => "High",
        Priority::Medium => "Medium",
        Priority::Low => "Low",
    }
}

/// One placed chip: where it is, what it draws, and — for a tag — where its
/// remove `×` is.
///
/// A struct rather than the tuple this was, because the `×` made it five
/// things and a `.4` at a call site says nothing about which.
#[derive(Clone, PartialEq, Debug)]
pub struct ChipBox {
    /// The whole chip, in the coordinates [`chip_boxes`] answers in.
    pub rect: Rect,
    /// The remove affordance's lane inside [`rect`](Self::rect) — `Some` on
    /// **every** chip kind ([ADR-188], amended 2026-08-27), with one
    /// exception: a **dateless task's** due chip — [`ChipSpec::Due`] holding
    /// `None` — which has nothing to clear. Its date is already unset, and the
    /// thing an `×` there would plausibly mean — take this off the task list —
    /// is a destructive change of state with its own confirmed menu row
    /// (SPEC §6), not a lane on a chip. So the dash chip is all label:
    /// pressing anywhere on it opens the due picker.
    ///
    /// It was the tag chip's alone, on the argument that the other three hold
    /// one value each and are cleared through their own picker's Clear row.
    /// That made the chip a control for setting an attribute but not for
    /// taking it off, and the difference was invisible: the only way to find
    /// out which chips could be removed in place was to hover each one. The
    /// Clear rows stay — they are the keyboard's reach — so there are two ways
    /// to clear one value, which is fine because both say the same thing.
    ///
    /// On a **swatch** the lane sits to the right of the dot; on the three
    /// labelled lozenges it sits inside the lozenge, after the label.
    ///
    /// **The lane is reserved whether or not the `×` is inked.** Its width is
    /// part of the chip's width always, so hovering one cannot make the row's
    /// chips shuffle sideways; what hover changes is only whether the glyph is
    /// painted. Geometry is therefore the single authority for the hit test,
    /// and a press can only land here while the pointer is inside the chip —
    /// which is exactly when the `×` is showing.
    ///
    /// [ADR-188]: ../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
    pub close: Option<Rect>,
    pub kind: ChipKind,
    pub label: String,
    pub accent: Option<String>,
    /// The glyph inside the lozenge, before the label — a due chip's queue.
    ///
    /// **Width-bearing, and therefore part of the cache key.** It adds a lane,
    /// so the paint has to ask the cache for the entry the measurement built or
    /// the chip would paint short of where it is pressable. `None` on every kind
    /// but [`ChipKind::Due`], and on an open dateless one.
    pub lead: Option<char>,
    /// The label is struck — a completed task's due.
    pub struck: bool,
}

impl ChipBox {
    /// This chip as **the one chip component** (`koru_ui::label_chip`): the
    /// attribute chip its kind earns, its lead and strike, and the remove
    /// lane — a chip in a document is the control that takes its attribute
    /// off. Measuring, hit-testing and painting all ask this, so the three
    /// cannot disagree about what the chip is.
    pub fn face(&self) -> LabelChip<'_> {
        face(
            self.kind,
            &self.label,
            self.accent.as_deref(),
            self.lead,
            self.struck,
        )
    }
}

/// The chip component for one attribute chip — see [`ChipBox::face`].
fn face<'a>(
    kind: ChipKind,
    label: &'a str,
    accent: Option<&'a str>,
    lead: Option<char>,
    struck: bool,
) -> LabelChip<'a> {
    LabelChip::attribute(kind, label, accent)
        .lead(lead.map_or(Lead::None, Lead::Glyph))
        .struck(struck)
        .trail(Trail::Remove)
}

/// Where a row's chips sit, in document coordinates — **the** geometry
/// function, shared by drawing and by hit-testing. Chips trail the text: same
/// line as the layout's last line, starting one [`CHIP_GAP`] after its advance.
///
/// Measuring goes through the engine's chip cache, so a frame that has drawn
/// these labels once shapes nothing.
pub fn chip_boxes(
    row: &Paragraph,
    engine: &mut TextEngine,
    tags: &TagLookup,
    lists: &ListLookup,
) -> Vec<ChipBox> {
    // The one place the wall clock enters chip drawing: resolved here, once
    // per row per pass, and handed down — so the label rules stay pure and
    // a chip that says "Today" corrects itself on the first paint after
    // midnight (the spec caches the value, never the text).
    chip_boxes_on(row, engine, tags, lists, chrono::Local::now().date_naive())
}

/// [`chip_boxes`] against an explicit `today`, for tests.
pub fn chip_boxes_on(
    row: &Paragraph,
    engine: &mut TextEngine,
    tags: &TagLookup,
    lists: &ListLookup,
    today: NaiveDate,
) -> Vec<ChipBox> {
    if row.chips.is_empty() {
        return Vec::new();
    }
    let last = row.layout.lines().last();
    let (line_top, line_h, advance) = match last {
        Some(line) => {
            let m = line.metrics();
            (
                m.block_min_coord,
                m.block_max_coord - m.block_min_coord,
                m.advance,
            )
        }
        None => (0.0, CHIP_H, 0.0),
    };
    let mut x = row.text_x() + advance + CHIP_GAP;
    let y = row.y + line_top + (line_h - CHIP_H) * 0.5;
    // **Once for the row, not once per chip.** The lifecycle is a row-level
    // fact and the priority is a sibling chip's, so all four chips would get
    // the same answer; asking inside the loop would call `bucket` up to four
    // times to learn it. `today` is hoisted one level further out for this
    // reason already.
    let queue = row_bucket(row, today);
    let mut out = Vec::new();
    for spec in &row.chips {
        let (kind, label, accent, lead) = face_of(spec, tags, lists, row.list, queue, today);
        // A completed task's chip is struck. It rides the *measurement* because
        // the visual carries it, and it is safe to keep out of the cache key
        // **only** because every completed due chip also carries the
        // check-square — see `face_of`'s dateless arm, which is what makes that
        // true.
        let struck = lead == Some(bucket_glyph(Bucket::Completed));
        // The measured label (or, for a swatch, its dot), plus the remove lane
        // every chip here carries beyond it — and the lead's lane, which is
        // inside the lozenge and therefore inside the measurement.
        let chip = face(kind, &label, accent.as_deref(), lead, struck);
        let w = chip.width(engine);
        let rect = Rect::new(x, y, w, CHIP_H);
        let close = chip.trail_rect(rect);
        out.push(ChipBox {
            rect,
            // **Every chip offers its lane, the dateless task included.**
            //
            // It did not, once, and the reasoning was sound while the `×`
            // meant *empty this value*: a task with no date has nothing to
            // clear, so a lane there would have been a button that did
            // nothing. The due chip's `×` now removes the **task**, which a
            // dateless task has as much of as a dated one — so the lane is
            // always there, and the one chip that used to hide it is the one
            // that most needs it.
            close,
            kind,
            label,
            accent,
            lead,
            struck,
        });
        x += w + CHIP_GAP;
    }
    out
}

/// What a pointer found on a row's chips.
#[derive(Clone, PartialEq, Debug)]
pub struct ChipHit {
    /// Index into `row.chips` — which is what decides *which* picker opens.
    pub index: usize,
    pub spec: ChipSpec,
    /// The whole chip, for a picker to hang off.
    pub rect: Rect,
    /// The point was in the chip's remove lane. Every kind has one.
    pub on_close: bool,
}

/// The chip under a point, in the same coordinates [`chip_boxes`] answers in —
/// content-column x, document y.
///
/// **The one hit-test, over the one geometry function.** Hover and click both
/// come through here, so the hover treatment, the `×` and the picker cannot
/// disagree about where a chip is; the alternative — a second copy of the
/// arithmetic for input — is exactly what `date_picker`'s `cell_at` exists to
/// avoid.
///
/// Measuring is a hashmap lookup on a warm cache: `chip_boxes` keys the
/// engine's chip cache on the label, and the frame that drew this row filled
/// it. Nothing here shapes text a second time.
pub fn chip_at(
    row: &Paragraph,
    engine: &mut TextEngine,
    tags: &TagLookup,
    lists: &ListLookup,
    x: f32,
    doc_y: f32,
) -> Option<ChipHit> {
    // **The same table the paint is given.** The accent changes no width, so a
    // lookup that reached only the paint would leave geometry correct and split
    // the cache in two — the hit test and the paint computing different entries
    // for one chip, free to diverge the day an accent does affect a width.
    chip_boxes(row, engine, tags, lists)
        .into_iter()
        .enumerate()
        .find(|(_, placed)| placed.rect.contains(x, doc_y))
        .map(|(index, placed)| ChipHit {
            index,
            spec: row.chips[index].clone(),
            rect: placed.rect,
            on_close: placed.close.is_some_and(|r| r.contains(x, doc_y)),
        })
}

/// Paint a row's chips. `ox`/`oy` translate document coordinates to the
/// screen the way every other row element does — note `oy` already includes
/// the scroll, while [`chip_boxes`] answers in document y.
///
/// `hovered` is the index of the chip the pointer is on, if it is on one of
/// this row's: a chip's `×` is inked only there. The lane it sits in is
/// reserved either way, so nothing moves when the pointer arrives — a
/// permanent `×` beside every chip is an affordance on top of content that is
/// mostly not being edited, and `docs/features/design/DESIGN.md` asks for the quiet reading.
pub fn draw_row_chips(
    scene: &mut vello::Scene,
    root: vello::kurbo::Affine,
    row: &Paragraph,
    engine: &mut TextEngine,
    tags: &TagLookup,
    lists: &ListLookup,
    oy: f32,
    hovered: Option<usize>,
) {
    for (i, placed) in chip_boxes(row, engine, tags, lists).into_iter().enumerate() {
        let rect = Rect::new(
            placed.rect.x,
            oy + (placed.rect.y - row.y),
            placed.rect.w,
            placed.rect.h,
        );
        placed.face().draw(
            scene,
            root,
            engine,
            rect,
            State {
                hovered: false,
                reveal: placed.close.is_some() && hovered == Some(i),
            },
        );
    }
}

#[cfg(test)]
mod tests;
