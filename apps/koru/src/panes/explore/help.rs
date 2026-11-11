// SPDX-License-Identifier: MPL-2.0

//! The Help timeline is a projection of the Agent's whole update report.
//! Actions carry identity only; the container reads the report again on press.

use koru_agent_wire::{UpdateReport, UpdateStage};
use koru_draw::el::{
    Action as ElAction, El, ElState, FrameCx, Key, Leaf, Object, Role, col, icon, leaf, row,
    scroll_y, text,
};
use koru_draw::theme::{IconSize, Ink, Line, Radius, Space, alias, metrics, roles};
use koru_i18n::{Key as Words, fill, t};

use super::popover_elements::{PAGE, SURFACE, WORKSPACE_ROW_STATES};
use super::{ExploreView, PopoverKind, PopoverRow};

pub(crate) const LIST: &str = "Help timeline and links";

struct TimelineMark {
    offered: bool,
    /// The first and the last row of the timeline: the line starts at the
    /// first dot and ends at the last, rather than running past either.
    first: bool,
    last: bool,
}
impl Leaf for TimelineMark {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        (Space::S2.px(koru_draw::theme::Profile::Desktop), 0.0)
    }
    fn paint(
        &self,
        scene: &mut vello::Scene,
        root: vello::kurbo::Affine,
        _: &mut koru_draw::text::TextEngine,
        at: koru_draw::geometry::Rect,
        cx: &FrameCx,
        _: ElState,
    ) {
        let x = at.x + at.w * 0.5;
        let radius = Space::S1.px(koru_draw::theme::Profile::Desktop) * 0.5;
        let y = at.y + metrics::ROW_MIN.value * 0.5;
        // **One continuous line** (the user, 2026-11-10): the mark's box is
        // the row's content, and the row pads it by `S1` above and below, so
        // a line drawn within the box broke at every row. It reaches through
        // that padding to meet the next row's, and stops at the end dots.
        let pad = Space::S1.px(koru_draw::theme::Profile::Desktop);
        let top = if self.first { y } else { at.y - pad };
        let bottom = if self.last { y } else { at.y + at.h + pad };
        if bottom > top {
            koru_draw::scene::stroke(
                scene,
                root,
                Line::Divider.of(cx.palette()),
                koru_draw::theme::BORDER_W,
                &vello::kurbo::Line::new((x as f64, top as f64), (x as f64, bottom as f64)),
            );
        }
        koru_draw::scene::fill(
            scene,
            root,
            if self.offered { Ink::Accent } else { Ink::Hint }.of(cx.palette()),
            &vello::kurbo::Circle::new((x as f64, y as f64), radius as f64),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Offer,
    Current,
    /// One published release, by its place in [`Model::releases`]. It takes
    /// the place of [`Action::Current`] once the list has been read.
    Release(u8),
    Releases,
    Feedback,
    GitHub,
    X,
}

/// The control part of a release row; its position is the key's number.
const RELEASE_PART: &str = "help-release";

impl Action {
    /// The fixed rows. Release rows come from [`Model::releases`].
    pub(crate) const ALL: [Self; 6] = [
        Self::Offer,
        Self::Current,
        Self::Releases,
        Self::Feedback,
        Self::GitHub,
        Self::X,
    ];
    pub(crate) fn key(self) -> Key {
        Key::surface(
            SURFACE,
            match self {
                Self::Offer => "help-offer",
                Self::Current => "help-current",
                Self::Release(index) => return Key::model(u128::from(index), RELEASE_PART),
                Self::Releases => "help-releases",
                Self::Feedback => "help-feedback",
                Self::GitHub => "help-github",
                Self::X => "help-x",
            },
        )
    }
    pub(crate) fn from_key(key: Key) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|a| a.key() == key)
            .or(match key.object {
                Object::Model(index) if key.part == RELEASE_PART => {
                    u8::try_from(index).ok().map(Self::Release)
                }
                _ => None,
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OfferAction {
    Download,
    Manual,
    Restart,
    Retry,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Projection {
    pub offer: bool,
    pub action: Option<OfferAction>,
    pub dot: bool,
}

pub(crate) fn project(
    report: Option<&UpdateReport>,
    was_offered: bool,
    notes_unread: bool,
) -> Projection {
    use UpdateStage::*;
    let Some(report) = report else {
        return Projection {
            dot: notes_unread,
            ..Projection::default()
        };
    };
    let offer = (report.stage == Failed && was_offered)
        || (!report.version.trim().is_empty()
            && matches!(
                report.stage,
                Available | Downloading | Verifying | Ready | Installing | Deferred
            ));
    let action = if !offer {
        None
    } else {
        match report.stage {
            Available | Deferred if report.can_install => Some(OfferAction::Download),
            Available | Deferred
                if report
                    .download_url
                    .as_ref()
                    .is_some_and(|url| !url.trim().is_empty()) =>
            {
                Some(OfferAction::Manual)
            }
            Ready => Some(OfferAction::Restart),
            Failed => Some(OfferAction::Retry),
            _ => None,
        }
    };
    Projection {
        offer,
        action,
        dot: notes_unread || (offer && report.stage != Deferred),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Model {
    pub report: Option<UpdateReport>,
    pub was_offered: bool,
    pub notes_unread: bool,
    pub headline: Option<String>,
    /// The published releases, once read. `None` (or empty) keeps the one
    /// row for this build.
    pub releases: Option<Vec<crate::whatsnew::PublishedRelease>>,
}

impl Model {
    /// The rows of the popover, top to bottom: the offered update, then the
    /// releases (or this build's row while they are not known), then links.
    pub(crate) fn rows(&self) -> Vec<Action> {
        let mut rows = Vec::new();
        if self.projection().action.is_some() {
            rows.push(Action::Offer);
        }
        match self.releases.as_deref() {
            Some(releases) if !releases.is_empty() => rows.extend(
                (0..releases.len().min(usize::from(u8::MAX)))
                    .map(|index| Action::Release(index as u8)),
            ),
            _ => rows.push(Action::Current),
        }
        rows.extend([
            Action::Releases,
            Action::Feedback,
            Action::GitHub,
            Action::X,
        ]);
        rows
    }

    pub(crate) fn release(&self, index: u8) -> Option<&crate::whatsnew::PublishedRelease> {
        self.releases.as_ref()?.get(usize::from(index))
    }

    /// Whether `release` is the build that is running: the nightly row on a
    /// nightly build, a version's row on the stable or beta build of it.
    pub(crate) fn is_running(release: &crate::whatsnew::PublishedRelease) -> bool {
        use koru_channel::ReleaseChannel;
        let channel = ReleaseChannel::of_this_build();
        match &release.version {
            None => channel == ReleaseChannel::Nightly,
            Some(version) => {
                matches!(channel, ReleaseChannel::Stable | ReleaseChannel::Beta)
                    && version == koru_channel::build_info().version
            }
        }
    }

    /// A release row's two lines: *Nightly* or the version, then what it is
    /// — built nightly, or the day it was published — and *This version* on
    /// the one that is running.
    pub(crate) fn release_lines(&self, index: u8) -> Option<(String, String)> {
        let release = self.release(index)?;
        let lang = crate::format::lang();
        let (title, mut detail) = match &release.version {
            None => (
                t(lang, Words::HelpReleaseNightly).to_owned(),
                t(lang, Words::HelpReleaseNightlyBody).to_owned(),
            ),
            Some(version) => (
                version.clone(),
                release
                    .published
                    .map(|(year, month, day)| {
                        koru_i18n::time::absolute_date(
                            lang,
                            koru_i18n::time::DateTimeParts {
                                year,
                                month,
                                day,
                                hour: 0,
                                minute: 0,
                                second: 0,
                            },
                            true,
                        )
                    })
                    .unwrap_or_default(),
            ),
        };
        if Self::is_running(release) {
            let this = t(lang, Words::HelpReleaseThisVersion);
            detail = if detail.is_empty() {
                this.to_owned()
            } else {
                format!("{detail} · {this}")
            };
        }
        Some((title, detail))
    }

    pub(crate) fn projection(&self) -> Projection {
        project(self.report.as_ref(), self.was_offered, self.notes_unread)
    }
    pub(crate) fn action_label(&self, action: Action) -> String {
        let lang = crate::format::lang();
        let key = match action {
            Action::Offer => match self.projection().action {
                Some(OfferAction::Download) => Words::Download,
                Some(OfferAction::Manual) => Words::UpdateDownloadManually,
                Some(OfferAction::Restart) => Words::UpdateRestart,
                Some(OfferAction::Retry) => Words::TryAgain,
                None => return self.stage_label(),
            },
            Action::Current | Action::Release(_) => Words::WhatsNew,
            Action::Releases => Words::HelpViewAllReleases,
            Action::Feedback => Words::SendFeedback,
            Action::GitHub => Words::StarOnGitHub,
            Action::X => Words::HelpFollowOnX,
        };
        t(lang, key).to_owned()
    }
    pub(crate) fn current_label(&self) -> String {
        let lang = crate::format::lang();
        self.headline.as_ref().map_or_else(
            || t(lang, Words::NotShippedWithThisBuild).to_owned(),
            |headline| fill(t(lang, Words::HelpCurrentRelease), &[headline]),
        )
    }
    pub(crate) fn stage_label(&self) -> String {
        let Some(report) = &self.report else {
            return String::new();
        };
        let lang = crate::format::lang();
        use UpdateStage::*;
        let key = match report.stage {
            Available | Deferred if self.projection().action.is_none() => Words::UpdateRowNoRoute,
            Available => Words::UpdateRowAvailable,
            Deferred => Words::UpdateRowDeferred,
            Downloading => {
                let percent = if report.total == 0 {
                    0
                } else {
                    ((report.received as u128 * 100 / report.total as u128).min(100)) as u64
                };
                return fill(
                    t(lang, Words::UpdateRowDownloading),
                    &[&koru_i18n::number::decimal(lang, percent)],
                );
            }
            Verifying => Words::UpdateRowVerifying,
            Ready if report.active_session_count > 0 => {
                return fill(
                    t(lang, Words::UpdateReadyWithSessions),
                    &[&koru_i18n::number::decimal(
                        lang,
                        report.active_session_count as u64,
                    )],
                );
            }
            Ready => Words::UpdateRowReady,
            Installing => Words::UpdateRowInstalling,
            Failed => {
                return report
                    .message
                    .clone()
                    .unwrap_or_else(|| t(lang, Words::TryAgain).to_owned());
            }
            _ => return String::new(),
        };
        t(lang, key).to_owned()
    }
}

impl ExploreView {
    pub(crate) fn set_help(&mut self, model: Model) {
        if self.help == model {
            return;
        }
        let focused = self
            .live()
            .filter(|p| p.kind == PopoverKind::Help)
            .and_then(|p| p.hover)
            .and_then(|i| self.rows_for(PopoverKind::Help).get(i).copied());
        self.help = model;
        if let Some(focused) = focused {
            let rows = self.rows_for(PopoverKind::Help);
            // A row that went (this build's, once the releases arrive)
            // hands focus to the first release, or back to this build's.
            let index = rows.iter().position(|row| *row == focused).or_else(|| {
                rows.iter().position(|row| {
                    matches!(row, PopoverRow::Help(Action::Current | Action::Release(0)))
                })
            });
            if let Some(p) = &mut self.popover {
                p.hover = index;
            }
        }
    }

    pub(crate) fn help_page(&self, scroll: f32) -> El {
        let mut list = scroll_y(LIST).fill().offset(scroll);
        // The timeline's rows, gathered first so each knows whether it is the
        // first or the last: the line runs between their dots.
        let mut timeline: Vec<(Action, String, String, bool)> = Vec::new();
        if self.help.projection().offer {
            let version = self
                .help
                .report
                .as_ref()
                .map(|r| r.version.clone())
                .unwrap_or_default();
            timeline.push((
                Action::Offer,
                version,
                self.help.stage_label(),
                self.help.projection().action.is_some(),
            ));
        }
        let releases = self.help.releases.as_ref().map_or(0, Vec::len);
        if releases == 0 {
            timeline.push((
                Action::Current,
                koru_channel::build_info().version.to_owned(),
                self.help.current_label(),
                true,
            ));
        }
        for index in 0..releases.min(usize::from(u8::MAX)) {
            let index = index as u8;
            if let Some((title, detail)) = self.help.release_lines(index) {
                timeline.push((Action::Release(index), title, detail, true));
            }
        }
        let count = timeline.len();
        for (at, (action, version, description, enabled)) in timeline.into_iter().enumerate() {
            list = list.child(self.help_timeline_row(
                action,
                &version,
                description,
                enabled,
                (at == 0, at + 1 == count),
            ));
        }
        for action in [
            Action::Releases,
            Action::Feedback,
            Action::GitHub,
            Action::X,
        ] {
            if action == Action::Feedback {
                list = list.child(super::workspace_menu_elements::rule());
            }
            let label = self.help.action_label(action);
            list = list.child(
                row("Help link")
                    .id(action.key())
                    .min_h(metrics::ROW_MIN)
                    .pad_x(alias::NAV_PAD)
                    .radius(Radius::M)
                    .states(&WORKSPACE_ROW_STATES)
                    .hit()
                    .focusable()
                    .action(ElAction::Click)
                    .action(ElAction::Focus)
                    .semantics(Role::MenuItem)
                    .name_text(&label)
                    .child(text(label).role(roles::ROW).fill()),
            );
        }
        col("Help page")
            .id(PAGE)
            .semantics(Role::Menu)
            .name(Words::Help)
            .pad_y(alias::PANEL_PAD)
            .child(
                text(t(crate::format::lang(), Words::WhatsNew).to_uppercase())
                    .id(Key::surface(SURFACE, "help-heading"))
                    .semantics(Role::Heading)
                    .name(Words::WhatsNew)
                    .role(roles::POPOVER_HEADING)
                    .ink(Ink::Hint)
                    .min_h(metrics::ROW_MIN)
                    .pad_x(alias::NAV_PAD),
            )
            .child(list)
    }

    fn help_timeline_row(
        &self,
        action: Action,
        version: &str,
        description: String,
        enabled: bool,
        (first, last): (bool, bool),
    ) -> El {
        let label = self.help.action_label(action);
        let mut item = row("Help release row")
            .id(action.key())
            .min_h(metrics::PROFILE_ROW_MIN)
            .pad_x(alias::NAV_PAD)
            .pad_y(Space::S1)
            .gap(Space::S2)
            .radius(Radius::M)
            .states(&WORKSPACE_ROW_STATES)
            .semantics(if enabled { Role::MenuItem } else { Role::Group })
            .name_text(if enabled {
                format!("{version} · {description} · {label}")
            } else {
                format!("{version} · {description}")
            });
        if enabled {
            item = item
                .hit()
                .focusable()
                .action(ElAction::Click)
                .action(ElAction::Focus);
        }
        item = item.child(
            leaf(
                "Release timeline",
                TimelineMark {
                    offered: action == Action::Offer,
                    first,
                    last,
                },
            )
            .decorative()
            .w(Space::S2),
        );
        item = item.child(
            col("Release details")
                .fill()
                .gap(Space::S1)
                .child(
                    text(version)
                        .role(roles::ROW)
                        .emphasis(true)
                        .fill()
                        .truncate(),
                )
                .child(
                    text(description)
                        .role(roles::HINT)
                        .ink(Ink::Secondary)
                        .fill(),
                ),
        );
        if enabled {
            item = item.child(if matches!(action, Action::Current | Action::Release(_)) {
                icon(koru_draw::icons::CARET_RIGHT, IconSize::L)
                    .decorative()
                    .ink(Ink::Secondary)
            } else {
                text(label).role(roles::HINT).ink(Ink::Accent)
            });
        }
        item
    }
}

#[cfg(test)]
mod tests;
