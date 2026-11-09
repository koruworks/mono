// SPDX-License-Identifier: MPL-2.0

//! The chat's composer: **a Koru document in a private in-memory workspace,
//! edited by `koru-editor`** (AGENTUX I6, decision X1).
//!
//! It exists so a person can type `@`, pick a document, and have the document
//! sit *in the text* as a chip — the editor's own pill, drawn and edited the
//! way it is in any page. At send, each chip becomes a pinned `explicit_ref`
//! and the text goes out through `koru-export-markdown`, whose
//! `[[koru://doc/<id>]]` the conversation draws as a titled link
//! ([`crate::doc_links`]).
//!
//! One module below the chat hosts — the embedded pane and tab in
//! `apps/koru` — so no host repeats what `MainView` does for a document leaf: the private workspace,
//! the [`Editor`], the entity search behind `@`, and the loop that drains the
//! picker's requests and delivers its answers.
//!
//! ## What the private document is allowed to be
//!
//! The editor runs under [`Policy::Composer`]: text, line breaks, undo and the
//! `@` picker, and nothing that reshapes a row — no heading from `#`, no list
//! from `- `, no `/` menu, no marks, no new page. A line break is a new plain
//! paragraph (a paragraph may not hold `\n`), and [`ChatComposer::message`]
//! joins the paragraphs with one newline, so what is sent reads exactly as the
//! one-block field this replaced sent it.
//!
//! ## Quotes
//!
//! A passage of a reply added to the message (koru-ai D88) is a **quote
//! chip** on a line of its own — the editor's pill under
//! [`koru_core::attributes::ROLE_QUOTE`], holding the passage whole — with
//! the person's comment on the lines under it. Several interleave with their
//! comments in the order they were added. At send each is a Markdown
//! blockquote before the text that follows it
//! (`koru_export_markdown::message_text`); a recalled or queued message
//! brings its quotes back as chips.
//!
//! ## Agent skills and commands
//!
//! A skill of the chat's agent picked under `$` (koru-ai D121, [`crate::skills`])
//! is a **skill chip** where the `$` was typed — the editor's pill under
//! [`koru_core::attributes::ROLE_SKILL`], holding the skill's name. At send
//! its selected name is stored as a durable skill link and in
//! [`ComposedMessage::skills`]. A command selected under `/` is a command
//! pill and link at the same position. Recall restores both kinds as pills;
//! typed `$name` and `/name` words keep their literal meaning.
//!
//! ## Where it is drawn
//!
//! The editor believes it owns a viewport with its own margins; the chat gives
//! it a few lines inside a rounded box. The two meet through
//! [`koru_editor::Embedded`]: the editor is placed so its first text column
//! and first line land on the box's inner edge, its rows are clipped to the
//! box, and the `@` picker is allowed to float above the box over the
//! conversation. Everything here converts between the chat's coordinates
//! ("host") and the editor's ("view") at that one offset.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use koru_core::node::{Attr, AttrValue, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{NodeId, WorkspaceId};
use koru_draw::ImageCache;
use koru_draw::geometry::Rect;
use koru_draw::theme;
use koru_editor::components::Overlay;
use koru_editor::components::entity_picker::{DocumentChoice, HeadingChoice, WorkspaceChoice};
use koru_editor::draw::{Frame, frame};
use koru_editor::input::action::Action;
use koru_editor::trigger::DocumentTarget;
use koru_editor::{Editing, Editor, Embedded, Policy};
use koru_search_ui::{EntitySearch, EntityWorkspace};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};

/// How far inside its box the composer's text sits: the chat's own padding
/// ([`crate::COMPOSER_PAD_X`], [`crate::COMPOSER_PAD_TOP`]) and, below the
/// last line, the gap to the toolbar row ([`crate::COMPOSER_TEXT_GAP`]).
/// **Named here, defined there**, so the box the chat measures and the inset
/// the editor is placed at cannot disagree.
const PAD_X: f32 = crate::COMPOSER_PAD_X;
const PAD_TOP: f32 = crate::COMPOSER_PAD_TOP;
const PAD_BOTTOM: f32 = crate::COMPOSER_TEXT_GAP;

/// How often an open picker waiting on the store is polled.
const POLL: Duration = Duration::from_millis(16);

/// The editor's own margin to the left of the first text column, in view
/// coordinates: the gutter plus the affordance lane (`Paragraph::text_x`).
fn side() -> f32 {
    theme::MARGIN_X + theme::AFFORDANCE_W
}

/// One line of body text, the unit the composer grows by.
///
/// The composer's body is set in the app's face ([`fresh`]), so its box and
/// its text are measured from the same number whatever the document size is.
fn line_height() -> f32 {
    crate::chat_line_height()
}

/// What a composed message is at send.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComposedMessage {
    /// The text as typed, chips as `[[koru://doc/<id>]]`, paragraphs joined by
    /// one newline.
    pub markdown: String,
    /// Every chip's document, in reading order, each once, with the caption it
    /// was chosen under. A heading chip names its whole document here: until
    /// UC15 a turn sends the whole document.
    pub documents: Vec<(koru_ai_core::DocumentId, String)>,
    /// Every skill chip's name (koru-ai D121), in reading order, each once;
    /// Markdown keeps its selected-link identity until prompt rendering.
    pub skills: Vec<String>,
}

/// **A `$` being typed** (koru-ai D121): where it is, and what follows it up
/// to the caret — the query the skill list is filtered by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DollarToken {
    pub(crate) row: NodeId,
    pub(crate) marker: char,
    /// The `$`'s byte offset in the row's text.
    pub(crate) start: usize,
    /// The caret's: where the token ends.
    pub(crate) end: usize,
    /// What follows the `$`.
    pub(crate) query: String,
}

