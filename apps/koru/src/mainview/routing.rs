// SPDX-License-Identifier: MPL-2.0

//! **Which layer owns this window's keyboard**, computed from one ordered
//! table.
//!
//! The vocabulary — [`Blockers`], [`EditableTarget`] and the kinds under them —
//! lives in [`koru_workspace_host::routing`], because the built-in document answers the
//! same two questions and a second copy would be a second opinion. What is here
//! is this container's answer: the order its own surfaces stack in, said once.
//!
//! ## Why the order is a table and not a chain of `if`s
//!
//! It used to be a chain of `if`s, and there were two of them: the `View::key`
//! wrapper claims the *Move to…* picker and the search overlay, then the
//! inherent `MainView::key` walks the ten surfaces below. Two chains drift the
//! first time a panel is added, and the drift is silent — a dictation lands
//! behind a modal, or in the document under a prompt.
//!
//! [`Layer`] is that order, once. Both entry points walk the same table and
//! differ only in **where they start**, which is why there is no second order
//! to get wrong. [`MainView::blockers`] starts at the top and is the whole
//! truth about this window; [`MainView::blockers_from`] is what the inherent
//! `key` uses to skip the two the trait wrapper has already claimed.
//!
//! ## What is deliberately *not* here
//!
//! The per-key layering below the blockers: the explore popover, the tree's
//! navigation keys, the container chords, the active tab. Those decide **per
//! key** — an open popover *"takes Esc, the arrows and Enter — and lets
//! everything else through"* — so no key-independent value can describe them.
//! `koru_workspace_host::routing`'s own note says why at length.

use koru_core::schema::WorkspaceDoc;
use koru_dictation::DictationAction;
use koru_ui::dictation::ComposeInto;

use koru_workspace_host::routing::{
    Blockers, EditableTarget, FieldId, ModalKind, PromptKind, TabId,
};

use super::{MainView, TabView};

/// Every surface that owns the keyboard outright, **topmost first**.
///
/// This is the whole order, and the only place it is written down. `Ord` is
/// derived and load-bearing: `blockers_from` compares against it to skip the
/// layers a caller above has already dealt with.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Layer {
    /// The installed-font picker. Above everything: it stands over a
    /// terminal tab or the Settings page that opened it, and owns the keys
    /// while it is up.
    FontPicker,
    /// The centred *Move to…* picker. Claimed by the `View::key` wrapper.
    MoveTo,
    /// The global-search overlay. Claimed by the `View::key` wrapper.
    Search,
    Error,
    Import,
    Updates,
    UpdateNewer,
    Confirm,
    NewDocument,
    Snapshot,
    LinkDevice,
    Appearance,
    Rename,
    /// The tasks rail's inline "+ New list" (or rename-list) field. Below
    /// `Rename` because a workspace rename and a list rename cannot stand at
    /// once, and above `Menu` because the field outlives the context menu
    /// that opened it.
    NewList,
    /// The delegation modal. Above `Menu` because the surface outlives the
    /// menu row that opened it, and below `NewList` because a rename field
    /// and this cannot both be standing.
    Delegate,
    /// The session-recording setup modal. Same argument as `Delegate`: the
    /// caption-menu row that opens it is gone before the modal answers.
    Recording,
    Menu,
    ReminderOverlay,
    ReminderManagement,
    ReminderCreation,
}

impl Layer {
    /// The table, in order. Walked rather than short-circuited through a chain
    /// of `if`s so that "the order" is a value two callers can share.
    ///
    /// `pub(crate)` because there is a third caller now: the accessibility
    /// projection publishes the standing blocker and nothing beneath it, and
    /// the test that every kind of blocker publishes the control it focuses
    /// walks *this* table rather than a second list of kinds to keep in step.
    pub(crate) const ALL: [Self; 20] = [
        Self::FontPicker,
        Self::MoveTo,
        Self::Search,
        Self::Error,
        Self::Import,
        Self::Updates,
        Self::UpdateNewer,
        Self::Confirm,
        Self::NewDocument,
        Self::Snapshot,
        Self::LinkDevice,
        Self::Appearance,
        Self::Rename,
        Self::NewList,
        Self::Delegate,
        Self::Recording,
        Self::Menu,
        Self::ReminderOverlay,
        Self::ReminderManagement,
        Self::ReminderCreation,
    ];

