// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::sidebar::rail_elements::{self, Contents, Item, Rail};
use koru_draw::el::{Frame, Interaction};

impl ExploreView {
    pub(crate) fn toggle_workspace_menu(&mut self) -> bool {
        if self.live().is_some_and(|p| p.kind == PopoverKind::Summary) {
            self.close_popover()
        } else {
            self.open_popover(PopoverKind::Summary)
        }
    }

    pub(crate) fn toggle_profile_menu(&mut self) -> bool {
        if self.live().is_some_and(|p| p.kind == PopoverKind::Profiles) {
            self.close_popover()
        } else {
            self.open_popover(PopoverKind::Profiles)
        }
    }

    pub(crate) fn rail_model(&self) -> Rail {
        use koru_i18n::{Key, fill, t};
        let lang = crate::format::lang();
        let profile = self.profile();
        let workspace = self
            .current
            .and_then(|id| self.workspaces.iter().find(|w| w.id == id));
        let name = workspace
            .map(|w| w.name.as_str())
            .or_else(|| profile.map(|p| p.name.as_str()))
            .unwrap_or(t(lang, Key::Workspaces));
        let current = Item {
            hit: self
                .current
                .map_or(apps_rail::Hit::WaitingWorkspace, apps_rail::Hit::Workspace),
            name: self
                .workspace_waiting
                .clone()
                .unwrap_or_else(|| fill(t(lang, Key::RailCurrentWorkspace), &[name])),
            contents: Contents::Tile {
                name: name.to_owned(),
                icon: workspace
                    .map_or_else(|| profile.and_then(|p| p.icon.clone()), |w| w.icon.clone()),
                color: workspace.map_or_else(
                    || profile.and_then(|p| p.color.clone()),
                    |w| w.color.clone(),
                ),
            },
            selected: self.current.is_some(),
            dimmed: self.current.is_none(),
        };
        let glyph = |hit, name: &str, glyph| Item {
            hit,
            name: name.to_owned(),
            contents: Contents::Glyph(glyph),
            selected: false,
            dimmed: false,
        };
        let views = self
            .rail_modes()
            .into_iter()
            .map(|mode| Item {
                hit: apps_rail::Hit::App(mode),
                name: mode.label_in(lang).to_owned(),
                contents: Contents::Glyph(Self::navigator_mode_icon(mode)),
                selected: mode == self.mode,
                dimmed: false,
            })
            .collect();
        Rail {
            workspaces: vec![current],
            views,
            foot: vec![
                glyph(apps_rail::Hit::Help, t(lang, Key::Help), icons::QUESTION),
                glyph(
                    apps_rail::Hit::Settings,
                    t(lang, Key::TerminalSettings),
                    icons::FADERS,
                ),
                Item {
                    hit: apps_rail::Hit::Profile,
                    name: profile
                        .map_or(t(lang, Key::FloorChipAction), |p| p.name.as_str())
                        .to_owned(),
                    contents: Contents::Tile {
                        name: profile.map_or("", |p| p.name.as_str()).to_owned(),
                        icon: profile.and_then(|p| p.icon.clone()),
                        color: profile.and_then(|p| p.color.clone()),
                    },
                    selected: self.live().is_some_and(|p| p.kind == PopoverKind::Profiles),
                    dimmed: false,
                },
            ],
        }
    }

    pub(crate) fn with_rail_frame<R>(&self, at: Rect, read: impl FnOnce(&Rail, &Frame) -> R) -> R {
        let rail = self.rail_model();
        let layout = crate::input_engine::with(|engine| {
            rail.layout(
                engine,
                &mut self.rail_layout.borrow_mut(),
                at,
                Interaction::default(),
            )
        });
        read(&rail, &layout.frame)
    }

    pub(crate) fn rail_hit(&self, at: Rect, x: f32, y: f32) -> Option<apps_rail::Hit> {
        if at.w <= 0.0 {
            return None;
        }
        self.with_rail_frame(at, |rail, frame| rail.hit(frame, x, y))
    }

    pub(crate) fn rail_controls(&self, at: Rect) -> Vec<apps_rail::Hit> {
        self.with_rail_frame(at, |rail, frame| rail.controls(frame))
    }

    pub(crate) fn rail_focus(&self, at: Rect, index: usize) -> ExploreChromeFocus {
        match self
            .rail_controls(at)
            .get(index)
            .copied()
            .unwrap_or(apps_rail::Hit::WaitingWorkspace)
        {
            apps_rail::Hit::Workspace(id) => ExploreChromeFocus::Workspace(Some(id)),
            apps_rail::Hit::WaitingWorkspace => ExploreChromeFocus::Workspace(None),
            apps_rail::Hit::App(mode) => ExploreChromeFocus::App(
                self.rail_modes()
                    .iter()
                    .position(|m| *m == mode)
                    .unwrap_or(0),
            ),
            apps_rail::Hit::Settings => ExploreChromeFocus::RailSettings,
            apps_rail::Hit::Help => ExploreChromeFocus::RailHelp,
            apps_rail::Hit::Profile => ExploreChromeFocus::RailProfile,
        }
    }

    pub(super) fn paint_rail_elements(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
    ) {
        let rail = self.rail_model();
        let focus = match self.chrome_focus {
            Some(ExploreChromeFocus::Workspace(id)) => {
                Some(id.map_or(apps_rail::Hit::WaitingWorkspace, apps_rail::Hit::Workspace))
            }
            Some(ExploreChromeFocus::App(i)) => {
                self.rail_modes().get(i).copied().map(apps_rail::Hit::App)
            }
            Some(ExploreChromeFocus::RailSettings) => Some(apps_rail::Hit::Settings),
            Some(ExploreChromeFocus::RailHelp) => Some(apps_rail::Hit::Help),
            Some(ExploreChromeFocus::RailProfile) => Some(apps_rail::Hit::Profile),
            _ => None,
        };
        let interaction = Interaction {
            hot: self.rail_hover.map(rail_elements::key),
            focused: focus.map(rail_elements::key),
            ..Interaction::default()
        };
        let layout = rail.layout(engine, &mut self.rail_layout.borrow_mut(), at, interaction);
        let rects = rail.rectangles(&layout.frame, at);
        let carried = self.rail_carried(&rects).map(|(index, rect)| {
            (
                rail_elements::key(apps_rail::Hit::App(rects.apps[index].0)),
                rect,
            )
        });
        let offsets: Vec<_> = rects
            .apps
            .iter()
            .enumerate()
            .map(|(i, (mode, _))| {
                (
                    rail_elements::key(apps_rail::Hit::App(*mode)),
                    self.rail_slides.offset(i),
                )
            })
            .collect();
        layout.paint(&rail, scene, root, engine, &offsets, carried);
    }
}