/// Whether `character` can be part of a skill's name typed after `$`.
fn name_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_' | ':' | '.')
}

/// **The `$` token that ends at `caret` in `text`**: a `$` at the start of
/// the line or after a space, then name characters up to the caret. Its
/// byte offset and what follows it; `None` when the caret is not in one —
/// `a$b`, `$$`, or a price like `5$` is not a token.
#[cfg(test)]
pub(crate) fn dollar_before(text: &str, caret: usize) -> Option<(usize, &str)> {
    trigger_before(text, caret, '$')
}

#[cfg(test)]
pub(crate) fn slash_before(text: &str, caret: usize) -> Option<(usize, &str)> {
    trigger_before(text, caret, '/')
}

fn trigger_before(text: &str, caret: usize, marker: char) -> Option<(usize, &str)> {
    let head = text.get(..caret)?;
    let start = head.rfind(|character: char| !name_char(character))?;
    if !head[start..].starts_with(marker) {
        return None;
    }
    if head[..start]
        .chars()
        .next_back()
        .is_some_and(|before| !before.is_whitespace())
    {
        return None;
    }
    Some((start, &head[start + 1..]))
}

/// A skill chip's record: the skill's name, under the skill role, labelled
/// with what the chip shows.
fn skill_attr(name: &str, label: &str) -> Attr {
    Attr::labelled(
        koru_core::attributes::ROLE_SKILL,
        AttrValue::Text(name.to_owned()),
        label.to_owned(),
    )
}

/// A message set aside by [`ChatComposer::stash`]: its private document and
/// the editor over it, so it comes back exactly as it was left.
pub(crate) struct Draft {
    ws: WorkspaceDoc,
    root: NodeId,
    editor: Editor,
}

/// The composer: a private one-document workspace and the editor over it.
pub struct ChatComposer {
    ws: WorkspaceDoc,
    root: NodeId,
    editor: Editor,
    images: ImageCache,
    /// The `@` picker's search, started the first time a picker opens — a
    /// chat that never mentions a document never starts a search reader.
    search: Option<EntitySearch>,
    store_root: Option<PathBuf>,
    /// The workspaces the last search answer named, as the picker's scope list.
    workspaces: Vec<WorkspaceChoice>,
    /// A complete Store catalog has been read — what a host with no current
    /// workspace waits for before its picker can search.
    catalog_known: bool,
    /// The workspace the host is showing, when it knows one.
    current_workspace: Option<WorkspaceId>,
    /// The document the host is showing, offered to a bare `@#`.
    current_document: Option<(koru_core::DocumentId, String)>,
    recent_documents: Vec<(koru_core::DocumentId, String)>,
    /// The box the text sits in, in host coordinates, padding included.
    text: Rect,
    /// Where host coordinates put the editor's view origin.
    origin: (f32, f32),
    open_request: Option<koru_link::KoruLink>,
}

impl ChatComposer {
    pub(crate) fn new(store_root: Option<PathBuf>) -> Self {
        let (ws, root, editor) = fresh();
        Self {
            ws,
            root,
            editor,
            images: ImageCache::new(),
            search: None,
            store_root,
            workspaces: Vec::new(),
            catalog_known: false,
            current_workspace: None,
            current_document: None,
            recent_documents: Vec::new(),
            text: Rect::new(0.0, 0.0, 1.0, 1.0),
            origin: (0.0, 0.0),
            open_request: None,
        }
    }

    // ------------------------------------------------------------ content

    /// Empty: no text and no chip. What the send button and `Enter` ask.
    pub(crate) fn is_blank(&self) -> bool {
        self.message().markdown.trim().is_empty()
    }

    /// The message as it would be sent now.
    pub(crate) fn message(&self) -> ComposedMessage {
        let markdown = koru_export_markdown::message_text(&self.ws, self.root).unwrap_or_default();
        let mut documents: Vec<(koru_ai_core::DocumentId, String)> = Vec::new();
        let mut skills: Vec<String> = Vec::new();
        for id in self
            .ws
            .children(Parent::Node(self.root))
            .unwrap_or_default()
        {
            let Ok(node) = self.ws.read(id) else {
                continue;
            };
            let attrs = node.content.attrs();
            for run in self.ws.runs(id).unwrap_or_default() {
                let Some(pill) = run.pill else {
                    continue;
                };
                let Some(attr) = attrs.iter().find(|attr| attr.id == pill) else {
                    continue;
                };
                if let AttrValue::Document(reference) = &attr.value {
                    let document =
                        koru_ai_core::DocumentId::from_uuid(reference.document.as_uuid());
                    if documents.iter().all(|(known, _)| *known != document) {
                        documents.push((document, attr.label.clone().unwrap_or_default()));
                    }
                }
                if attr.role == koru_core::attributes::ROLE_SKILL
                    && let AttrValue::Text(name) = &attr.value
                    && !name.is_empty()
                    && !skills.contains(name)
                {
                    skills.push(name.clone());
                }
            }
        }
        ComposedMessage {
            markdown,
            documents,
            skills,
        }
    }

    /// **The `$` the caret is in, if any** (koru-ai D121): not while a
    /// dictation is provisional, and not across a selection.
    pub(crate) fn dollar_token(&self) -> Option<DollarToken> {
        self.name_token('$')
    }

    pub(crate) fn slash_token(&self) -> Option<DollarToken> {
        self.name_token('/')
    }

