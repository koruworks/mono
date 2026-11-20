// SPDX-License-Identifier: MPL-2.0

//! Visible runtime choices using the same Chip and provider marks as Settings.
use super::*;
use koru_ai_ui::agent_catalog::config::picker::{elements::stable_id, mark::draw_mark};
use koru_draw::brand::ProfileMark;
use koru_draw::theme::Profile;
use koru_ui::chip::Chip;

pub(super) fn agent_key(provider: &AiProviderId) -> Key {
    Key::model(stable_id(provider.as_str()), "new-bot-agent")
}

struct AgentChip {
    label: String,
    mark: ProfileMark,
    selected: bool,
    width: f32,
}

impl AgentChip {
    fn chip(&self) -> Chip<'_> {
        Chip::new(&self.label, self.selected)
            .leading(IconSize::S.px(Profile::Desktop) + Space::S1_5.px(Profile::Desktop))
    }
}

impl Leaf for AgentChip {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.width, Chip::H)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let chip = self.chip();
        chip.draw(
            scene,
            root,
            engine,
            at,
            state.hovered && !state.disabled,
            state.focused,
        );
        let slot = chip.leading_rect(at);
        let size = IconSize::S
            .px(Profile::Desktop)
            .min((at.w - Chip::PAD_X * 2.0).max(0.0));
        draw_mark(
            scene,
            root,
            engine,
            self.mark,
            Rect::new(slot.x, slot.y + (slot.h - size) * 0.5, size, size),
            if self.selected {
                cx.palette().text
            } else {
                cx.palette().text_dim
            },
        );
    }
}

impl NewBotDialog {
    pub(super) fn agent_problem(&self) -> Option<String> {
        self.form()
            .and_then(|form| form.notice())
            .filter(|words| crate::bots::creation::names_the_agent(*words))
            .map(|words| self.host.create_notice_words(words))
            .or_else(|| self.host.agent_readiness_words())
    }

    pub(super) fn settings_link(
        &self,
        name: &'static str,
        key: Key,
        label: Words,
        locked: bool,
    ) -> El {
        text(self.words(label))
            .named(name)
            .id(key)
            .role(roles::META)
            .ink(Ink::Accent)
            .pad_x(Space::S1)
            .pad_y(Space::S1)
            .radius(Radius::S)
            .states(&QUIET_STATES)
            .hit()
            .focusable()
            .disabled(locked)
            .action(Action::Click)
            .action(Action::Focus)
            .semantics(Role::Button)
            .name(label)
    }

    pub(super) fn panel_width(&self) -> f32 {
        let margin = Space::S4.px(Profile::Desktop);
        DIALOG_W
            .value
            .min((self.viewport.0 - margin * 2.0).max(0.0))
    }

    pub(super) fn agent_of(&self, key: Key) -> Option<usize> {
        self.agents()
            .iter()
            .position(|(id, _)| agent_key(id) == key)
    }

    pub(super) fn runtime_choices(&self, engine: &mut TextEngine, locked: bool) -> El {
        let chosen = self.host.agent_provider();
        let available = (self.panel_width() - Space::S6.px(Profile::Desktop) * 2.0).max(1.0);
        let gap = Space::S1_5.px(Profile::Desktop);
        let mut group = col("New bot agents")
            .id(AGENTS)
            .gap(Space::S2)
            .semantics(Role::Group)
            .name(Words::BotsNewBotAgent)
            .child(
                row("New bot agents heading")
                    .items_center()
                    .gap(Space::S2)
                    .child(
                        text(self.words(Words::BotsNewBotAgent))
                            .named("New bot agents label")
                            .role(roles::META)
                            .ink(Ink::Secondary)
                            .fill(),
                    )
                    .child(self.settings_link(
                        "New bot manage agents",
                        MANAGE_AGENTS,
                        Words::AiPickerManageAgents,
                        locked,
                    )),
            );
        let mut lines = col("New bot agent choices").gap(Space::S1_5);
        let mut line = row("New bot agent row").gap(Space::S1_5);
        let mut used = 0.0;
        for agent in self
            .host
            .catalog
            .control_agents(&self.host.agent_settings)
            .into_iter()
            .filter(|agent| !agent.hidden)
        {
            let selected = chosen.as_ref() == Some(&agent.id);
            let mut chip = AgentChip {
                label: agent.label.to_owned(),
                mark: agent.mark,
                selected,
                width: 0.0,
            };
            chip.width = chip.chip().width(engine).min(available);
            if used > 0.0 && used + gap + chip.width > available {
                lines = lines.child(line);
                line = row("New bot agent row").gap(Space::S1_5);
                used = 0.0;
            }
            used += if used > 0.0 {
                gap + chip.width
            } else {
                chip.width
            };
            line = line.child(
                leaf("New bot agent", chip)
                    .semantic()
                    .id(agent_key(&agent.id))
                    .hit()
                    .focusable()
                    .disabled(locked)
                    .selected(selected)
                    .action(Action::Click)
                    .action(Action::Focus)
                    .semantics(Role::Button)
                    .name_text(agent.label.to_owned()),
            );
        }
        if used > 0.0 {
            lines = lines.child(line);
        }
        group = group.child(lines);
        if let Some(readiness) = self.agent_problem() {
            group = group.child(Self::sentence(
                "New bot agent readiness",
                AGENT_READINESS,
                readiness,
                Ink::Secondary,
            ));
            if !chosen.as_ref().is_some_and(|provider| {
                matches!(
                    self.host.catalog.standing(provider),
                    crate::bots::catalog::Standing::Checking
                )
            }) {
                group = group.child(
                    row("New bot agent help")
                        .child(self.settings_link(
                            "New bot agent settings",
                            AGENT_SETTINGS,
                            Words::SettingsTab,
                            locked,
                        ))
                        .child(spacer()),
                );
            }
        }
        group
    }
}
