// SPDX-License-Identifier: MPL-2.0

//! **An agent's skills under `$` and commands under `/`**.
//!
//! ```text
//!   ┌─────────────────────────────────────────────┐
//!   │ review-pr                                    │   the folder's first,
//!   │ Review the open pull request against the…   │   then the person's,
//!   │ Release Notes                                │   the agent's own and
//!   │ Write the notes for a release               │   an administrator's
//!   └─────────────────────────────────────────────┘
//!   Use $re▏
//! ```
//!
//! Typing `$` at the start of a line or after a space opens, above the
//! composer, the skills the chat's agent lists for the folder it works in,
//! filtered as the person types: names and display names that start with
//! what follows the `$` first, then the ones that contain it, each group in
//! the agent's order. Up and Down move, Enter or Tab picks, Escape closes
//! (and the list stays shut for that `$`); a press on a row picks it. A
//! picked entry becomes a skill or command pill at the trigger's position;
//! the provider sees `$name` or `/name` there when it receives the prompt.
//!
//! ## Where the list comes from
//!
//! **Nothing here reads a command from disk.** The list is the agent's own
//! (`Command::DiscoverCommands`, the session host's `skills/list` for the
//! chat's folder), asked for off the window thread when `$` or `/` is typed and
//! the one held is older than [`STALE_AFTER`] — the one held is shown
//! meanwhile, so a skill added or removed shows on the next `$`. A chat
//! whose agent lists none, or that has no local agent, never opens the list:
//! there `$` is only a character. Where a skill lives never reaches the
//! window; the session host resolves it by name at send.
//!
//! [`ChatComposer::insert_skill`]: crate::composer::ChatComposer::insert_skill

use std::time::{Duration, Instant};

use koru_ai_core::AiProviderId;
use koru_ai_wire::AgentCommandView;
use koru_draw::el::{Frame, FrameCx, Interaction};
use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::theme::{Profile, roles};
use koru_ui::tooltip::{Tooltip, TooltipContent};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::ChatSurface;
use crate::composer::DollarToken;

mod elements;
#[cfg(test)]
mod tests;
mod tokens;

/// The panel's corners, as the composer's other panels have them.
const PANEL_RADIUS: f32 = 8.0;
/// The rows shown at once; the keyboard and the wheel move through the rest.
pub(crate) const VISIBLE_ROWS: usize = 8;
/// A list older than this is asked for again when a new `$` is typed.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(5);
/// How long an ask that has not been answered holds off another.
const ASK_PATIENCE: Duration = Duration::from_secs(30);

/// Whose skills: the agent, and the folder its chat works in (`None`, the
/// home folder it starts in).
pub(crate) type SkillSource = (AiProviderId, Option<String>);

/// A list the agent answered, for one source.
struct Listed {
    source: SkillSource,
    skills: Vec<AgentCommandView>,
    at: Instant,
}

/// The `$` list's state, kept on the surface.
#[derive(Default)]
pub(crate) struct SkillList {
    listed: Option<Listed>,
    /// An ask on its way, for which source and since when.
    asked: Option<(SkillSource, Instant)>,
    /// The `$` the caret is in — its row and offset — while there is one.
    following: Option<(koru_core::NodeId, usize, char)>,
    /// Escape shut the list for the `$` being followed.
    dismissed: bool,
    /// The open list's token; `None` while it is shut.
    open: Option<DollarToken>,
    /// The row the keyboard is on, among the matches.
    focus: usize,
    /// The first match shown.
    offset: usize,
    /// A row's whole description, while the pointer rests on a row that
    /// elides it ([`ChatSurface::skill_tooltip_at`]).
    tooltip: Tooltip,
}

/// **The matches for `query`, in the order the list shows them**: indexes
/// into `skills` whose name or display name starts with it (ignoring case),
/// then those that contain it, each group in the agent's order. Every skill
/// for an empty query.
pub(crate) fn matches(skills: &[AgentCommandView], query: &str) -> Vec<usize> {
    matches_for(skills, query, '$')
}