    fn name_token(&self, marker: char) -> Option<DollarToken> {
        if self.composing() {
            return None;
        }
        let paragraph = self.editor.body.focused()?;
        if !paragraph.selection.is_collapsed() {
            return None;
        }
        let caret = paragraph.caret();
        let (start, query) = trigger_before(&paragraph.text, caret, marker)?;
        Some(DollarToken {
            row: paragraph.id,
            marker,
            start,
            end: caret,
            query: query.to_owned(),
        })
    }

    /// **Put a picked skill where its `$` was typed** (koru-ai D121): the
    /// `$` and what followed it become the skill's chip, labelled `label`,
    /// with a space after it, and the caret after that. One undo step;
    /// Backspace takes the chip back whole. `false` when the token is no
    /// longer there.
    pub(crate) fn insert_skill(&mut self, token: &DollarToken, name: &str, label: &str) -> bool {
        if token.marker != '$' && token.marker != '/' {
            return false;
        }
        self.insert_named(token, skill_attr(name, label))
    }

    pub(crate) fn insert_command(&mut self, token: &DollarToken, name: &str, label: &str) -> bool {
        if token.marker != '/' {
            return false;
        }
        self.insert_named(
            token,
            Attr::labelled(
                koru_core::attributes::ROLE_COMMAND,
                AttrValue::Text(name.to_owned()),
                label.to_owned(),
            ),
        )
    }

    fn insert_named(&mut self, token: &DollarToken, attr: Attr) -> bool {
        let current = self
            .ws
            .read(token.row)
            .ok()
            .and_then(|node| node.content.as_text().map(str::to_owned));
        if current
            .as_deref()
            .and_then(|text| trigger_before(text, token.end, token.marker))
            .is_none_or(|(start, _)| start != token.start)
        {
            return false;
        }
        self.ws.checkpoint();
        self.ws.begin_group();
        if self
            .ws
            .delete_text(token.row, token.start..token.end)
            .is_err()
            || self.ws.insert_pill(token.row, token.start, &attr).is_err()
        {
            self.ws.checkpoint();
            return false;
        }
        let mut caret = token.start + koru_core::ANCHOR.len_utf8();
        let spaced = self
            .ws
            .read(token.row)
            .ok()
            .and_then(|node| {
                node.content
                    .as_text()
                    .and_then(|text| text.get(caret..))
                    .map(|rest| rest.starts_with(char::is_whitespace))
            })
            .unwrap_or(false);
        if spaced || self.ws.insert_text(token.row, caret, " ").is_ok() {
            caret += 1;
        }
        self.ws.checkpoint();
        self.editor.view.active = true;
        self.editor.body.rebuild(&self.ws);
        self.editor.body.focus_on(token.row);
        self.editor.place_caret(caret);
        self.editor.view.desired_x = None;
        self.editor.settle();
        self.editor.scroll_to_caret();
        true
    }

    /// The message's text as it would be sent now — chips in their
    /// `[[koru://doc/…]]` spelling, one newline between lines.
    pub fn text(&self) -> String {
        self.message().markdown
    }

    /// Whether a dictation is still provisional. Send waits for it: a
    /// transcript that is still changing is not what the person meant to send.
    pub(crate) fn composing(&self) -> bool {
        self.editor.body.composition().is_some()
    }

    /// Start again with an empty message — after a send. A fresh private
    /// document, so undo does not reach back into a message already sent.
    pub(crate) fn clear(&mut self) {
        let active = self.editor.view.active;
        let viewport = self.editor.view.viewport;
        let (ws, root, mut editor) = fresh();
        editor.view.active = active;
        editor.set_viewport(viewport.0, viewport.1);
        self.ws = ws;
        self.root = root;
        self.editor = editor;
    }

    /// Replace the message with `text` as plain text, line breaks becoming
    /// paragraphs, the caret after the last character. For tests; a recalled
    /// message is [`set_message`](Self::set_message), which keeps its chips.
    #[cfg(test)]
    pub(crate) fn set_text(&mut self, text: &str) {
        self.clear();
        if !text.is_empty() {
            koru_editor::clipboard::insert_lines(&mut self.editor, &mut self.ws, text);
        }
    }

    /// Replace the message with one already sent, **chips and all** — what a
    /// recalled message is. `markdown` is the message as it went out, chips
    /// in their `[[koru://doc/…]]` spelling; each comes back as the chip it
    /// was, captioned by `title`, a blockquote comes back as the quote chip
    /// it was sent from (D88), and every other token stays text. One undo
    /// step, the caret after the last character.
    pub(crate) fn set_message(
        &mut self,
        markdown: &str,
        title: impl Fn(koru_ai_core::DocumentId) -> String,
    ) {
        self.clear();
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let Some(mut row) = self
            .ws
            .children(Parent::Node(self.root))
            .ok()
            .and_then(|rows| rows.first().copied())
        else {
            return;
        };
        self.ws.checkpoint();
        self.ws.begin_group();
        let mut at = 0;
        for (i, recalled) in recalled_rows(&normalized).into_iter().enumerate() {
            if i > 0 {
                match self.ws.split(row, at) {
                    Ok(tail) => {
                        row = tail;
                        at = 0;
                    }
                    Err(_) => break,
                }
            }
            let line = match recalled {
                Recalled::Line(line) => line,
                Recalled::Quote(passage) => {
                    if self.ws.insert_pill(row, at, &quote_attr(&passage)).is_ok() {
                        at += koru_core::ANCHOR.len_utf8();
                    }
                    continue;
                }
            };
            let mut from = 0;
            for found in koru_link::wrapped(line) {
                let attr = match found.link {
                    koru_link::KoruLink::Document { document, node } => {
                        let reference = koru_core::DocumentReference {
                            document: koru_core::DocumentId::from_uuid(document),
                            node: node.map(NodeId::from_uuid),
                        };
                        Attr::labelled(
                            "document",
                            AttrValue::Document(reference),
                            title(koru_ai_core::DocumentId::from_uuid(document)),
                        )
                    }
                    koru_link::KoruLink::Skill { name } => skill_attr(&name, &name),
                    koru_link::KoruLink::Command { name } => Attr::labelled(
                        koru_core::attributes::ROLE_COMMAND,
                        AttrValue::Text(name.clone()),
                        name,
                    ),
                    _ => continue,
                };
                at += insert_text(&mut self.ws, row, at, &line[from..found.range.start]);
                if self.ws.insert_pill(row, at, &attr).is_ok() {
                    at += koru_core::ANCHOR.len_utf8();
                }
                from = found.range.end;
            }
            at += insert_text(&mut self.ws, row, at, &line[from..]);
        }
        self.ws.checkpoint();
        self.editor.body.rebuild(&self.ws);
        self.editor.body.focus_on(row);
        self.editor.place_caret(at);
        self.editor.view.desired_x = None;
        self.editor.settle();
    }

