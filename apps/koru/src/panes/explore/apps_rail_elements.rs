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
        // The open workspace's square: its own tile, or the Profile's while
        // the catalog has not listed it yet, and the waiting square when no
        // workspace is open. It sits **where the run puts it**, not first.
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
        // **The run keeps its order.** The open workspace used to be lifted
        // to the top, so every switch reshuffled the squares under the
        // pointer; now each square stays where it was put, and the open one
        // is the one drawn selected. With no workspace open, the waiting
        // square leads, as it always has.
        let mut workspaces = Vec::new();
        let mut current = Some(current);
        if self.current.is_none() {
            workspaces.extend(current.take());
        }
        for id in self.rail_workspace_ids() {
            if Some(id) == self.current {
                workspaces.extend(current.take());
                continue;
            }
            if let Some(w) = self.workspaces.iter().find(|w| w.id == id) {
                workspaces.push(Item {
                    hit: apps_rail::Hit::Workspace(w.id),
                    name: fill(t(lang, Key::RailSwitchWorkspace), &[&w.name]),
                    contents: Contents::Tile {
                        name: w.name.clone(),
                        icon: w.icon.clone(),
                        color: w.color.clone(),
                    },
                    selected: false,
                    dimmed: false,
                });
            }
        }
        workspaces.extend(current);
        // The workspace menu's own door, under the squares and above the
        // rule: the squares themselves now only switch.
        workspaces.push(Item {
            hit: apps_rail::Hit::WorkspaceMenu,
            name: t(lang, Key::RailWorkspaceMenu).to_owned(),
            contents: Contents::Glyph(icons::DOTS_THREE),
            selected: self.popover_is(PopoverKind::Summary),
            dimmed: false,
        });
        Rail {
            workspaces,
            views,
            foot: vec![
                Item {
                    contents: if self.help.projection().dot {
                        Contents::NotifiedGlyph(icons::QUESTION)
                    } else {
                        Contents::Glyph(icons::QUESTION)
                    },
                    selected: self.popover_is(PopoverKind::Help),
                    name: if self.help.projection().dot {
                        format!("{} · {}", t(lang, Key::Help), t(lang, Key::WhatsNew))
                    } else {
                        t(lang, Key::Help).to_owned()
                    },
                    ..glyph(apps_rail::Hit::Help, t(lang, Key::Help), icons::QUESTION)
                },
                // App settings are the app's, not a Profile's: they sat at
                // the foot of the Profile menu, behind a press on a square
                // that says nothing about settings. Here they are one press
                // away, between Help and the profile square.
                glyph(
                    apps_rail::Hit::Settings,
                    t(lang, Key::ProfileAppSettings),
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
        let rail = self.rail_model_at(at.h);
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
            apps_rail::Hit::WorkspaceMenu => ExploreChromeFocus::WorkspaceMenu,
            apps_rail::Hit::App(mode) => ExploreChromeFocus::App(
                self.rail_modes()
                    .iter()
                    .position(|m| *m == mode)
                    .unwrap_or(0),
            ),
            apps_rail::Hit::Overflow => ExploreChromeFocus::RailOverflow,
            apps_rail::Hit::Help => ExploreChromeFocus::RailHelp,
            apps_rail::Hit::Settings => ExploreChromeFocus::RailSettings,
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
        let rail = self.rail_model_at(at.h);
        let focus = match self.chrome_focus {
            Some(ExploreChromeFocus::Workspace(id)) => {
                Some(id.map_or(apps_rail::Hit::WaitingWorkspace, apps_rail::Hit::Workspace))
            }
            Some(ExploreChromeFocus::App(i)) => {
                self.rail_modes().get(i).copied().map(apps_rail::Hit::App)
            }
            Some(ExploreChromeFocus::RailOverflow) => Some(apps_rail::Hit::Overflow),
            Some(ExploreChromeFocus::WorkspaceMenu) => Some(apps_rail::Hit::WorkspaceMenu),
            Some(ExploreChromeFocus::RailHelp) => Some(apps_rail::Hit::Help),
            Some(ExploreChromeFocus::RailSettings) => Some(apps_rail::Hit::Settings),
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
        let carried = self
            .rail_carried(&rects)
            .map(|(index, rect)| {
                (
                    rail_elements::key(apps_rail::Hit::App(rects.apps[index].0)),
                    rect,
                )
            })
            .or_else(|| {
                self.workspace_carried(&rects).and_then(|(index, rect)| {
                    rects.workspaces[index]
                        .0
                        .map(|id| (rail_elements::key(apps_rail::Hit::Workspace(id)), rect))
                })
            });
        let mut offsets: Vec<_> = rects
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
        offsets.extend(
            rects
                .workspaces
                .iter()
                .enumerate()
                .filter_map(|(i, (id, _))| {
                    id.map(|id| {
                        (
                            rail_elements::key(apps_rail::Hit::Workspace(id)),
                            self.workspace_slides.offset(i),
                        )
                    })
                }),
        );
        layout.paint(&rail, scene, root, engine, &offsets, carried);
    }
}
