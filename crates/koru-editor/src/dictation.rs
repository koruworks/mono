// SPDX-License-Identifier: MPL-2.0

//! Dictating into a document: an ephemeral composition over one text node.
//!
//! Voice is **an ephemeral composition routed to the focused editable
//! surface**, committed as one final logical edit and one undo step
//! ([ADR-248](../../../docs/decisions/ADR-226-250.md#adr-248-voicecomposition--dictation-is-an-ephemeral-composition-and-only-the-final-transcript-is-a-transaction)).
//! This is the document half of that contract; `koru_ui::text_field` is the
//! one-line half, and [`koru_ui::dictation::DictationTarget`] is the trait
//! both answer.
//!
//! ## What is stored, and where
//!
//! [`Composition`] is a [`StableTextRange`] and a string, and it lives on
//! [`Body`] rather than on `ViewState`. That is a deliberate departure from the
//! roadmap's sketch, and the reason is the one thing this module cannot get
//! wrong: **the provisional run has to be re-spliced after every row re-read**,
//! and a row is re-read from three places (`rebuild`, `rebuild_reusing`,
//! `refresh`) that are called from dozens more. `Body` is the type that owns
//! the rows *and* is handed the `WorkspaceDoc` at every one of those points, so
//! putting the composition there makes re-splicing a hook inside the type that
//! rebuilds rather than an obligation on every caller that triggers one.
//! `ViewState` has neither the rows nor the document, so the same state kept
//! there would be re-applied by whoever remembered to. See Q-V37.
//!
//! It is still transient view state that Loro never sees — the same category as
//! `ViewState::search_flash` — and **zero Loro operations happen until commit**.
//!
//! ## The provisional text is spliced into the row
//!
//! While a composition is open, [`Paragraph::text`](crate::Paragraph::text) is
//! the **composed** string: the authored text with the replaced range swapped
//! for the transcript so far. It has to be, because a row's parley `Layout`,
//! its caret, its hit-testing, its wrapping and its selection are all indexed
//! into `row.text`, and a second string alongside it would mean a second copy
//! of all of that. `row.composition` records the splice so the two coordinate
//! spaces can be told apart — see [`Mapped`].
//! After every splice the caret is queued at the transcript's trailing edge;
//! retaining the authored caret would make streaming words grow in front of a
//! caret left behind at the start of the utterance.
//!
//! What makes that safe is the lifecycle rule, not luck: **an ordinary local
//! event commits the composition and is then re-dispatched**, so no editing
//! path ever runs while `row.text` is not the document's text. Lifecycle events
//! — window close, focus loss, sleep, shutdown — discard instead.

use std::ops::Range;

use koru_core::NodeId;
use koru_core::schema::{StableTextRange, WorkspaceDoc};
use koru_core::text::{TextRun, Utf8Pos};
use koru_draw::geometry::Rect;

use crate::body::Body;
use crate::view_state::ViewState;

/// An open dictation: what it will replace, and what has been heard so far.
///
/// **Both endpoints are Loro cursors.** Dictation replaces a selection, and one
/// stable end plus a raw byte offset is stale the moment a peer edits inside or
/// before that selection. [`StableTextRange`] deliberately does not normalise
/// direction, so a backwards selection comes back backwards; the direction is
/// preserved on every resolve and normalised only for the replacement itself.
/// A collapsed range is an insertion at a caret — there is no second case.
#[derive(Clone, Debug)]
pub struct Composition {
    replacing: StableTextRange,
    text: String,
    /// `Some` identifies native input; its inner `None` hides the caret.
    pub(crate) native_cursor: Option<Option<(usize, usize)>>,
    pub(crate) armed: (Option<koru_core::MarkSet>, Option<koru_core::TextStyle>),
}

impl Composition {
    /// The node being dictated into. Both endpoints name it —
    /// `resolve_text_range` refuses a range that crosses nodes — so either one
    /// answers.
    pub fn node(&self) -> NodeId {
        self.replacing.anchor.node()
    }

    /// The transcript so far. Empty until the first partial arrives.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
    }

    /// The range this dictation will replace, **directed**: `(anchor, focus)`,
    /// backwards if that is how it was selected. `None` when either endpoint no
    /// longer resolves, which is how a deleted node is discovered.
    pub fn resolve(&self, ws: &WorkspaceDoc) -> Option<(usize, usize)> {
        ws.resolve_text_range(&self.replacing).ok()
    }

    /// The same range, normalised — what the replacement itself needs.
    pub fn range(&self, ws: &WorkspaceDoc) -> Option<Range<usize>> {
        let (a, f) = self.resolve(ws)?;
        Some(a.min(f)..a.max(f))
    }
}