    /// What this layer *is*, to a caller that only wants to know whether it can
    /// be typed into.
    pub(crate) fn blocker(self) -> Blockers {
        match self {
            Self::FontPicker => Blockers::Prompt(PromptKind::FontPicker),
            Self::MoveTo => Blockers::Prompt(PromptKind::MoveTo),
            Self::Search => Blockers::Prompt(PromptKind::Search),
            Self::Error => Blockers::Modal(ModalKind::Error),
            Self::Import => Blockers::Modal(ModalKind::Import),
            Self::Updates => Blockers::Modal(ModalKind::Updates),
            Self::UpdateNewer => Blockers::Modal(ModalKind::UpdateNewer),
            Self::Confirm => Blockers::Modal(ModalKind::Confirm),
            Self::NewDocument => Blockers::Prompt(PromptKind::NewDocument),
            Self::Snapshot => Blockers::Prompt(PromptKind::Snapshot),
            Self::LinkDevice => Blockers::Prompt(PromptKind::LinkDevice),
            Self::Appearance => Blockers::Prompt(PromptKind::Appearance),
            Self::Rename => Blockers::Prompt(PromptKind::Rename),
            Self::NewList => Blockers::Prompt(PromptKind::NewList),
            // A **prompt**, not a modal: the objective is somewhere to type,
            // which is what lets a dictation reach it (`EditableTarget`).
            Self::Delegate => Blockers::Prompt(PromptKind::Delegate),
            // A **modal**: the setup is a small form with its own focus ring,
            // and nothing underneath may receive keys while it stands.
            Self::Recording => Blockers::Modal(ModalKind::Recording),
            Self::Menu => Blockers::Modal(ModalKind::ContextMenu),
            Self::ReminderOverlay => Blockers::Modal(ModalKind::ReminderOverlay),
            Self::ReminderManagement => Blockers::Modal(ModalKind::ReminderManagement),
            Self::ReminderCreation => Blockers::Prompt(PromptKind::ReminderCreation),
        }
    }
}

