// SPDX-License-Identifier: MPL-2.0

//! **Welcome back / profile chooser modal** (`Screen::ExistingProfiles` in
//! `workspace_modal` mode): eyebrow with provider + email + *Not you?*, circular
//! avatar + *Welcome back, {Name}*, grouped rounded list of account profiles
//! with dark rounded checkmarks and profile tiles, full-width primary *Sync N
//! profiles* button, and *Create a new profile · Continue offline* footer links.

use super::welcome_modal::leaves::LinkLeaf;
use super::welcome_modal::tokens::*;
use super::*;

use koru_draw::el::{FrameCx, Leaf};
use koru_draw::tile::Tile;
use koru_draw::{inspect, scene, theme};
use koru_i18n::{Key as T, t};
use koru_ui::avatar::KoruAvatar;
use koru_ui::button::{ButtonRole, draw_button_enabled};
use vello::kurbo::{Arc as KurboArc, BezPath, Line, Point, Stroke, Vec2};

/// Deterministic accent palette names so hosted profiles without a persisted
/// appearance color still wear a distinct, stable tile in the chooser list.
const PROFILE_TILE_COLORS: [&str; 6] = ["purple", "teal", "blue", "amber", "green", "rose"];

const AVATAR_SIZE: f32 = 44.0;
const AVATAR_GAP: f32 = 14.0;
const CHECKBOX_SIZE: f32 = 20.0;
const CHECKBOX_RADIUS: f32 = 5.0;
const TILE_SIZE: f32 = 36.0;
const ROW_PAD_X: f32 = 16.0;
const ROW_PAD_Y: f32 = 13.0;
const ROW_INNER_GAP: f32 = 12.0;
const LIST_RADIUS: f32 = 12.0;
const PRIMARY_H: f32 = 44.0;
const REFRESH_ICON_SIZE: f32 = 14.0;
const REFRESH_ICON_GAP: f32 = 5.0;

pub(super) struct ProfilesModalRow {
    pub(super) rect: Rect,
    pub(super) source: String,
    pub(super) source_height: f32,
    pub(super) status: String,
    pub(super) action: Option<(Hit, Rect, &'static str)>,
}

pub(super) struct ProfilesModalLayout {
    pub(super) card: Rect,
    pub(super) eyebrow_y: f32,
    pub(super) not_you: Rect,
    pub(super) avatar: Rect,
    pub(super) heading_x: f32,
    pub(super) heading_y: f32,
    pub(super) heading_w: f32,
    pub(super) body: Rect,
    pub(super) section_y: f32,
    pub(super) refresh: Option<Rect>,
    pub(super) list_box: Rect,
    pub(super) rows: Vec<ProfilesModalRow>,
    pub(super) content_height: f32,
    pub(super) primary: Rect,
    pub(super) primary_label: String,
    pub(super) primary_enabled: bool,
    pub(super) cancel: Option<Rect>,
    pub(super) create_new: Rect,
    pub(super) dot_x: f32,
    pub(super) continue_offline: Rect,
    pub(super) footer_y: f32,
}

fn tile_color_for(profile_id: &str) -> &'static str {
    let hash = profile_id
        .bytes()
        .fold(0usize, |acc, b| acc.wrapping_mul(31).wrapping_add(b as usize));
    PROFILE_TILE_COLORS[hash % PROFILE_TILE_COLORS.len()]
}

fn first_name(full: &str) -> &str {
    let trimmed = full.trim();
    trimmed.split_whitespace().next().unwrap_or(trimmed)
}

impl OnboardingView {
    fn profiles_modal_provider(&self) -> BrowserProvider {
        if let Some(provider) = self
            .account_profiles
            .first()
            .and_then(|row| row.auth_identities.first())
            .map(|auth| auth.provider.as_str())
        {
            return match provider {
                "google" => BrowserProvider::Google,
                "apple" => BrowserProvider::Apple,
                _ => BrowserProvider::Email,
            };
        }
        self.browser_provider
    }