fn matches_for(skills: &[AgentCommandView], query: &str, marker: char) -> Vec<usize> {
    let query = query.to_lowercase();
    let words = |skill: &AgentCommandView| {
        let mut words = vec![skill.name.to_lowercase()];
        if let Some(display) = &skill.display_name {
            words.push(display.to_lowercase());
        }
        words
    };
    let mut prefix = Vec::new();
    let mut within = Vec::new();
    for (index, skill) in skills.iter().enumerate() {
        if marker == '$' && skill.kind == koru_ai_wire::AgentCommandKindView::Command {
            continue;
        }
        let words = words(skill);
        if words.iter().any(|word| word.starts_with(&query)) {
            prefix.push(index);
        } else if words.iter().any(|word| word.contains(&query)) {
            within.push(index);
        }
    }
    prefix.extend(within);
    prefix
}

/// What a skill's row and chip are called: the agent's display name when
/// it gives one, else the skill's own name.
pub(crate) fn title(skill: &AgentCommandView) -> &str {
    skill
        .display_name
        .as_deref()
        .filter(|display| !display.trim().is_empty())
        .unwrap_or(&skill.name)
}

impl ChatSurface {
    /// Whose skills the composer offers: the chat's local agent, in its
    /// folder. `None` for a chat with no local agent.
    fn skill_source(&self) -> Option<SkillSource> {
        let chat = &self.transcript.as_ref()?.chat;
        crate::agents::is_agent_provider(&chat.current_model.provider).then(|| {
            (
                chat.current_model.provider.clone(),
                chat.agent_working_folder.clone(),
            )
        })
    }

    /// The skills listed for the chat's agent and folder; none until it
    /// answers.
    fn listed_skills(&self) -> &[AgentCommandView] {
        if let Some(commands) = self
            .transcript
            .as_ref()
            .and_then(|transcript| transcript.agent_commands.as_ref())
        {
            return commands;
        }
        match (&self.skills.listed, self.skill_source()) {
            (Some(listed), Some(source)) if listed.source == source => &listed.skills,
            _ => &[],
        }
    }

    /// The open list's matches, in its order; none while it is shut.
    fn skill_rows(&self) -> Vec<usize> {
        match &self.skills.open {
            Some(token) => matches_for(self.listed_skills(), &token.query, token.marker),
            None => Vec::new(),
        }
    }

    /// Whether the `$` list is open.
    pub(crate) fn skill_list_open(&self) -> bool {
        self.skills.open.is_some()
    }

    /// Shut the list; a later key opens it again where it applies.
    pub(crate) fn close_skill_list(&mut self) -> bool {
        self.skills.tooltip.clear();
        self.skills.open.take().is_some()
    }

    /// **Ask the agent for its skills**, off the window thread, unless the
    /// list held is fresh or an ask is already on its way. Straight to the
    /// reader: a background ask is not something the person waits on.
    fn ask_skills(&mut self, source: &SkillSource) {
        if self
            .transcript
            .as_ref()
            .is_some_and(|transcript| transcript.agent_commands.is_some())
        {
            return;
        }
        let now = Instant::now();
        let fresh = self.skills.listed.as_ref().is_some_and(|listed| {
            listed.source == *source && now.saturating_duration_since(listed.at) < STALE_AFTER
        });
        let waiting = self.skills.asked.as_ref().is_some_and(|(asked, at)| {
            asked == source && now.saturating_duration_since(*at) < ASK_PATIENCE
        });
        if fresh || waiting {
            return;
        }
        let sent = self.reader.as_ref().is_some_and(|reader| {
            reader
                .send(crate::reader::Command::DiscoverCommands(
                    source.0.clone(),
                    source.1.clone(),
                ))
                .is_ok()
        });
        if sent {
            self.skills.asked = Some((source.clone(), now));
        }
    }