impl MainView {
    /// One line of `KORU_INPUT_DEBUG` about who owns this window's keyboard.
    ///
    /// The shell already logs that a key arrived and whether the view said
    /// `handled`. This is the missing middle: a caret that moves while typing
    /// does not is usually a layer above the document still claiming keys, or
    /// the document having dropped the caret (`active=false`).
    pub(crate) fn trace_input_state(&self, stage: &'static str) {
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] view {stage} blockers={:?} explore_focus={} ai_focused={} ai_visible={} tab={} pinned={}",
            self.blockers(),
            self.explore.has_focus(),
            self.ai_focused,
            self.ai_visible(),
            match &self.tabs().active_tab().view {
                TabView::Leaf(_) => "document",
                TabView::Welcome(_) => "welcome",
                TabView::Home(_) => "home",
                TabView::Manage(_) => "manage",
                TabView::Trash(_) => "trash",
                TabView::Tasks(_) => "tasks",
                TabView::Settings(_) => "settings",
                TabView::Feedback(_) => "feedback",
                TabView::Diff(_) => "diff",
                TabView::AiChat(_) => "ai-chat",
                TabView::Socials(_) => "socials",
                TabView::Session(_) => "session",
                TabView::Run(_) => "run",
                TabView::Result(_) => "result",
                TabView::Canvas(_) => "canvas",
                TabView::Reminders(_) => "reminders",
            },
            self.tabs().active_tab().is_pinned(),
        ));
    }

    /// Whether `layer`'s surface is standing.
    ///
    /// One predicate per row of the table, and nothing else in the tree asks
    /// these questions in an order of its own.
    fn layer_is_up(&self, layer: Layer) -> bool {
        match layer {
            Layer::FontPicker => self.font_picker.is_some(),
            Layer::MoveTo => self.move_to.is_some(),
            Layer::Search => self.search.is_some(),
            Layer::Error => self.error.is_some(),
            Layer::Import => self.import_dialog.is_some(),
            Layer::Updates => self.updates_modal,
            Layer::UpdateNewer => self.update_newer_modal_open(),
            Layer::Confirm => self.confirm.is_some(),
            Layer::NewDocument => self.new_document.is_some(),
            Layer::Snapshot => self.snapshot_prompt.is_some(),
            Layer::LinkDevice => self.link_device.is_some(),
            Layer::Appearance => self.workspace_icon.is_some(),
            Layer::Rename => self.workspace_rename.is_some(),
            Layer::NewList => self.new_list.is_some(),
            Layer::Delegate => self.delegation_open(),
            Layer::Recording => self.recording_modal.is_some(),
            Layer::Menu => self.menu.is_some(),
            Layer::ReminderOverlay => self.reminder_shell.overlay_live(),
            Layer::ReminderManagement => self.reminder_creation.is_management(),
            Layer::ReminderCreation => self.reminder_creation.is_creation(),
        }
    }

    /// The topmost blocker over this whole window.
    ///
    /// What a dictation asks, and the honest answer: it includes the two
    /// surfaces the `View::key` wrapper claims before the container's own chain
    /// ever runs.
    pub(crate) fn blockers(&self) -> Blockers {
        self.blockers_from(Layer::FontPicker)
    }

    /// …and the same order starting at `from`, for a caller that has already
    /// dealt with the layers above it.
    ///
    /// The inherent `MainView::key` starts at [`Layer::Error`] because
    /// `View::key` has already routed *Move to…* and search. That asymmetry is
    /// real and is preserved here rather than tidied away: the inherent method
    /// is what the container's own tests drive, and making it claim two
    /// surfaces it never claimed would be a behaviour change wearing a
    /// refactor's clothes.
    pub(crate) fn blockers_from(&self, from: Layer) -> Blockers {
        for layer in Layer::ALL {
            if layer >= from && self.layer_is_up(layer) {
                return layer.blocker();
            }
        }
        Blockers::None
    }

    /// Where a typed character would land.
    ///
    /// **Not** "who owns the keyboard": neither an open popover nor a focused
    /// explore tree accepts text — both let a character through to the view
    /// underneath — so neither appears here. That is the whole reason this
    /// question has a single answer where the other one does not.
    ///
    /// `None` is a real answer and the safe one. A welcome screen, the
    /// workspace manager, the Trash listing and a read-only document all take
    /// no text, and a wrong `None` merely declines a dictation where a wrong
    /// `Tab` would put words into a document.
    pub(crate) fn editable_target(&self) -> EditableTarget {
        let blockers = self.blockers();
        if blockers.is_blocking() {
            return blockers.editable();
        }
        // The AI composer, which sits below every chord and above the tab — the
        // same place `MainView::key` routes it.
        if self.ai_visible() && self.ai_focused {
            return EditableTarget::Field(FieldId::Ai);
        }
        match &self.tabs().active_tab().view {
            TabView::Leaf(leaf) => leaf.editable_target(TabId(self.tabs().active())),
            // The Tasks page's capture row is not a dictation surface this
            // release; a wrong `None` merely declines a dictation.
            TabView::Welcome(_)
            | TabView::Home(_)
            | TabView::Manage(_)
            | TabView::Settings(_)
            | TabView::Trash(_)
            | TabView::Tasks(_)
            | TabView::Reminders(_)
            | TabView::Diff(_)
            | TabView::Feedback(_)
            | TabView::AiChat(_)
            | TabView::Socials(_)
            | TabView::Session(_)
            // The rating's note is the one field on a run view, and it is a
            // remark about how a run went rather than a dictation surface. A
            // result tab has no field at all: it reads a version.
            | TabView::Run(_)
            | TabView::Result(_)
            // The placeholder's one field is its name prompt, which is not a
            // dictation surface; the canvas surface decides for its cards.
            | TabView::Canvas(_) => EditableTarget::None,
        }
    }
}