    /// Put the caret before the message's first character: where a message
    /// recalled by Up leaves it, on the first line, so the next Up walks on
    /// instead of climbing through the lines of a long one.
    pub(crate) fn caret_to_start(&mut self) {
        let Some(first) = self
            .ws
            .children(Parent::Node(self.root))
            .ok()
            .and_then(|rows| rows.first().copied())
        else {
            return;
        };
        self.editor.body.focus_on(first);
        self.editor.place_caret(0);
        self.editor.view.desired_x = None;
        self.editor.settle();
    }

    /// Set the message being typed aside whole — its chips, its undo — and
    /// start an empty one: what walking back through earlier messages does,
    /// so walking forward past the newest can give it back untouched.
    pub(crate) fn stash(&mut self) -> Draft {
        let active = self.editor.view.active;
        let viewport = self.editor.view.viewport;
        let (ws, root, mut editor) = fresh();
        editor.view.active = active;
        editor.set_viewport(viewport.0, viewport.1);
        Draft {
            ws: std::mem::replace(&mut self.ws, ws),
            root: std::mem::replace(&mut self.root, root),
            editor: std::mem::replace(&mut self.editor, editor),
        }
    }

    /// Put a stashed message back, caret where it was left.
    pub(crate) fn unstash(&mut self, draft: Draft) {
        let active = self.editor.view.active;
        let viewport = self.editor.view.viewport;
        self.ws = draft.ws;
        self.root = draft.root;
        self.editor = draft.editor;
        self.editor.view.active = active;
        self.editor.set_viewport(viewport.0, viewport.1);
    }

    /// **Quote a passage into the message** (koru-ai D88): a quote chip at
    /// the caret, on a line of its own, and the caret on a new line under it
    /// for the comment. Text before the caret stays above the chip; text after
    /// it moves under, the caret at its start. One undo step. `false` for a
    /// passage with nothing in it.
    pub(crate) fn insert_quote(&mut self, passage: &str) -> bool {
        let passage = passage.replace("\r\n", "\n").replace('\r', "\n");
        let passage = passage.trim_matches('\n');
        if passage.trim().is_empty() {
            return false;
        }
        let first = self
            .ws
            .children(Parent::Node(self.root))
            .ok()
            .and_then(|rows| rows.first().copied());
        let Some((mut row, mut at)) = self
            .editor
            .body
            .focused()
            .map(|paragraph| (paragraph.id, paragraph.caret()))
            .or(first.map(|row| (row, 0)))
        else {
            return false;
        };
        self.ws.checkpoint();
        self.ws.begin_group();
        if at > 0
            && let Ok(tail) = self.ws.split(row, at)
        {
            row = tail;
            at = 0;
        }
        if self.ws.insert_pill(row, at, &quote_attr(passage)).is_err() {
            self.ws.checkpoint();
            return false;
        }
        let after = at + koru_core::ANCHOR.len_utf8();
        let (caret_row, caret) = match self.ws.split(row, after) {
            Ok(tail) => (tail, 0),
            Err(_) => (row, after),
        };
        self.ws.checkpoint();
        self.editor.view.active = true;
        self.editor.body.rebuild(&self.ws);
        self.editor.body.focus_on(caret_row);
        self.editor.place_caret(caret);
        self.editor.view.desired_x = None;
        self.editor.settle();
        self.editor.scroll_to_caret();
        true
    }

    /// Add a recent document at the caret, using the same stored pill as an
    /// `@` picker choice. The menu supplies only a title and identity; it
    /// never reads the document on the window thread.
    pub(crate) fn insert_document(
        &mut self,
        document: koru_ai_core::DocumentId,
        title: &str,
    ) -> bool {
        let first = self
            .ws
            .children(Parent::Node(self.root))
            .ok()
            .and_then(|rows| rows.first().copied());
        let Some((row, at)) = self
            .editor
            .body
            .focused()
            .map(|paragraph| (paragraph.id, paragraph.caret()))
            .or(first.map(|row| (row, 0)))
        else {
            return false;
        };
        let attr = Attr::labelled(
            "document",
            AttrValue::Document(koru_core::DocumentReference {
                document: koru_core::DocumentId::from_uuid(document.as_uuid()),
                node: None,
            }),
            title.to_owned(),
        );
        self.ws.checkpoint();
        self.ws.begin_group();
        if self.ws.insert_pill(row, at, &attr).is_err() {
            self.ws.checkpoint();
            return false;
        }
        let after = at + koru_core::ANCHOR.len_utf8();
        if !self
            .ws
            .read(row)
            .ok()
            .and_then(|node| {
                node.content
                    .as_text()
                    .map(|text| text[after..].starts_with(' '))
            })
            .unwrap_or(false)
        {
            let _ = self.ws.insert_text(row, after, " ");
        }
        self.ws.checkpoint();
        self.editor.view.active = true;
        self.editor.body.rebuild(&self.ws);
        self.editor.body.focus_on(row);
        self.editor.place_caret(after + 1);
        self.editor.view.desired_x = None;
        self.editor.settle();
        self.editor.scroll_to_caret();
        true
    }