    /// **The agent's answer** (`Update::AgentCommands`): kept for its source,
    /// and the list follows the caret again — it opens now if a `$` was
    /// typed while the answer was on its way. A failed listing keeps
    /// nothing new; the next `$` asks again.
    pub(crate) fn skills_listed(
        &mut self,
        provider: AiProviderId,
        folder: Option<String>,
        skills: Option<Vec<AgentCommandView>>,
    ) -> bool {
        let source = (provider, folder);
        if self
            .skills
            .asked
            .as_ref()
            .is_some_and(|(asked, _)| *asked == source)
        {
            self.skills.asked = None;
        }
        if let Some(skills) = skills {
            self.skills.listed = Some(Listed {
                source,
                skills,
                at: Instant::now(),
            });
        }
        self.sync_skill_list()
    }

    /// **Follow the caret** (after every key the composer takes and every
    /// press in it): open the list while the caret is in a `$` token the
    /// chat's agent has skills for, filtered by what follows the `$`; shut
    /// it otherwise. A new `$` asks for the list when the one held is stale.
    /// Answers whether what is drawn changed.
    pub(crate) fn sync_skill_list(&mut self) -> bool {
        let was = self.skills.open.clone();
        let token = self
            .composer
            .dollar_token()
            .or_else(|| self.composer.slash_token());
        let source = self.skill_source();
        let (Some(token), Some(source)) = (token, source) else {
            self.skills.following = None;
            self.skills.dismissed = false;
            self.skills.open = None;
            self.skills.tooltip.clear();
            return was.is_some();
        };
        let at = (token.row, token.start, token.marker);
        if self.skills.following != Some(at) {
            self.skills.following = Some(at);
            self.skills.dismissed = false;
            self.skills.focus = 0;
            self.skills.offset = 0;
            self.ask_skills(&source);
        }
        if self.skills.dismissed {
            self.skills.open = None;
            self.skills.tooltip.clear();
            return was.is_some();
        }
        let count = matches_for(self.listed_skills(), &token.query, token.marker).len();
        if count == 0 {
            self.skills.open = None;
            self.skills.tooltip.clear();
            return was.is_some();
        }
        if was.as_ref().is_some_and(|was| was.query != token.query) {
            self.skills.focus = 0;
            self.skills.offset = 0;
        }
        self.skills.focus = self.skills.focus.min(count - 1);
        self.keep_skill_focus_shown(count);
        let changed = was.as_ref() != Some(&token);
        self.skills.open = Some(token);
        changed
    }

    /// Scroll the rows shown so the keyboard's row is among them.
    fn keep_skill_focus_shown(&mut self, count: usize) {
        let skills = &mut self.skills;
        if skills.focus < skills.offset {
            skills.offset = skills.focus;
        } else if skills.focus >= skills.offset + VISIBLE_ROWS {
            skills.offset = skills.focus + 1 - VISIBLE_ROWS;
        }
        skills.offset = skills.offset.min(count.saturating_sub(VISIBLE_ROWS));
    }

    /// **Pick the `index`th match**: its chip takes the `$` token's place,
    /// and the list shuts.
    fn pick_skill(&mut self, index: usize) -> bool {
        let Some(token) = self.skills.open.clone() else {
            return false;
        };
        let rows = self.skill_rows();
        let Some(skill) = rows
            .get(index)
            .and_then(|row| self.listed_skills().get(*row))
            .cloned()
        else {
            return false;
        };
        self.skills.open = None;
        self.skills.following = None;
        let picked = if skill.kind == koru_ai_wire::AgentCommandKindView::Command {
            self.composer
                .insert_command(&token, &skill.name, title(&skill))
        } else {
            self.composer
                .insert_skill(&token, &skill.name, title(&skill))
        };
        self.sync_skill_list();
        picked
    }