    fn profiles_modal_email(&self) -> Option<String> {
        self.account_email
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.account_profiles
                    .iter()
                    .flat_map(|row| row.auth_identities.iter())
                    .filter_map(|auth| auth.verified_email.as_deref())
                    .map(str::trim)
                    .find(|email| !email.is_empty())
                    .map(str::to_owned)
            })
    }

    fn profiles_modal_eyebrow(&self) -> String {
        let lang = self.language;
        let provider_key = match self.profiles_modal_provider() {
            BrowserProvider::Google => T::ProfileChooserEyebrowGoogle,
            BrowserProvider::Apple => T::ProfileChooserEyebrowApple,
            BrowserProvider::Email => T::ProfileChooserEyebrowEmail,
        };
        let label = t(lang, provider_key);
        match self.profiles_modal_email() {
            Some(email) => format!("{label} · {email}"),
            None => label.to_owned(),
        }
    }

    fn profiles_modal_owner_name(&self) -> String {
        if let Some(name) = self
            .account_owner_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            return name.to_owned();
        }
        let typed = self.owner_name.borrow().value();
        if !typed.trim().is_empty() {
            return typed.trim().to_owned();
        }
        String::new()
    }

    fn profiles_modal_heading(&self) -> String {
        let lang = self.language;
        let owner = self.profiles_modal_owner_name();
        let first = first_name(&owner);
        if first.is_empty() {
            t(lang, T::ProfileChooserWelcomeBack).to_owned()
        } else {
            koru_i18n::fill(t(lang, T::ProfileChooserWelcomeBackName), &[first])
        }
    }

    fn profiles_modal_body(&self) -> String {
        self.selection
            .notice
            .map(|key| t(self.language, key).to_owned())
            .or_else(|| self.error.clone())
            .unwrap_or_else(|| t(self.language, T::ProfileChooserModalBody).to_owned())
    }

    fn profiles_modal_primary_label(&self) -> String {
        let lang = self.language;
        if self.selection.waiting || self.selection.syncing_workspaces {
            return t(lang, T::ProfileChooserPending).to_owned();
        }
        let count = self.selection_ids().len();
        if count == 0 {
            t(lang, T::ProfileChooserSyncNone).to_owned()
        } else if self.enrollment_selection_ids().is_empty() {
            t(lang, T::ProfileChooserOpen).to_owned()
        } else if count == 1 {
            t(lang, T::ProfileChooserSyncOne).to_owned()
        } else {
            koru_i18n::fill(t(lang, T::ProfileChooserSyncMany), &[&count.to_string()])
        }
    }

    pub(super) fn profiles_modal_layout(&mut self) -> ProfilesModalLayout {
        let lang = self.language;
        let area = self.setup_area();
        let margin = 24.0_f32;
        let pad = 28.0_f32;
        let card_w = WELCOME_CARD_MAX_W.value.min((area.w - margin * 2.0).max(280.0));
        let inner_w = (card_w - pad * 2.0).max(220.0);

        let not_you_label = t(lang, T::ProfileChooserNotYou);
        let not_you_w = LinkLeaf::new(&mut self.engine, not_you_label, false, true, false, false)
            .width;

        let heading_text = self.profiles_modal_heading();
        let heading_style = theme::TextStyle::new(
            theme::role::subtitle().metrics,
            theme::FontWeight::SemiBold,
        );
        let heading_w = (inner_w - AVATAR_SIZE - AVATAR_GAP).max(120.0);
        let heading_h = self
            .engine
            .simple_layout_wrapped(&heading_text, heading_style, heading_w)
            .height();
        let greeting_h = AVATAR_SIZE.max(heading_h);

        let body_text = self.profiles_modal_body();
        let body_h = self
            .engine
            .simple_layout_wrapped(&body_text, theme::role::row(), inner_w)
            .height();

        let refresh_label = t(lang, T::ProfileChooserRefreshShort);
        let refresh_style =
            theme::TextStyle::new(theme::role::meta().metrics, theme::FontWeight::Medium);
        let refresh_w = REFRESH_ICON_SIZE
            + REFRESH_ICON_GAP
            + self.engine.simple_layout(refresh_label, refresh_style).width()
            + 4.0;

        let create_label = t(lang, T::ProfileChooserCreate);
        let create_w = LinkLeaf::new(&mut self.engine, create_label, false, true, false, false)
            .width;
        let offline_label = t(lang, T::AccountContinueOffline);
        let offline_w = LinkLeaf::new(&mut self.engine, offline_label, false, true, false, false)
            .width;
        let dot_w = self.engine.simple_layout(" · ", theme::role::meta()).width();

        let show_cancel = self.selection_has_unfinished() && !self.selection.waiting;
        let cancel_h = if show_cancel {
            WELCOME_LINK_H.value + 8.0
        } else {
            0.0
        };

        // Compute rows at inner_w first so we know total content height.
        let mut row_specs = Vec::with_capacity(self.account_profiles.len());
        let mut total_rows_h = 0.0_f32;
        for index in 0..self.account_profiles.len() {
            let here = self.account_profile_sources[index] == ProfileSource::Local;
            let source = match self.account_profile_sources[index] {
                ProfileSource::Local => t(lang, T::ProfileChooserLocal),
                ProfileSource::TrustedDevice => t(lang, T::ProfileChooserReadyToSync),
                ProfileSource::Unavailable => t(lang, T::ProfileChooserDevicesOffline),
            }
            .to_owned();
            let row_action = if here {
                Some((Hit::OpenExisting(index), t(lang, T::ProfileChooserOpen)))
            } else if self.member_retry(index) {
                Some((Hit::RetryExisting(index), t(lang, T::ProfileChooserRetry)))
            } else {
                None
            };
            let action_w = row_action.map_or(0.0, |(_, label)| {
                koru_ui::button::button_width(&mut self.engine, label).min(130.0) + 12.0
            });
            let left_prefix = ROW_PAD_X
                + if here {
                    0.0
                } else {
                    CHECKBOX_SIZE + ROW_INNER_GAP
                }
                + TILE_SIZE
                + ROW_INNER_GAP;
            let text_w = (inner_w - left_prefix - ROW_PAD_X - action_w).max(120.0);
            let source_h = self
                .engine
                .simple_layout_wrapped(&source, theme::role::meta(), text_w)
                .height();
            let row = self
                .account_state
                .enrollments
                .iter()
                .find(|row| row.profile_id == self.account_profiles[index].profile_id);
            let mut status = row
                .map(|row| {
                    if row.state == koru_agent_wire::EnrollmentState::Complete {
                        t(lang, T::ProfileChooserComplete).to_owned()
                    } else if row.failure_code.is_some() {
                        let projected = crate::account_ui::state_for_profile(
                            &self.account_state,
                            Some(&row.profile_id),
                        );
                        crate::account_ui::project_for_lang(&projected, None, self.language)
                            .title
                            .to_owned()
                    } else {
                        t(lang, T::ProfileChooserConnecting).to_owned()
                    }
                })
                .unwrap_or_default();
            if let Some(at) = row
                .and_then(|row| row.retry_at_millis)
                .and_then(chrono::DateTime::from_timestamp_millis)
            {
                let time = at
                    .with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string();
                status.push('\n');
                status.push_str(&koru_i18n::fill(
                    t(lang, T::ProfileChooserRetryAt),
                    &[&time],
                ));
            }
            let status_h = if status.is_empty() {
                0.0
            } else {
                self.engine
                    .simple_layout_wrapped(&status, theme::role::meta(), text_w)
                    .height()
                    + 4.0
            };
            let title_h = theme::role::row().metrics.line_height;
            let text_block_h = title_h + 2.0 + source_h + status_h;
            let row_h = (ROW_PAD_Y * 2.0 + text_block_h.max(TILE_SIZE)).max(64.0);
            total_rows_h += row_h;
            row_specs.push((row_h, source, source_h, status, row_action));
        }

        // Fixed vertical chrome height around the profile list box.
        let eyebrow_h = WELCOME_LINK_H.value;
        let section_h = WELCOME_LINK_H.value;
        let top_chrome = pad + eyebrow_h + 12.0 + greeting_h + 14.0 + body_h + 22.0 + section_h + 10.0;
        let bottom_chrome = 20.0 + PRIMARY_H + 14.0 + cancel_h + WELCOME_LINK_H.value + pad;
        let max_card_h = (area.h - margin * 2.0).max(360.0);
        let max_list_h = (max_card_h - top_chrome - bottom_chrome).max(120.0);
        let natural_card_h = (top_chrome + total_rows_h.min(max_list_h) + bottom_chrome)
            .max(WELCOME_MODAL_MIN_H.value)
            .min(max_card_h);
        let list_h = (natural_card_h - top_chrome - bottom_chrome)
            .min(total_rows_h.max(64.0))
            .max(64.0);

        let card_x = area.x + (area.w - card_w) * 0.5;
        let card_y = area.y + (area.h - natural_card_h) * 0.5;
        let card = Rect::new(card_x, card_y, card_w, natural_card_h);
        let x0 = card.x + pad;

        let eyebrow_y = card.y + pad;
        let not_you = Rect::new(x0 + inner_w - not_you_w, eyebrow_y, not_you_w, eyebrow_h);

        let greeting_y = eyebrow_y + eyebrow_h + 12.0;
        let avatar = Rect::new(
            x0,
            greeting_y + (greeting_h - AVATAR_SIZE) * 0.5,
            AVATAR_SIZE,
            AVATAR_SIZE,
        );
        let heading_x = x0 + AVATAR_SIZE + AVATAR_GAP;
        let heading_y = greeting_y + (greeting_h - heading_h) * 0.5;

        let body_y = greeting_y + greeting_h + 14.0;
        let body = Rect::new(x0, body_y, inner_w, body_h);

        let section_y = body_y + body_h + 22.0;
        let refresh = (!self.selection.locked()).then(|| {
            Rect::new(x0 + inner_w - refresh_w, section_y, refresh_w, section_h)
        });

        let list_y = section_y + section_h + 10.0;
        let list_box = Rect::new(x0, list_y, inner_w, list_h);

        let mut rows = Vec::with_capacity(row_specs.len());
        let mut cursor_y = 0.0_f32;
        let row_w = if total_rows_h > list_h {
            (inner_w - theme::SCROLLBAR_GUTTER_W).max(1.0)
        } else {
            inner_w
        };
        for (row_h, source, source_h, status, row_action) in row_specs {
            let ry = list_box.y + cursor_y - self.existing_profile_scroll;
            let action = row_action.map(|(hit, label)| {
                let bw = koru_ui::button::button_width(&mut self.engine, label).min(130.0);
                let bh = 30.0_f32;
                let rect = Rect::new(
                    list_box.x + row_w - ROW_PAD_X - bw,
                    ry + (row_h - bh) * 0.5,
                    bw,
                    bh,
                );
                (hit, rect, label)
            });
            rows.push(ProfilesModalRow {
                rect: Rect::new(list_box.x, ry, row_w, row_h),
                source,
                source_height: source_h,
                status,
                action,
            });
            cursor_y += row_h;
        }

        let footer_y = card.y + card.h - pad - WELCOME_LINK_H.value;
        let footer_total_w = create_w + dot_w + offline_w;
        let footer_start_x = card.x + (card.w - footer_total_w) * 0.5;
        let create_new = Rect::new(footer_start_x, footer_y, create_w, WELCOME_LINK_H.value);
        let dot_x = footer_start_x + create_w;
        let continue_offline = Rect::new(
            dot_x + dot_w,
            footer_y,
            offline_w,
            WELCOME_LINK_H.value,
        );

        let cancel = show_cancel.then(|| {
            let cancel_label = t(lang, T::ProfileChooserCancel);
            let cancel_w =
                LinkLeaf::new(&mut self.engine, cancel_label, false, true, false, false).width;
            Rect::new(
                card.x + (card.w - cancel_w) * 0.5,
                footer_y - 8.0 - WELCOME_LINK_H.value,
                cancel_w,
                WELCOME_LINK_H.value,
            )
        });

        let primary_y = cancel.map_or(footer_y - 14.0 - PRIMARY_H, |c| c.y - 10.0 - PRIMARY_H);
        let primary = Rect::new(x0, primary_y, inner_w, PRIMARY_H);
        let primary_enabled = !self.selection.locked() && !self.selection_ids().is_empty();
        let primary_label = self.profiles_modal_primary_label();

        ProfilesModalLayout {
            card,
            eyebrow_y,
            not_you,
            avatar,
            heading_x,
            heading_y,
            heading_w,
            body,
            section_y,
            refresh,
            list_box,
            rows,
            content_height: total_rows_h,
            primary,
            primary_label,
            primary_enabled,
            cancel,
            create_new,
            dot_x,
            continue_offline,
            footer_y,
        }
    }

    pub(super) fn profiles_modal_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        let layout = self.profiles_modal_layout();
        let controls = self.selection_controls();
        if layout.not_you.contains(x, y) {
            return Some(Hit::SwitchAccount);
        }
        if let Some(refresh) = layout.refresh
            && refresh.contains(x, y)
            && controls.contains(&Hit::RefreshProfiles)
        {
            return Some(Hit::RefreshProfiles);
        }
        if layout.primary_enabled && layout.primary.contains(x, y) {
            return Some(Hit::Primary);
        }
        if let Some(cancel) = layout.cancel
            && cancel.contains(x, y)
            && controls.contains(&Hit::CancelSelection)
        {
            return Some(Hit::CancelSelection);
        }
        if layout.create_new.contains(x, y) {
            return Some(Hit::CreateNew);
        }
        if layout.continue_offline.contains(x, y) {
            return Some(Hit::ContinueOffline);
        }
        if !layout.list_box.contains(x, y) {
            return None;
        }
        for (index, row) in layout.rows.iter().enumerate() {
            if let Some((hit, rect, _)) = row.action
                && rect.contains(x, y)
            {
                return Some(hit);
            }
            if row.rect.contains(x, y) && controls.contains(&Hit::Existing(index)) {
                return Some(Hit::Existing(index));
            }
        }
        None
    }

    pub(super) fn draw_profiles_modal(&mut self, canvas: &mut Scene, root: Affine) {
        let lang = self.language;
        let layout = self.profiles_modal_layout();
        let _panel = scene::floating_panel_scope(
            canvas,
            root,
            layout.card.x,
            layout.card.y,
            layout.card.w,
            layout.card.h,
            &scene::Panel::popover(WELCOME_CARD_RADIUS.value),
        );
        let p = theme::p();
        let cx = FrameCx::root();
        let controls = self.controls();
        let focused = self
            .keyboard_focus
            .then(|| controls.get(self.focus).copied())
            .flatten();

        // 1. Eyebrow row: Signed in with Google · email | Not you?
        let eyebrow = self.profiles_modal_eyebrow();
        let eyebrow_layout = self.engine.simple_layout(&eyebrow, theme::role::meta());
        let eyebrow_h = scene::first_line_height(&eyebrow_layout);
        scene::draw_layout(
            canvas,
            root,
            &eyebrow_layout,
            layout.body.x,
            layout.eyebrow_y + (WELCOME_LINK_H.value - eyebrow_h) * 0.5,
            Some(p.text_dim),
        );
        let not_you_leaf = LinkLeaf::new(
            &mut self.engine,
            t(lang, T::ProfileChooserNotYou),
            false,
            true,
            self.hover == Some(Hit::SwitchAccount),
            focused == Some(Hit::SwitchAccount),
        );
        not_you_leaf.paint(
            canvas,
            root,
            &mut self.engine,
            layout.not_you,
            &cx,
            Default::default(),
        );

        // 2. Greeting row: circular KoruAvatar + Welcome back, Steven
        let owner_name = self.profiles_modal_owner_name();
        KoruAvatar::initial(&owner_name).draw_in(canvas, root, &mut self.engine, layout.avatar, &p);
        let heading = self.profiles_modal_heading();
        let heading_style = theme::TextStyle::new(
            theme::role::subtitle().metrics,
            theme::FontWeight::SemiBold,
        );
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            &heading,
            layout.heading_x,
            layout.heading_y,
            layout.heading_w,
            heading_style,
            "theme::role::subtitle() SemiBold",
            p.text,
        );

        // 3. Lead copy (or refusal notice)
        let body = self.profiles_modal_body();
        let body_color = if self.selection.notice.is_some() || self.error.is_some() {
            p.notice_fg
        } else {
            p.text_dim
        };
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            &body,
            layout.body.x,
            layout.body.y,
            layout.body.w,
            theme::role::row(),
            "theme::role::row()",
            body_color,
        );

        // 4. Section header row: Profiles on your account | ↻ Refresh
        let section_label = if self.selection.waiting {
            t(lang, T::ProfileChooserPending)
        } else {
            t(lang, T::ProfileChooserSectionLabel)
        };
        let sec_layout = self.engine.simple_layout(section_label, theme::role::meta());
        let sec_h = scene::first_line_height(&sec_layout);
        scene::draw_layout(
            canvas,
            root,
            &sec_layout,
            layout.body.x,
            layout.section_y + (WELCOME_LINK_H.value - sec_h) * 0.5,
            Some(p.text_dim),
        );
        if let Some(refresh_rect) = layout.refresh {
            draw_refresh_link(
                canvas,
                root,
                &mut self.engine,
                refresh_rect,
                t(lang, T::ProfileChooserRefreshShort),
                self.hover == Some(Hit::RefreshProfiles),
                focused == Some(Hit::RefreshProfiles),
            );
        }

        // 5. Grouped rounded profile card
        let list_box = layout.list_box;
        let list_shape = scene::rrect(
            list_box.x,
            list_box.y,
            list_box.w,
            list_box.h,
            LIST_RADIUS,
        );
        scene::fill(canvas, root, p.panel_bg, &list_shape);
        canvas.push_clip_layer(Fill::NonZero, root, &list_shape);
        for (index, row) in layout.rows.iter().enumerate() {
            if row.rect.y + row.rect.h < list_box.y || row.rect.y > list_box.y + list_box.h {
                continue;
            }
            let profile = &self.account_profiles[index];
            let here = self.account_profile_sources[index] == ProfileSource::Local;
            let checked = !here && self.selection.checked.contains(&profile.profile_id);
            let hovered = self.hover == Some(Hit::Existing(index));
            let is_focused = focused == Some(Hit::Existing(index));

            let _row_scope = inspect::scope(
                "ProfileRow",
                "container",
                root,
                scene::rect(row.rect.x, row.rect.y, row.rect.w, row.rect.h),
                |record| {
                    record.property("checked", checked, "view state");
                    record.property("source", &row.source, "account provider");
                },
            );
            if hovered {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rect(row.rect.x, row.rect.y, row.rect.w, row.rect.h),
                );
            }
            if is_focused {
                scene::stroke(
                    canvas,
                    root,
                    p.focus_ring,
                    f64::from(WELCOME_RING_W.value),
                    &scene::rect(
                        row.rect.x + 1.0,
                        row.rect.y + 1.0,
                        row.rect.w - 2.0,
                        row.rect.h - 2.0,
                    ),
                );
            }
            if index > 0 {
                let divider_y = f64::from(row.rect.y);
                scene::stroke(
                    canvas,
                    root,
                    p.panel_border,
                    1.0,
                    &Line::new(
                        (f64::from(list_box.x), divider_y),
                        (f64::from(list_box.x + list_box.w), divider_y),
                    ),
                );
            }

            let mut x = row.rect.x + ROW_PAD_X;
            if !here {
                let cy = row.rect.y + (row.rect.h - CHECKBOX_SIZE) * 0.5;
                let check_shape =
                    scene::rrect(x, cy, CHECKBOX_SIZE, CHECKBOX_SIZE, CHECKBOX_RADIUS);
                if checked {
                    scene::fill(canvas, root, p.text, &check_shape);
                    let mut check = BezPath::new();
                    check.move_to((f64::from(x + 5.2), f64::from(cy + 10.4)));
                    check.line_to((f64::from(x + 8.6), f64::from(cy + 13.8)));
                    check.line_to((f64::from(x + 14.8), f64::from(cy + 6.6)));
                    canvas.stroke(
                        &Stroke::new(2.0)
                            .with_caps(vello::kurbo::Cap::Round)
                            .with_join(vello::kurbo::Join::Round),
                        root,
                        p.bg,
                        None,
                        &check,
                    );
                } else {
                    scene::stroke(canvas, root, p.panel_border, 1.5, &check_shape);
                }
                x += CHECKBOX_SIZE + ROW_INNER_GAP;
            }

            let name = profile
                .name
                .clone()
                .unwrap_or_else(|| t(lang, T::ProfileChooserUnnamed).to_owned());
            let tile_rect = Rect::new(
                x,
                row.rect.y + (row.rect.h - TILE_SIZE) * 0.5,
                TILE_SIZE,
                TILE_SIZE,
            );
            Tile {
                icon: None,
                color: Some(tile_color_for(&profile.profile_id)),
                name: &name,
            }
            .draw_in(canvas, root, &mut self.engine, tile_rect, &p);
            x += TILE_SIZE + ROW_INNER_GAP;

            let right_limit = row
                .action
                .map_or(row.rect.x + row.rect.w - ROW_PAD_X, |(_, r, _)| r.x - 10.0);
            let text_w = (right_limit - x).max(60.0);
            let title_style = theme::TextStyle::new(
                theme::role::row().metrics,
                theme::FontWeight::Medium,
            );
            let title_h = theme::role::row().metrics.line_height;
            let status_h = if row.status.is_empty() {
                0.0
            } else {
                self.engine
                    .simple_layout_wrapped(&row.status, theme::role::meta(), text_w)
                    .height()
                    + 4.0
            };
            let block_h = title_h + 2.0 + row.source_height + status_h;
            let text_top = row.rect.y + (row.rect.h - block_h) * 0.5;
            let name_layout = self.engine.simple_layout(&name, title_style);
            scene::draw_layout(canvas, root, &name_layout, x, text_top, Some(p.text));
            draw_wrapped_text(
                &mut self.engine,
                canvas,
                root,
                &row.source,
                x,
                text_top + title_h + 2.0,
                text_w,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
            if !row.status.is_empty() {
                draw_wrapped_text(
                    &mut self.engine,
                    canvas,
                    root,
                    &row.status,
                    x,
                    text_top + title_h + 6.0 + row.source_height,
                    text_w,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text,
                );
            }
            if let Some((hit, rect, label)) = row.action {
                draw_button_enabled(
                    canvas,
                    root,
                    &mut self.engine,
                    rect,
                    label,
                    ButtonRole::Secondary,
                    self.hover == Some(hit),
                    focused == Some(hit),
                    true,
                );
            }
        }
        canvas.pop_layer();
        scene::stroke(canvas, root, p.panel_border, 1.0, &list_shape);

        if let Some(bar) = ScrollbarGeom::for_content(
            list_box,
            layout.content_height,
            self.existing_profile_scroll,
        ) {
            scrollbar::draw(
                canvas,
                root,
                bar,
                bar.on_thumb(self.pointer.0, self.pointer.1)
                    || self.existing_profile_scroll_drag.is_some(),
            );
        }

        // 6. Full-width primary button: Sync N profiles
        draw_button_enabled(
            canvas,
            root,
            &mut self.engine,
            layout.primary,
            &layout.primary_label,
            ButtonRole::Inverted,
            self.hover == Some(Hit::Primary) && layout.primary_enabled,
            focused == Some(Hit::Primary) && layout.primary_enabled,
            layout.primary_enabled,
        );

        // 7. Optional Cancel link when an enrollment is unfinished
        if let Some(cancel_rect) = layout.cancel {
            let cancel_leaf = LinkLeaf::new(
                &mut self.engine,
                t(lang, T::ProfileChooserCancel),
                false,
                true,
                self.hover == Some(Hit::CancelSelection),
                focused == Some(Hit::CancelSelection),
            );
            cancel_leaf.paint(
                canvas,
                root,
                &mut self.engine,
                cancel_rect,
                &cx,
                Default::default(),
            );
        }

        // 8. Footer links: Create a new profile · Continue offline
        let create_leaf = LinkLeaf::new(
            &mut self.engine,
            t(lang, T::ProfileChooserCreate),
            false,
            true,
            self.hover == Some(Hit::CreateNew),
            focused == Some(Hit::CreateNew),
        );
        create_leaf.paint(
            canvas,
            root,
            &mut self.engine,
            layout.create_new,
            &cx,
            Default::default(),
        );
        let dot_layout = self.engine.simple_layout(" · ", theme::role::meta());
        let dot_h = scene::first_line_height(&dot_layout);
        scene::draw_layout(
            canvas,
            root,
            &dot_layout,
            layout.dot_x,
            layout.footer_y + (WELCOME_LINK_H.value - dot_h) * 0.5,
            Some(p.text_dim),
        );
        let offline_leaf = LinkLeaf::new(
            &mut self.engine,
            t(lang, T::AccountContinueOffline),
            false,
            true,
            self.hover == Some(Hit::ContinueOffline),
            focused == Some(Hit::ContinueOffline),
        );
        offline_leaf.paint(
            canvas,
            root,
            &mut self.engine,
            layout.continue_offline,
            &cx,
            Default::default(),
        );
    }
}