/// The splice a row is carrying: which authored bytes are hidden, and what is
/// standing in for them.
///
/// A cache of [`Composition`] projected onto one row, rebuilt from it whenever
/// the row is re-read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionalRun {
    /// The authored byte range this run stands in for, normalised. Empty for an
    /// insertion at a caret.
    pub authored: Range<usize>,
    /// The provisional text now occupying it.
    pub text: String,
}

impl ProvisionalRun {
    /// Where the provisional text sits in the composed layout.
    pub fn layout_range(&self) -> Range<usize> {
        self.authored.start..self.authored.start + self.text.len()
    }

    /// How much longer the composed text is than the authored text.
    fn delta(&self) -> isize {
        self.text.len() as isize - (self.authored.end - self.authored.start) as isize
    }
}

/// Which side of a hidden range a position was reported at.
///
/// Deliberately not `parley::layout::Affinity`: that one is about which of two
/// clusters a caret belongs to, and this one is about which end of a replaced
/// range an authored index was rounded to. Two different questions that would
/// share a name and never a meaning.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Affinity {
    /// Rounded back to where the replaced range begins.
    Leading,
    /// Rounded forward to where the replaced range ends.
    Trailing,
}

/// Which coordinate space an index inhabits once the answer is known.
///
/// **[`Paragraph::doc_pos`](crate::Paragraph::doc_pos) and
/// [`Paragraph::layout_pos`](crate::Paragraph::layout_pos) are not two total
/// inverses, and they cannot be.** A composition *replaces* a range: authored
/// positions strictly inside it have no place in the composed layout, and
/// positions strictly inside the provisional run have no authored position —
/// several of them collapse onto one boundary. So the caller is handed which
/// case it is in rather than a plausible number.
///
/// With no composition open both functions answer `Authored(i)` for every `i`,
/// and the pair is the identity.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mapped {
    /// A real position in the space that was asked about, and the inverse of it
    /// exists.
    Authored(usize),
    /// Inside the provisional run: it has no authored position. The value is
    /// the offset **within the transcript**, so a caller placing something
    /// against what was heard still can. Only
    /// [`Paragraph::doc_pos`](crate::Paragraph::doc_pos) produces it — the
    /// layout has no hidden bytes.
    Provisional(usize),
    /// Inside the replaced range: hidden, and reported at the nearer boundary
    /// **in layout coordinates** with an explicit affinity rather than silently
    /// rounded. Only [`Paragraph::layout_pos`](crate::Paragraph::layout_pos)
    /// produces it — the transcript hides nothing of itself.
    Hidden { boundary: usize, affinity: Affinity },
}

impl Mapped {
    /// The index, whichever space it turned out to be in. For a caller that has
    /// already decided all three answers are acceptable — a scroll target, a
    /// debug print — and never for one placing a caret.
    pub fn index(self) -> usize {
        match self {
            Self::Authored(i) | Self::Provisional(i) => i,
            Self::Hidden { boundary, .. } => boundary,
        }
    }

    /// Whether this is a real position in the space that was asked about.
    pub fn is_authored(self) -> bool {
        matches!(self, Self::Authored(_))
    }
}

/// Map a layout index back into the authored text. See [`Mapped`].
pub(crate) fn doc_pos(run: Option<&ProvisionalRun>, layout: usize) -> Mapped {
    let Some(run) = run else {
        return Mapped::Authored(layout);
    };
    let span = run.layout_range();
    if layout <= span.start {
        Mapped::Authored(layout)
    } else if layout < span.end {
        Mapped::Provisional(layout - span.start)
    } else if layout == span.end {
        // The far edge of the transcript is exactly the far edge of the range
        // it replaced. That is an authored position, and it is the one the
        // caret sits at after every partial.
        Mapped::Authored(run.authored.end)
    } else {
        Mapped::Authored((layout as isize - run.delta()) as usize)
    }
}