    /// Whether the caret is on the message's first visual line — where Up
    /// leaves the text instead of moving inside it.
    pub(crate) fn caret_on_first_line(&self) -> bool {
        let body = &self.editor.body;
        body.focus <= body.first_body_row()
            && body
                .rows
                .get(body.focus)
                .is_none_or(|row| row.line_index() == 0)
    }

    /// Whether the caret is on the message's last visual line — where Down
    /// leaves the text instead of moving inside it.
    pub(crate) fn caret_on_last_line(&self) -> bool {
        let body = &self.editor.body;
        body.focus + 1 >= body.rows.len()
            && body
                .rows
                .get(body.focus)
                .is_none_or(|row| row.line_index() + 1 >= row.line_count())
    }

    /// Shift+Enter and Ctrl+J: a line break, which is a new plain paragraph.
    pub(crate) fn insert_break(&mut self) -> bool {
        self.editor.view.active = true;
        let handled = self.editor.act(&mut self.ws, Action::SplitLine);
        self.editor.settle();
        handled
    }

    /// Answer an open picker with `hits`, as the search worker would — for
    /// tests, which run with no store.
    #[cfg(test)]
    pub(crate) fn offer(&mut self, hits: &[(koru_ai_core::DocumentId, &str)]) -> bool {
        let hits = hits
            .iter()
            .map(|(document, title)| DocumentChoice {
                target: DocumentTarget {
                    document: koru_core::DocumentId::from_uuid(document.as_uuid()),
                    node: None,
                },
                title: (*title).to_owned(),
                breadcrumb: String::new(),
                is_file: false,
            })
            .collect();
        self.editor.set_entity_search_results(hits, false)
    }