fn draw_refresh_link(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    at: Rect,
    label: &str,
    hovered: bool,
    focused: bool,
) {
    let p = theme::p();
    let style = theme::TextStyle::new(theme::role::meta().metrics, theme::FontWeight::Medium);
    let layout = engine.simple_layout(label, style);
    let text_h = scene::first_line_height(&layout);
    let color = p.accent;

    // Circular refresh arrow on the left.
    let cx = f64::from(at.x + REFRESH_ICON_SIZE * 0.5);
    let cy = f64::from(at.y + at.h * 0.5);
    let r = f64::from(REFRESH_ICON_SIZE * 0.5) - 1.5;
    let arc = KurboArc::new(
        Point::new(cx, cy),
        Vec2::new(r, r),
        -std::f64::consts::FRAC_PI_4,
        std::f64::consts::PI * 1.65,
        0.0,
    );
    canvas.stroke(
        &Stroke::new(1.5).with_caps(vello::kurbo::Cap::Round),
        root,
        color,
        None,
        &arc,
    );
    let tip_x = cx + r * (-std::f64::consts::FRAC_PI_4).cos();
    let tip_y = cy + r * (-std::f64::consts::FRAC_PI_4).sin();
    let mut head = BezPath::new();
    head.move_to((tip_x - 3.2, tip_y + 0.2));
    head.line_to((tip_x + 0.5, tip_y + 0.5));
    head.line_to((tip_x + 0.3, tip_y - 3.2));
    canvas.stroke(
        &Stroke::new(1.5)
            .with_caps(vello::kurbo::Cap::Round)
            .with_join(vello::kurbo::Join::Round),
        root,
        color,
        None,
        &head,
    );

    let text_x = at.x + REFRESH_ICON_SIZE + REFRESH_ICON_GAP;
    let text_y = at.y + (at.h - text_h) * 0.5;
    scene::draw_layout(canvas, root, &layout, text_x, text_y, Some(color));
    if hovered || focused {
        let line_y = f64::from(text_y + text_h + 1.0);
        scene::stroke(
            canvas,
            root,
            color,
            1.0,
            &Line::new(
                (f64::from(text_x), line_y),
                (f64::from(text_x + layout.width()), line_y),
            ),
        );
    }
}
