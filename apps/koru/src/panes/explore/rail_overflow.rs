// SPDX-License-Identifier: MPL-2.0

//! Height-dependent projection of the one persisted view order.

use super::*;
use crate::panes::sidebar::rail_elements::{Contents, Item, Rail};

#[derive(Debug, PartialEq)]
pub(crate) struct ViewProjection {
    pub visible: Vec<NavigatorMode>,
    pub folded: Vec<NavigatorMode>,
    pub promoted: Option<NavigatorMode>,
}

fn project(modes: &[NavigatorMode], active: NavigatorMode, capacity: usize) -> ViewProjection {
    if modes.len() <= capacity {
        return ViewProjection {
            visible: modes.to_vec(),
            folded: vec![],
            promoted: None,
        };
    }
    // One slot belongs to More views. The active view occupies the last
    // remaining slot when its stored position is folded.
    let count = capacity.saturating_sub(1).max(1);
    let mut visible = modes[..count.min(modes.len())].to_vec();
    let promoted = (!visible.contains(&active) && modes.contains(&active)).then_some(active);
    if let Some(active) = promoted {
        *visible.last_mut().unwrap() = active;
    }
    let folded = modes
        .iter()
        .copied()
        .filter(|mode| !visible.contains(mode))
        .collect();
    ViewProjection {
        visible,
        folded,
        promoted,
    }
}

#[cfg(test)]
mod tests;

impl ExploreView {
    pub(crate) fn view_projection(&self, height: f32) -> ViewProjection {
        let rail = self.rail_model();
        project(&self.rail_modes(), self.mode, rail.view_capacity(height))
    }

    pub(crate) fn rail_model_at(&self, height: f32) -> Rail {
        let mut rail = self.rail_model();
        let projection = project(&self.rail_modes(), self.mode, rail.view_capacity(height));
        rail.views = projection
            .visible
            .iter()
            .filter_map(|mode| {
                rail.views
                    .iter()
                    .find(|item| item.hit == apps_rail::Hit::App(*mode))
                    .cloned()
            })
            .collect();
        if !projection.folded.is_empty() {
            rail.views.push(Item {
                hit: apps_rail::Hit::Overflow,
                name: koru_i18n::t(crate::format::lang(), koru_i18n::Key::RailMoreViews).to_owned(),
                contents: Contents::Glyph(icons::DOTS_THREE),
                selected: false,
                dimmed: false,
            });
        }
        rail
    }
}