    /// Theme, scale or font changes: lay everything out again, keep the text.
    pub(crate) fn invalidate(&mut self) {
        self.editor.view.engine.clear_pill_cache();
        self.editor.body.invalidate();
        self.editor.reflow();
    }

    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.editor.view.active = focused;
    }

    // ------------------------------------------------------------ context

    /// The workspace and document the host is showing. The workspace seeds
    /// the picker's scope; without one, the store's catalog does.
    pub(crate) fn set_context(
        &mut self,
        workspace: Option<koru_ai_core::WorkspaceId>,
        document: Option<(koru_ai_core::DocumentId, String)>,
    ) {
        self.current_workspace = workspace.map(|id| WorkspaceId::from_uuid(id.as_uuid()));
        self.current_document =
            document.map(|(id, title)| (koru_core::DocumentId::from_uuid(id.as_uuid()), title));
    }

    pub(crate) fn set_recent_documents(
        &mut self,
        documents: Vec<(koru_core::DocumentId, String)>,
    ) -> bool {
        if self.recent_documents == documents {
            return false;
        }
        self.recent_documents = documents;
        if !self.recent_documents.is_empty()
            && matches!(&self.editor.view.overlay, Overlay::Entity(picker) if picker.document_query.trim().is_empty())
        {
            let hits = self
                .recent_documents
                .iter()
                .map(|(document, title)| DocumentChoice {
                    target: DocumentTarget {
                        document: *document,
                        node: None,
                    },
                    title: title.clone(),
                    breadcrumb: String::new(),
                    is_file: false,
                })
                .collect();
            self.editor.set_entity_recents(hits)
        } else {
            false
        }
    }

    /// A chip was clicked, in the composer or in a sent message: the document
    /// the host should open.
    /// A paste found no text: the surface reads the clipboard's picture or
    /// files as attachments, off the window thread (chat-attachments D9).
    pub(crate) fn take_wants_paste_attachment(&mut self) -> bool {
        std::mem::take(&mut self.editor.view.wants_paste_attachment)
    }

    /// Arm the paste flag as a paste with no text would, for a test that
    /// must not touch the desktop clipboard.
    #[cfg(test)]
    pub(crate) fn arm_paste_attachment(&mut self) {
        self.editor.view.wants_paste_attachment = true;
    }

    pub(crate) fn take_open_request(&mut self) -> Option<koru_link::KoruLink> {
        self.open_request.take()
    }

    // ------------------------------------------------------------ geometry

    /// The height of the text the composer should show at `inner_w`: between
    /// `min_lines` and `max_lines` visual lines, not counting its padding.
    /// Past `max_lines` the composer scrolls.
    pub(crate) fn text_height(&mut self, inner_w: f32, min_lines: usize, max_lines: usize) -> f32 {
        let w = inner_w.max(1.0) + 2.0 * side();
        if (self.editor.view.viewport.0 - w).abs() > 0.5 {
            let h = self.editor.view.viewport.1;
            self.editor.set_viewport(w, h);
        }
        let rows = &self.editor.body.rows;
        let min = line_height() * min_lines.max(1) as f32;
        let Some(first) = rows.first() else {
            return min;
        };
        let top = first.y;
        let mut lines = 0usize;
        for row in rows {
            let count = row.layout.len().max(1);
            if lines + count >= max_lines {
                let take = max_lines - lines;
                return ((row.y - top) + row.height * take as f32 / count as f32).max(min);
            }
            lines += count;
        }
        let last = rows.last().expect("checked above");
        (last.y + last.height - top).max(min)
    }

    /// Place the editor in `text` (host coordinates, padding included), with
    /// `panels` the room its picker may float in.
    pub(crate) fn place(&mut self, text: Rect, panels: Rect) {
        let inner = Rect::new(
            text.x + PAD_X,
            text.y + PAD_TOP,
            (text.w - 2.0 * PAD_X).max(1.0),
            (text.h - PAD_TOP - PAD_BOTTOM).max(1.0),
        );
        let origin = (inner.x - side(), inner.y - theme::MARGIN_TOP);
        let viewport = (inner.w + 2.0 * side(), inner.h + 2.0 * theme::MARGIN_TOP);
        let embedded = Embedded {
            clip: Rect::new(text.x - origin.0, text.y - origin.1, text.w, text.h),
            panels: Rect::new(panels.x - origin.0, panels.y - origin.1, panels.w, panels.h),
        };
        self.text = text;
        self.origin = origin;
        self.editor.view.embedded = Some(embedded);
        let (w, h) = self.editor.view.viewport;
        if (w - viewport.0).abs() > 0.5 || (h - viewport.1).abs() > 0.5 {
            self.editor.set_viewport(viewport.0, viewport.1);
            self.editor.scroll_to_caret();
        }
    }

    fn view_point(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.origin.0, y - self.origin.1)
    }

    /// The `@` picker's panel, in host coordinates, while it is open.
    pub(crate) fn picker_rect(&self) -> Option<Rect> {
        if !self.picker_open() {
            return None;
        }
        self.editor
            .overlay_panel_rect()
            .map(|r| Rect::new(r.x + self.origin.0, r.y + self.origin.1, r.w, r.h))
    }

    /// Whether a point belongs to the composer: its text, or its open picker.
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        self.text.contains(x, y) || self.picker_rect().is_some_and(|r| r.contains(x, y))
    }

    /// Where a dictation's provisional words are, in host coordinates.
    pub(crate) fn composition_rect_host(&self) -> Option<Rect> {
        self.editor
            .dictation_rect()
            .map(|r| Rect::new(r.x + self.origin.0, r.y + self.origin.1, r.w, r.h))
    }

    // ------------------------------------------------------------ input

    /// Whether the `@` picker is open. **Its keys come first**: Up, Down,
    /// Enter and Escape belong to it until it closes.
    pub(crate) fn picker_open(&self) -> bool {
        matches!(self.editor.view.overlay, Overlay::Entity(_))
    }

    /// Close an open picker and leave what was typed — a press elsewhere in
    /// the chat, or outside it, is the same answer as Escape.
    pub(crate) fn close_picker(&mut self) -> bool {
        if !self.picker_open() {
            return false;
        }
        self.editor.view.overlay = Overlay::None;
        true
    }

    pub(crate) fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        self.editor.view.active = true;
        self.editor.view.mods = mods;
        let handled = self.editor.key(&mut self.ws, key, text, mods);
        self.sync();
        handled
    }

    pub(crate) fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        if !self.contains(x, y) {
            return false;
        }
        self.editor.view.active = true;
        let (vx, vy) = self.view_point(x, y);
        let handled = self.editor.mouse_down(&mut self.ws, vx, vy, false);
        self.sync();
        handled || self.text.contains(x, y)
    }

    pub(crate) fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let (vx, vy) = self.view_point(x, y);
        self.editor.mouse_move(vx, vy)
    }

    pub(crate) fn mouse_up(&mut self) -> bool {
        let changed = self.editor.mouse_up(&mut self.ws);
        self.sync();
        changed
    }

    pub(crate) fn mouse_double_click(&mut self, x: f32, y: f32) -> bool {
        if !self.text.contains(x, y) {
            return false;
        }
        let (vx, vy) = self.view_point(x, y);
        self.editor.mouse_double_click(&self.ws, vx, vy)
    }

    /// The wheel over the composer: the picker's list when it is under the
    /// pointer, else the composer's own lines once they overflow.
    pub(crate) fn scroll_by(&mut self, x: f32, y: f32, delta: MouseScrollDelta) -> bool {
        let over_picker = self.picker_rect().is_some_and(|r| r.contains(x, y));
        let overflows = self.editor.view.content_h > self.editor.view.viewport.1 + 0.5;
        if !over_picker && !(self.text.contains(x, y) && overflows) {
            return false;
        }
        if !over_picker && self.picker_open() {
            // `Editing::scroll_by` offers the wheel to an open panel first;
            // over the text it belongs to the text.
            return false;
        }
        self.editor.scroll_by(delta);
        true
    }

    // ------------------------------------------------------------ drawing

    pub(crate) fn draw(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        focused: bool,
        pointer: (f32, f32),
        placeholder: &str,
    ) {
        let pointer = self.view_point(pointer.0, pointer.1);
        let at = root * Affine::translate((self.origin.0 as f64, self.origin.1 as f64));
        frame(
            canvas,
            at,
            &mut self.editor,
            &self.images,
            Frame {
                caret_on: focused,
                pointer,
                placeholder,
                title_placeholder: "",
                status: ("", ""),
            },
            |_, _, _, _| {},
        );
    }

    /// The `@` picker, drawn **after** everything else the chat draws: it
    /// floats over the conversation, the document pill and the toolbar.
    pub(crate) fn draw_panels(&mut self, canvas: &mut Scene, root: Affine) {
        let at = root * Affine::translate((self.origin.0 as f64, self.origin.1 as f64));
        koru_editor::draw::floating_panels(canvas, at, &mut self.editor);
    }

    // ------------------------------------------------------------ the `@` loop

    /// Answer whatever the editor asked for since the last gesture: open the
    /// search, route a query or an outline, refuse a new page, surface a chip
    /// click. Cheap when nothing is pending.
    fn sync(&mut self) -> bool {
        let mut changed = false;
        // **A composer never makes a page.** The picker's "new page" row
        // closes the picker and leaves the typed `@…` as text; nothing is
        // created, and nothing is left waiting for a document that will never
        // come.
        if self.editor.take_wants_new_page().is_some() {
            self.editor.view.pending_new_page_trigger = None;
            changed = true;
        }
        if let Some((target, _)) = self.editor.take_wants_entity_open() {
            self.open_request = Some(koru_link::KoruLink::Document {
                document: target.document.as_uuid(),
                node: target.node.map(|node| node.as_uuid()),
            });
            changed = true;
        }
        // Doors the policy keeps shut; drained so a stale one cannot linger.
        self.editor.view.wants_open = None;
        self.editor.view.wants_toast = None;
        if !self.picker_open() {
            return changed;
        }
        if self.search.is_none() {
            self.search = Some(EntitySearch::new_at(self.store_root.clone()));
        }
        let scope = match self.current_workspace {
            Some(current) => Some((current, self.workspaces.clone())),
            // No workspace on screen. Every live workspace in the catalog is
            // the scope, the first one standing in for "current", which only
            // seeds the picker's bootstrap.
            None => self
                .search
                .as_mut()
                .and_then(EntitySearch::catalog_workspaces)
                .filter(|catalog| !catalog.is_empty())
                .map(|catalog| {
                    let choices = choices(catalog);
                    (choices[0].id, choices)
                }),
        };
        self.catalog_known |= self.current_workspace.is_none() && scope.is_some();
        if let Some((current, workspaces)) = scope {
            changed |= self.editor.set_entity_context(current, workspaces);
        }
        let here = self
            .current_document
            .as_ref()
            .map(|(document, title)| DocumentChoice {
                target: DocumentTarget {
                    document: *document,
                    node: None,
                },
                title: title.clone(),
                breadcrumb: String::new(),
                is_file: false,
            });
        changed |= self.editor.set_entity_current_document(here);
        if !self.recent_documents.is_empty()
            && matches!(&self.editor.view.overlay, Overlay::Entity(picker) if picker.document_query.trim().is_empty())
        {
            let hits = self
                .recent_documents
                .iter()
                .map(|(document, title)| DocumentChoice {
                    target: DocumentTarget {
                        document: *document,
                        node: None,
                    },
                    title: title.clone(),
                    breadcrumb: String::new(),
                    is_file: false,
                })
                .collect();
            changed |= self.editor.set_entity_recents(hits);
        }
        let search = self.search.as_mut().expect("started above");
        if let Some(request) = self
            .editor
            .take_entity_search_request()
            .and_then(|r| r.with_document_fallback(!self.recent_documents.is_empty()))
        {
            let scope = request
                .workspaces
                .into_iter()
                .map(|id| koru_ai_core::WorkspaceId::from_uuid(id.as_uuid()))
                .collect();
            search.request(request.query, scope);
            changed = true;
        }
        if let Some(document) = self.editor.take_heading_request() {
            search.request_outline(koru_ai_core::DocumentId::from_uuid(document.as_uuid()));
            changed = true;
        }
        changed
    }

    /// Deliver what the search worker answered. Called on the host's
    /// maintenance tick; never blocks.
    pub(crate) fn maintain(&mut self) -> bool {
        let mut changed = self.sync();
        let Some(search) = self.search.as_mut() else {
            return changed;
        };
        let mut outlines = Vec::new();
        while let Some(outline) = search.poll_outline() {
            outlines.push(outline);
        }
        let update = search.poll();
        for outline in outlines {
            let document = koru_core::DocumentId::from_uuid(outline.document.as_uuid());
            let headings = outline
                .headings
                .into_iter()
                .filter_map(|heading| {
                    Some(HeadingChoice {
                        node: NodeId::parse(&heading.node)?,
                        level: heading.level,
                        text: heading.text,
                    })
                })
                .collect();
            changed |= self
                .editor
                .set_headings(document, headings, outline.projected);
        }
        if let Some(update) = update {
            if !update.workspaces.is_empty() {
                self.workspaces = choices(update.workspaces);
            }
            // A chat mentions documents: a workspace *file* is not something
            // a turn can carry as context, so it is not offered.
            let hits = update
                .documents
                .into_iter()
                .filter(|hit| hit.kind != "file" && hit.kind != koru_core::explore::KIND_FILE)
                .map(|hit| DocumentChoice {
                    target: DocumentTarget {
                        document: koru_core::DocumentId::from_uuid(hit.document.as_uuid()),
                        node: hit.node.as_deref().and_then(NodeId::parse),
                    },
                    title: hit.title,
                    breadcrumb: hit.breadcrumb,
                    is_file: false,
                })
                .collect();
            if self.recent_documents.is_empty()
                || !matches!(&self.editor.view.overlay, Overlay::Entity(picker) if picker.document_query.trim().is_empty())
            {
                changed |= self
                    .editor
                    .set_entity_search_results(hits, update.unavailable);
            }
            changed |= self.sync();
        }
        changed
    }

    /// When the next poll is due: only while a search answer, an outline or
    /// the catalog is still owed to an open picker. An idle picker costs no
    /// ticks.
    pub(crate) fn maintenance_deadline(&self) -> Option<Instant> {
        let waiting = self.picker_open()
            && self.search.as_ref().is_some_and(|search| {
                search.pending()
                    || matches!(&self.editor.view.overlay, Overlay::Entity(picker)
                        if picker.headings().is_some_and(|headings| headings.loading))
                    || (self.current_workspace.is_none() && !self.catalog_known)
            });
        waiting.then(|| Instant::now() + POLL)
    }
}