/// **Which surface a dictation is going into, decided once.**
///
/// The generation is what the composition was opened for. An action naming any
/// other generation is dropped — a `Final` from a session that was cancelled a
/// moment ago must commit nothing, and the controller dropping it on the way
/// out is not a reason for the router to trust that it always will.
pub(crate) struct LiveDictation {
    pub(crate) generation: u64,
    pub(crate) target: EditableTarget,
    /// **Which tab, by index, for the four targets a leaf owns.**
    ///
    /// `editable_target` names a document's find bar as `Field(Find)` with no
    /// tab in it, because a character always goes to the *active* one. A
    /// dictation outlives a press, so the tab it started in is recorded here
    /// and addressed by index — otherwise a commit would land in whatever tab
    /// happened to be showing when the `Final` arrived.
    pub(crate) tab: usize,
}

impl MainView {
    /// **Route a dictation to wherever a typed character would go.**
    ///
    /// One decision, at [`DictationAction::Begin`], through
    /// [`blockers`](Self::blockers) and then
    /// [`editable_target`](Self::editable_target) — the same two answers the
    /// key path derives, which is the whole reason Plan 2.1 factored them out.
    /// **There is no second chain here**: a dictation that skipped a modal
    /// would type into the document behind a dialog, and the way that bug
    /// happens is somebody writing the order twice.
    ///
    /// Every later action goes to what was recorded. Re-deriving it per
    /// partial would let a surface that gained focus mid-utterance take the
    /// rest of the sentence.
    pub(crate) fn dictate(&mut self, ws: &mut WorkspaceDoc, action: &DictationAction) -> bool {
        if let DictationAction::Begin { generation } = action {
            if self.dictation.is_some() {
                return false;
            }
            let target = self.editable_target();
            if !Self::accepts_dictation(target) {
                return false;
            }
            let opened = self.deliver_to(ws, target, self.tabs().active(), action);
            if opened {
                self.dictation = Some(LiveDictation {
                    generation: *generation,
                    target,
                    tab: self.tabs().active(),
                });
            }
            return opened;
        }
        let Some(live) = self.dictation.as_ref() else {
            return false;
        };
        if action.generation() != live.generation {
            return false;
        }
        let (target, tab) = (live.target, live.tab);
        if action.closes() {
            self.dictation = None;
        }
        self.deliver_to(ws, target, tab, action)
    }

    /// **Whether this target may be dictated into at all.**
    ///
    /// The refusal lives *here*, in the host, and not in the widget — which is
    /// Q-V25's answer. A `TextInput` is a search box in one place and an API
    /// key in another, and the only thing that separates them is which `draw`
    /// method the host calls, so the box cannot know it is holding a secret.
    /// The host does.
    pub(crate) fn accepts_dictation(target: EditableTarget) -> bool {
        match target {
            // **The device-link code is a secret.** It is drawn through
            // `draw_masked`, it authorises a device against this account, and
            // it is read off another screen rather than composed — so
            // dictating it aloud is both useless and the one case where the
            // provisional text is a credential.
            EditableTarget::Field(FieldId::LinkDevice) => false,
            EditableTarget::Field(_) | EditableTarget::Tab(_) => true,
            EditableTarget::None => false,
        }
    }