    /// **The open list's keys**: Up and Down move, Enter or Tab picks,
    /// Escape shuts it for this `$`. `None` when it is shut or the key is
    /// not one of them — every other key is the composer's.
    pub(crate) fn skill_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if self.skills.open.is_none() || !mods.is_empty() {
            return None;
        }
        // A list left open over a token the caret has since left is shut.
        if self
            .composer
            .dollar_token()
            .or_else(|| self.composer.slash_token())
            != self.skills.open
        {
            self.sync_skill_list();
            if self.skills.open.is_none() {
                return None;
            }
        }
        let count = self.skill_rows().len();
        if count == 0 {
            return None;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.skills.dismissed = true;
                Some(self.close_skill_list())
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.skills.focus = (self.skills.focus + 1) % count;
                self.keep_skill_focus_shown(count);
                Some(true)
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.skills.focus = (self.skills.focus + count - 1) % count;
                self.keep_skill_focus_shown(count);
                Some(true)
            }
            Key::Named(NamedKey::Enter | NamedKey::Tab) => Some(self.pick_skill(self.skills.focus)),
            _ => None,
        }
    }

    /// What the open list's tree declares: the rows shown, the keyboard's
    /// lit.
    fn skill_specs(&self) -> Vec<elements::SkillSpec> {
        let skills = self.listed_skills();
        self.skill_rows()
            .into_iter()
            .enumerate()
            .skip(self.skills.offset)
            .take(VISIBLE_ROWS)
            .filter_map(|(index, row)| {
                let skill = skills.get(row)?;
                Some(elements::SkillSpec {
                    key: elements::row_key(&skill.name),
                    title: title(skill).to_owned(),
                    line: skill.description.clone(),
                    lit: index == self.skills.focus,
                })
            })
            .collect()
    }

    /// **The open list's panel**: above the composer, its left edge on the
    /// composer's, as tall as the rows shown.
    pub(crate) fn skill_panel(&self) -> Option<Rect> {
        // Only over the token it was opened for: a message sent, cleared or
        // put away takes the list with it.
        let open = self.skills.open.as_ref()?;
        if self
            .composer
            .dollar_token()
            .or_else(|| self.composer.slash_token())
            .as_ref()
            != Some(open)
        {
            return None;
        }
        let shown = self
            .skill_rows()
            .len()
            .saturating_sub(self.skills.offset)
            .min(VISIBLE_ROWS);
        if shown == 0 {
            return None;
        }
        let width = self
            .composer_hit_rect
            .w
            .min(elements::PANEL_WIDTH.value)
            .max(1.0);
        let inset = elements::PANEL_INSET.token.px(Profile::Desktop);
        let height = shown as f32 * elements::ROW_HEIGHT.value + inset * 2.0;
        Some(self.above_composer(self.composer_hit_rect.x, width, height))
    }

    /// The open list's frame: where a press lands and which row it is.
    fn skill_frame(&self) -> Option<(Rect, Vec<elements::SkillSpec>, Frame)> {
        let panel = self.skill_panel()?;
        let specs = self.skill_specs();
        let frame = elements::layout(&elements::tree(&specs), panel, FrameCx::root());
        Some((panel, specs, frame))
    }

    /// The match a press or the pointer at `x`, `y` is on.
    pub(crate) fn skill_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (_, specs, frame) = self.skill_frame()?;
        let key = frame.hit_at(x, y)?;
        specs
            .iter()
            .position(|spec| spec.key == key)
            .map(|shown| shown + self.skills.offset)
    }

    /// **A press**: a row picks its skill; a press elsewhere in the panel
    /// is the panel's; one anywhere else shuts it and goes on (`None`).
    pub(crate) fn skill_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if self.skills.open.is_none() {
            return None;
        }
        if let Some(index) = self.skill_row_at(x, y) {
            return Some(self.pick_skill(index));
        }
        if self.skill_panel().is_some_and(|panel| panel.contains(x, y)) {
            return Some(false);
        }
        self.skills.dismissed = true;
        self.close_skill_list();
        None
    }

    /// **The wheel over the open list** moves the rows shown, a row a notch.
    pub(crate) fn skill_scroll(&mut self, x: f32, y: f32, delta: MouseScrollDelta) -> bool {
        if !self.skill_panel().is_some_and(|panel| panel.contains(x, y)) {
            return false;
        }
        // The rows move under the pointer: what it rested on is gone.
        self.skills.tooltip.clear();
        let count = self.skill_rows().len();
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, lines) => lines,
            MouseScrollDelta::PixelDelta(position) => {
                position.y as f32 / elements::ROW_HEIGHT.value
            }
        };
        let step = lines.round() as isize;
        if step == 0 {
            return true;
        }
        let most = count.saturating_sub(VISIBLE_ROWS) as isize;
        self.skills.offset = (self.skills.offset as isize - step).clamp(0, most) as usize;
        true
    }

    /// The hover part for redraws: each row of the open list.
    pub(crate) fn skill_hover_at(&self, x: f32, y: f32) -> Option<(u8, usize)> {
        self.skill_row_at(x, y).map(|index| (20, index))
    }

    /// **A row's whole description, for the pointer at `x`, `y`**: the
    /// shared tooltip, wrapped, beside the list — its anchor is the row's
    /// height across the whole panel, so it stands clear of every row.
    /// `None` off the rows, and on a row whose description fits its line:
    /// a tooltip repeating what the row already shows says nothing. Only
    /// the pointer raises it; the keyboard's row never does.
    pub(crate) fn skill_tooltip_at(&mut self, x: f32, y: f32) -> Option<TooltipContent> {
        let (panel, specs, frame) = self.skill_frame()?;
        let key = frame.hit_at(x, y)?;
        let spec = specs.iter().find(|spec| spec.key == key)?;
        if spec.line.is_empty() {
            return None;
        }
        let row = frame.node(key)?.rect;
        let line = frame
            .named(elements::SKILL_LINE)
            .map(|node| node.rect)
            .find(|line| row.contains(line.x + line.w * 0.5, line.y + line.h * 0.5))?;
        // Measured as the row lays it out: the line's role, at its width.
        let full = self
            .engine
            .simple_layout(&spec.line, roles::META.style())
            .width();
        if full <= line.w {
            return None;
        }
        Some(
            TooltipContent::new(
                crate::model_picker::stable_id(&spec.title) as u64,
                Rect::new(panel.x, row.y, panel.w, row.h),
                spec.line.clone(),
            )
            .wrapped()
            .beside(),
        )
    }

    /// **The pointer moved**: the tooltip follows the row under it, after
    /// the app's hover delay. Answers whether what is drawn changed.
    pub(crate) fn skill_tooltip_hover(&mut self, x: f32, y: f32) -> bool {
        let tip = self.skill_tooltip_at(x, y);
        self.skills.tooltip.set_hover(tip, Instant::now())
    }

    /// **A key went down**: the description goes at once, whichever key.
    pub(crate) fn skill_tooltip_clear(&mut self) -> bool {
        self.skills.tooltip.clear()
    }

    /// The description's tooltip clock, for the surface's own.
    pub(crate) fn skill_tooltip(&self) -> &Tooltip {
        &self.skills.tooltip
    }

    /// Step the description's tooltip: its wait, its fades.
    pub(crate) fn maintain_skill_tooltip(&mut self, now: Instant) -> bool {
        self.skills.tooltip.maintain(now)
    }

    /// **The open list**: its element tree ([`elements`]) laid out with the
    /// pointer and the keyboard on it and painted while the panel's
    /// `FloatingPanel` record is open, so every row nests under the panel in
    /// the inspector.
    pub(crate) fn draw_skill_list(&mut self, canvas: &mut Scene, root: Affine) {
        let Some((panel, specs, targets)) = self.skill_frame() else {
            return;
        };
        let hot = targets.hit_at(self.pointer.0, self.pointer.1);
        let tree = elements::tree(&specs);
        let frame = elements::layout(
            &tree,
            panel,
            FrameCx::root().with_interaction(Interaction {
                hot,
                ..Interaction::default()
            }),
        );
        let record = scene::floating_panel_scope(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(PANEL_RADIUS),
        );
        frame.paint(&tree, canvas, root, &mut self.engine);
        drop(record);
        // Over the list and beside it, never inside its record.
        self.skills.tooltip.draw(
            canvas,
            root,
            &mut self.engine,
            self.viewport,
            Instant::now(),
        );
    }
}