/// A private one-document workspace, and the editor over it under the
/// composer policy. Undo is on: a chip survives undo and redo because the
/// editor's history is Loro's.
fn fresh() -> (WorkspaceDoc, NodeId, Editor) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "composer")
        .expect("an in-memory workspace is always creatable");
    let root = ws
        .insert(Parent::Root, Node::document("").expect("an empty title"))
        .expect("a document in a fresh workspace");
    ws.insert(
        Parent::Node(root),
        Node::paragraph("").expect("an empty paragraph"),
    )
    .expect("a paragraph in a fresh document");
    ws.enable_undo();
    let mut editor = Editor::rooted_at(&ws, root, true);
    // A chat message is not a document: it is set in the chat font — the
    // app font until chosen — at the app's size, like the messages above
    // it, and no document font, size or preview of one reaches it.
    editor.body.text_scale = koru_editor::TextScale::Chat;
    editor.body.title_row = false;
    editor.body.rebuild(&ws);
    editor.view.apply_policy(Policy::Composer);
    editor.view.status_h = 0.0;
    editor.view.chrome_h = 0.0;
    editor.view.page_gap_top = 0.0;
    editor.body.focus = editor.body.first_body_row();
    editor.place_caret(0);
    (ws, root, editor)
}

/// A quote chip's record: the passage whole, as text, under the quote role.
fn quote_attr(passage: &str) -> Attr {
    Attr::new(
        koru_core::attributes::ROLE_QUOTE,
        AttrValue::Text(passage.to_owned()),
    )
}