    /// Perform `action` on `target`, in the tab it began in.
    fn deliver_to(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: EditableTarget,
        tab: usize,
        action: &DictationAction,
    ) -> bool {
        match target {
            EditableTarget::None => false,
            // The three fields a *document* owns, and its body, are the
            // document's to route: it has its own find bar, its own prompts
            // and its own rows, and it answers the same two questions about
            // them that this container answers about its prompts.
            EditableTarget::Tab(_)
            | EditableTarget::Field(FieldId::Find)
            | EditableTarget::Field(FieldId::Replace)
            | EditableTarget::Field(FieldId::DocumentPrompt) => {
                match self.tabs_mut().get_mut(tab).map(|t| &mut t.view) {
                    Some(TabView::Leaf(leaf)) => leaf.dictate(ws, action),
                    _ => false,
                }
            }
            EditableTarget::Field(field) => {
                if field == FieldId::ReminderCreation {
                    let phrase = self.reminder_creation.dictate(&mut self.engine, action);
                    return phrase.is_some_and(|phrase| {
                        if let Some(phrase) = self.reminder_creation.resolve_edit_phrase(phrase) {
                            let _ = self.reminders.update_creation_phrase(phrase);
                        }
                        true
                    });
                }
                // Search owns result/completion invalidation. Editing its bare
                // field would show words while leaving the old result page.
                if field == FieldId::Search {
                    return self.search.as_mut().is_some_and(|s| s.dictate(action));
                }
                let Some(mut compose) = self.compose_into(field) else {
                    return false;
                };
                match action {
                    DictationAction::Begin { .. } => compose.begin(),
                    DictationAction::Update { text, .. } => {
                        compose.update(text.as_str());
                        true
                    }
                    DictationAction::Commit { text, .. } => {
                        compose.commit(text.as_str());
                        true
                    }
                    DictationAction::Cancel { .. } => {
                        compose.cancel();
                        true
                    }
                }
            }
        }
    }

    /// The one-line box `field` names, with the engine that lays it out.
    ///
    /// Most of these are laid out by this container's own `engine`; global
    /// search owns one of its own, which is why the pair travels together
    /// rather than the engine being assumed.
    ///
    /// `None` is not a refusal — [`accepts_dictation`](Self::accepts_dictation)
    /// is where a refusal is decided. It means this container either has no
    /// text box to hand over, or the owning surface needs to route the action
    /// itself, as global search does for result invalidation.
    fn compose_into(&mut self, field: FieldId) -> Option<ComposeInto<'_>> {
        match field {
            // Routed through the surface in `deliver_to`, because committing
            // the query has result-set side effects beyond editing the field.
            FieldId::Search => None,
            FieldId::FontPicker => Some(ComposeInto::Input(
                self.font_picker.as_mut()?.surface.dictation_field(),
                &mut self.engine,
            )),
            FieldId::MoveTo => Some(ComposeInto::Input(
                self.move_to.as_mut()?.dictation_field(),
                &mut self.engine,
            )),
            FieldId::Rename => Some(ComposeInto::Input(
                self.workspace_rename.as_mut()?.dictation_field(),
                &mut self.engine,
            )),
            FieldId::Snapshot => Some(ComposeInto::Field(
                self.snapshot_prompt.as_mut()?.dictation_field(),
                &mut self.engine,
            )),
            FieldId::NewDocument => Some(ComposeInto::Field(
                self.new_document.as_mut()?.dictation_field(),
                &mut self.engine,
            )),
            FieldId::Appearance => Some(ComposeInto::Field(
                self.workspace_icon.as_mut()?.dictation_field()?,
                &mut self.engine,
            )),
            // Refused above rather than answered here.
            FieldId::LinkDevice => None,
            // The rail's inline list-name field — no dictation route this
            // release, the LinkDevice pattern.
            FieldId::NewList => None,
            FieldId::ReminderCreation => None,
            // The delegation modal's objective is a real composer: saying an
            // objective out loud is exactly the shape of thing this field is
            // for. Only while the objective has the focus — a dictation must
            // not land in a field the ring has moved off.
            FieldId::Delegate => {
                let modal = self.delegation.as_mut()?;
                modal
                    .objective_focused()
                    .then(|| ComposeInto::Area(modal.dictation_field(), &mut self.engine))
            }
            // Owned by the active tab, routed there.
            FieldId::Find | FieldId::Replace | FieldId::DocumentPrompt => None,
            // The chip composer owns its private document, so it travels
            // with nothing borrowed beside it.
            FieldId::Ai => Some(ComposeInto::Held(self.ai_chat.dictation_composer())),
        }
    }
}

#[cfg(test)]
mod tests;