/// Map an authored index into the composed layout. See [`Mapped`].
pub(crate) fn layout_pos(run: Option<&ProvisionalRun>, doc: usize) -> Mapped {
    let Some(run) = run else {
        return Mapped::Authored(doc);
    };
    if doc <= run.authored.start {
        Mapped::Authored(doc)
    } else if doc >= run.authored.end {
        Mapped::Authored((doc as isize + run.delta()) as usize)
    } else {
        let span = run.layout_range();
        // The nearer boundary wins and the affinity says which, so a caller can
        // tell "it was at the front of what is being replaced" from "it was at
        // the back" rather than finding two authored bytes reporting one
        // number with nothing to separate them.
        if doc - run.authored.start <= run.authored.end - doc {
            Mapped::Hidden {
                boundary: span.start,
                affinity: Affinity::Leading,
            }
        } else {
            Mapped::Hidden {
                boundary: span.end,
                affinity: Affinity::Trailing,
            }
        }
    }
}

/// Re-range formatting runs across the splice.
///
/// A run wholly before the hole is untouched, one wholly after is shifted, and
/// one that straddles is clipped to the parts that survive. **A pill run that
/// touches the hole is dropped**: its anchor is one of the bytes being
/// replaced, so the pill is going away at commit and there is nothing for it to
/// hang on in the meantime.
pub(crate) fn splice_runs(
    runs: &[TextRun],
    hole: &Range<usize>,
    provisional: usize,
) -> Vec<TextRun> {
    let delta = provisional as isize - (hole.end - hole.start) as isize;
    let shift = |i: usize| (i as isize + delta) as usize;
    let mut out = Vec::with_capacity(runs.len() + 1);
    for run in runs {
        let (s, e) = (run.start.get(), run.end.get());
        if e <= hole.start {
            out.push(run.clone());
            continue;
        }
        if s >= hole.end {
            let mut next = run.clone();
            next.start = Utf8Pos(shift(s));
            next.end = Utf8Pos(shift(e));
            out.push(next);
            continue;
        }
        if run.pill.is_some() {
            continue;
        }
        if s < hole.start {
            let mut head = run.clone();
            head.end = Utf8Pos(hole.start);
            head.text = run.text.get(..hole.start - s).unwrap_or("").to_owned();
            out.push(head);
        }
        if e > hole.end {
            let mut tail = run.clone();
            tail.start = Utf8Pos(shift(hole.end));
            tail.end = Utf8Pos(shift(e));
            tail.text = run.text.get(hole.end - s..).unwrap_or("").to_owned();
            out.push(tail);
        }
    }
    out
}

/// Whether this view accepts dictation at all.
///
/// A read-only view refuses — which is also what a trashed document is, since
/// the container puts one into `read_only` when it shows the *In Trash* strip.
pub fn accepts(view: &ViewState) -> bool {
    !view.read_only
}

/// Open a composition over the focused row's selection, or at its caret when
/// the selection is collapsed — **dictation replaces a selection exactly as
/// typing does**.
///
/// `Body::is_title_row` is not a barrier: the title is an ordinary row for
/// typing, and dictating a document's name is the same edit as typing it.
///
/// Answers whether one is now open.
pub fn begin(body: &mut Body, view: &ViewState, ws: &mut WorkspaceDoc) -> bool {
    begin_source(body, view, ws, false)
}

pub(crate) fn begin_native(body: &mut Body, view: &ViewState, ws: &mut WorkspaceDoc) -> bool {
    begin_source(body, view, ws, true)
}

fn begin_source(body: &mut Body, view: &ViewState, ws: &mut WorkspaceDoc, native: bool) -> bool {
    if !accepts(view) || body.composition().is_some() {
        return false;
    }
    let Some(row) = body.focused() else {
        return false;
    };
    // An image row carries no text to compose into.
    if row.image.is_some() {
        return false;
    }
    let id = row.id;
    // Directed, so a backwards selection is captured backwards and comes back
    // that way after any number of peer edits. A collapsed caret is captured as
    // an empty range, which is an insertion and needs no second case.
    let caret = row.caret();
    let (anchor, focus) = row.directed_range().unwrap_or((caret, caret));
    let Ok(replacing) = ws.capture_text_range(id, anchor, focus) else {
        return false;
    };
    let armed = row.armed();
    body.open_composition(Composition {
        replacing,
        text: String::new(),
        native_cursor: native.then_some(None),
        armed,
    });
    body.refresh(ws, id);
    true
}