/// One row of a recalled message: a line of text, or a quoted passage.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Recalled<'a> {
    Line(&'a str),
    Quote(String),
}

/// A sent message read back into rows, **the inverse of what the composer
/// sends**: a run of `>` lines is one quoted passage, and the blank line
/// either side of it is the separator `message_text` wrote, not a line the
/// person typed. A message that ends on a quote gets an empty line under it
/// for the caret.
pub(crate) fn recalled_rows(markdown: &str) -> Vec<Recalled<'_>> {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut rows = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].starts_with('>') {
            rows.push(Recalled::Line(lines[i]));
            i += 1;
            continue;
        }
        let mut passage = Vec::new();
        while i < lines.len() && lines[i].starts_with('>') {
            let line = &lines[i][1..];
            passage.push(line.strip_prefix(' ').unwrap_or(line));
            i += 1;
        }
        if rows.last() == Some(&Recalled::Line("")) {
            rows.pop();
        }
        rows.push(Recalled::Quote(passage.join("\n")));
        if lines.get(i) == Some(&"") {
            i += 1;
        }
    }
    if matches!(rows.last(), Some(Recalled::Quote(_))) {
        rows.push(Recalled::Line(""));
    }
    rows
}

/// Insert `text` into `row` at `at`: the bytes it added, none for an empty
/// piece or a refusal.
fn insert_text(ws: &mut WorkspaceDoc, row: NodeId, at: usize, text: &str) -> usize {
    let plain = koru_link::unescape_literal_links(text);
    if !plain.is_empty() && ws.insert_text(row, at, &plain).is_ok() {
        plain.len()
    } else {
        0
    }
}

fn choices(workspaces: Vec<EntityWorkspace>) -> Vec<WorkspaceChoice> {
    workspaces
        .into_iter()
        .map(|workspace| WorkspaceChoice {
            profile: koru_core::ProfileId::from_uuid(workspace.profile.as_uuid()),
            profile_name: workspace.profile_name,
            id: WorkspaceId::from_uuid(workspace.id.as_uuid()),
            name: workspace.name,
        })
        .collect()
}

/// Dictation into the composer: the editor's own composition, over the
/// composer's own document — so the context is `()` and the chat's hosts route
/// it through `koru_ui::dictation::ComposeInto::Held`.
///
/// A multi-line transcript cannot be committed into one paragraph (the model
/// refuses `\n` in one), so it lands the way a paste does: the provisional run
/// is withdrawn and the lines go in at the caret as one undo step.
impl koru_ui::dictation::DictationTarget for ChatComposer {
    type Cx<'a> = ();

    fn accepts_dictation(&self) -> bool {
        koru_editor::dictation::accepts(&self.editor.view)
    }

    fn begin_composition(&mut self, (): ()) -> bool {
        self.editor.view.active = true;
        self.editor.begin_dictation(&mut self.ws)
    }

    fn update_composition(&mut self, (): (), text: &str) {
        self.editor.update_dictation(&mut self.ws, text);
    }

    fn commit_composition(&mut self, (): (), text: &str) {
        if text.contains(['\n', '\r']) {
            self.editor.cancel_dictation(&mut self.ws);
            koru_editor::clipboard::insert_lines(&mut self.editor, &mut self.ws, text);
        } else {
            self.editor.commit_dictation(&mut self.ws, text);
        }
        self.editor.settle();
    }

    fn cancel_composition(&mut self, (): ()) {
        self.editor.cancel_dictation(&mut self.ws);
        self.editor.settle();
    }

    /// In the chat's host coordinates, like `ChatSurface::dictation_anchor`.
    fn composition_rect(&self) -> Option<Rect> {
        self.composition_rect_host()
    }
}

#[cfg(test)]
mod tests;