/// Replace the provisional text. Called at most about ten times a second.
///
/// **No Loro operation, ever** — the whole point of the composition is that a
/// revision is not an edit. Only the composed row is marked dirty, so a partial
/// costs what a keystroke into the same row costs.
pub fn update(body: &mut Body, ws: &mut WorkspaceDoc, text: &str) {
    let Some(current) = body.composition() else {
        return;
    };
    if current.native_cursor.is_some() {
        return;
    }
    // A recogniser repeats itself. Re-reading and re-shaping a row to arrive at
    // the layout already on screen is the cheapest thing not to do.
    if current.text == text {
        return;
    }
    let id = current.node();
    body.set_composition_text(text);
    body.refresh(ws, id);
}

/// Close the composition by making `text` real: **one logical edit and one undo
/// step**.
///
/// It is deliberately *not* one Loro transaction and not one sync message:
/// [`WorkspaceDoc::replace_text`] wraps `atomic`, which groups undo, and the
/// `delete_text`/`insert_text` inside it each commit. Two commits, and nothing
/// anybody can observe cares.
///
/// An empty final transcript **cancels**. A dictation that produced no text
/// records no step at all — the rule a backspace at offset zero already
/// follows — and the alternative would let saying nothing over a selection
/// delete it.
pub fn commit(body: &mut Body, ws: &mut WorkspaceDoc, text: &str) {
    if text.is_empty() {
        cancel(body, ws);
        return;
    }
    let Some(open) = body.composition() else {
        return;
    };
    if open.native_cursor.is_some() {
        return;
    }
    let id = open.node();
    // Resolved *after* any import that has already landed: the endpoints are
    // cursors, so this is the converged range rather than the one captured.
    let Some(range) = open.range(ws) else {
        // The node went away underneath the dictation. Discard; there is
        // nowhere for the words to go that is not somebody else's text.
        body.close_composition();
        body.rebuild(ws);
        return;
    };
    // The row must be authored again before the write, or the refresh below
    // would splice the transcript back over the text it has just become.
    body.close_composition();
    // The person has been typing; end that group, so one Ctrl+Z takes the
    // dictation back and not the sentence before it as well.
    ws.checkpoint();
    let caret = match ws.replace_text(id, range, text) {
        Ok(caret) => caret,
        Err(_) => {
            body.refresh(ws, id);
            return;
        }
    };
    ws.checkpoint();
    body.focus_on(id);
    body.refresh(ws, id);
    if let Some(row) = body.row_mut(id) {
        row.place_caret(caret);
    }
}

/// Close the composition by throwing it away: no edit, no undo entry, no trace.
///
/// What an interruption does — window close, focus loss, sleep, shutdown —
/// because half a sentence landing in a document nobody was looking at is worse
/// than losing it.
pub fn cancel(body: &mut Body, ws: &mut WorkspaceDoc) {
    let Some(open) = body.composition() else {
        return;
    };
    if open.native_cursor.is_some() {
        return;
    }
    let id = open.node();
    body.close_composition();
    body.refresh(ws, id);
}

/// Where the provisional text is, **in view space** — the same coordinates
/// `Editor::selection_rects` answers in, which is what a leaf's own overlays
/// are placed with. A chrome field answers in a different space; see the
/// [`DictationTarget`](koru_ui::dictation::DictationTarget) module note.
///
/// The **union** of the run's rectangles, because a transcript inserted into a
/// bidi line is two visual pieces of one byte range and one that wraps is two
/// more. `None` when nothing is being composed, and while the transcript is
/// still empty and therefore covers no glyphs.
pub fn rect(body: &Body, view: &ViewState) -> Option<Rect> {
    let open = body.composition()?;
    let row = body.row(open.node())?;
    let span = row.composition.as_ref()?.layout_range();
    if span.is_empty() {
        return None;
    }
    let ox = view.content_x + row.text_x();
    let oy = row.y - view.scroll;
    let mut bounds: Option<(f32, f32, f32, f32)> = None;
    row.layout_selection(span)
        .geometry_with(&row.layout, |bb, _| {
            let (x0, y0) = (bb.x0 as f32 + ox, bb.y0 as f32 + oy);
            let (x1, y1) = (bb.x1 as f32 + ox, bb.y1 as f32 + oy);
            bounds = Some(match bounds {
                None => (x0, y0, x1, y1),
                Some((ax0, ay0, ax1, ay1)) => (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1)),
            });
        });
    bounds.map(|(x0, y0, x1, y1)| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ime_tests;
