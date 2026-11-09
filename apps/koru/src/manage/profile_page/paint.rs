// SPDX-License-Identifier: MPL-2.0

//! Painting a Profile's page from its layout: the header, the attention
//! card, *Sign-in and sync* (in `sign_in`), the footer, and each control in
//! the look the layout gave it. Nothing here places anything — every rect
//! comes from `ManageView::laid`.

use super::*;

impl ManageView {
    pub(in crate::manage) fn paint_profile_page(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(profile) = self.page_profile() else {
            return;
        };
        let laid = self.laid.clone();
        self.paint_header(canvas, root, &profile, &laid);
        if let Some(words) = &laid.attention {
            let _card = inspect::scope(
                "Manage attention",
                "part",
                root,
                scene::rect(words.rect.x, words.rect.y, words.rect.w, words.rect.h),
                |r| r.property("about", "this Profile's deletion (P19/P20)", "controller"),
            );
            let _box = Card::new(None).draw(canvas, root, &mut self.engine, words.rect, &[], 1.0);
            self.paint_words(canvas, root, words, &laid);
        }
        self.paint_workspaces_section(canvas, root);
        self.paint_recovery_sections(canvas, root);
        if let Some(section) = &laid.sign_in {
            self.paint_sign_in(canvas, root, section, &laid);
        }
        if let Some(footer) = laid.footer {
            let _footer = inspect::scope(
                "Manage delete profile",
                "part",
                root,
                scene::rect(footer.x, footer.y, footer.w, footer.h),
                |r| {
                    r.property(
                        "offers",
                        "the shared deletion, the device-local removal, Delete from Koru where allowed",
                        "dialogs (QUESTIONS Q4)",
                    )
                },
            );
            for control in [
                Control::Delete,
                Control::RemoveFromDevice,
                Control::DeleteFromKoru,
            ] {
                if let Some(placed) = laid.placed(control) {
                    self.paint_control(canvas, root, placed);
                }
            }
        }
    }

    fn paint_header(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        profile: &ProfileSummary,
        laid: &Laid,
    ) {
        let header = &laid.header;
        let p = theme::p();
        let _header = inspect::scope(
            "Manage profile header",
            "part",
            root,
            scene::rect(header.rect.x, header.rect.y, header.rect.w, header.rect.h),
            |r| {
                r.property("tile", page::TITLE_TILE, "page::TITLE_TILE");
                r.property(
                    "current",
                    self.c.current() == Some(profile.id),
                    "controller",
                );
                r.property("tile press", "the icon and color panel", "Request");
                r.property("name press", "rename in place", "Request");
            },
        );
        let lit = |control| self.page_hover == Some(control) || self.page_focus == Some(control);
        let (tile_lit, name_lit) = (lit(Control::Tile), lit(Control::Name));
        let (tile_focused, name_focused) = (
            self.page_focus == Some(Control::Tile),
            self.page_focus == Some(Control::Name),
        );
        Tile {
            icon: profile.icon.as_deref(),
            color: profile.color.as_deref(),
            name: &profile.name,
        }
        .draw(canvas, root, &mut self.engine, header.tile);
        if tile_lit {
            let t = header.tile;
            scene::stroke(
                canvas,
                root,
                if tile_focused {
                    p.focus_ring
                } else {
                    p.control_border
                },
                theme::BORDER_W * 2.0,
                &scene::rrect(t.x - 3.0, t.y - 3.0, t.w + 6.0, t.h + 6.0, 9.0),
            );
        }
        if name_lit {
            let n = header.name;
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rrect(n.x - 6.0, n.y, n.w + 12.0, n.h, 6.0),
            );
            if name_focused {
                scene::stroke(
                    canvas,
                    root,
                    p.focus_ring,
                    theme::BORDER_W * 2.0,
                    &scene::rrect(n.x - 6.0, n.y, n.w + 12.0, n.h, 6.0),
                );
            }
        }
        let head = page::Head {
            title: Rect::new(header.text_x, header.name.y, header.text_w, header.name.h),
            title_text: header.name_text.clone(),
            lead: header.status,
            lead_text: header.status_text.clone(),
        };
        self.paint_head(canvas, root, &head);
        for control in [Control::Switch, Control::CloudPointer] {
            if let Some(placed) = laid.placed(control) {
                self.paint_control(canvas, root, placed);
            }
        }
        if let Some((rect, text)) = &header.notice {
            KoruTextParagraph::new(text, theme::role::meta(), "theme::role::meta()", p.text_dim)
                .draw(canvas, root, &mut self.engine, *rect);
        }
    }

    /// An attention block's words and every control drawn inside it.
    pub(in crate::manage) fn paint_words(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        words: &Words,
        laid: &Laid,
    ) {
        let p = theme::p();
        KoruTextParagraph::new(
            &words.title,
            theme::role::section_heading(),
            "theme::role::section_heading()",
            p.text,
        )
        .draw(canvas, root, &mut self.engine, words.title_at);
        KoruTextParagraph::new(
            &words.body,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        )
        .draw(canvas, root, &mut self.engine, words.body_at);
        self.paint_controls_in(canvas, root, words.rect, laid);
    }

    /// Every control whose box lies inside `area`.
    pub(in crate::manage) fn paint_controls_in(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        area: Rect,
        laid: &Laid,
    ) {
        for placed in &laid.controls {
            let r = placed.rect;
            if placed.look != Look::Header
                && r.x >= area.x
                && r.y >= area.y
                && r.x + r.w <= area.x + area.w + 0.5
                && r.y + r.h <= area.y + area.h + 0.5
            {
                self.paint_control(canvas, root, placed);
            }
        }
    }

    pub(in crate::manage) fn paint_control(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        placed: &Placed,
    ) {
        let hovered = self.page_hover == Some(placed.control);
        let focused = self.page_focus == Some(placed.control);
        let enabled = !self.laid.disabled.contains(&placed.control);
        let r = placed.rect;
        match placed.look {
            Look::Header | Look::Row => {}
            Look::Button(role) => button::draw_button_enabled(
                canvas,
                root,
                &mut self.engine,
                r,
                &placed.label,
                role,
                hovered && enabled,
                focused,
                enabled,
            ),
            Look::Action(style) if !enabled => ActionButton::new(&placed.label, style, None)
                .draw_disabled(canvas, root, &mut self.engine, r),
            Look::More | Look::Close => {
                koru_ui::icon_button::draw(
                    canvas,
                    root,
                    &mut self.engine,
                    r,
                    if placed.look == Look::Close {
                        koru_draw::icons::WINDOW_CLOSE
                    } else {
                        koru_draw::icons::DOTS_THREE
                    },
                    koru_ui::icon_button::State {
                        hovered,
                        active: false,
                        disabled: !enabled,
                    },
                );
                if focused {
                    scene::stroke(
                        canvas,
                        root,
                        theme::p().focus_ring,
                        theme::BORDER_W * 2.0,
                        &scene::rrect(r.x, r.y, r.w, r.h, 6.0),
                    );
                }
            }
            Look::Google => button::draw_google_button(
                canvas,
                root,
                &mut self.engine,
                r,
                &placed.label,
                hovered,
                focused,
            ),
            Look::Action(style) => ActionButton::new(&placed.label, style, None).draw_focused(
                canvas,
                root,
                &mut self.engine,
                r,
                hovered,
                focused,
            ),
            Look::Toggle(toggle) => toggle.draw(
                canvas,
                root,
                &mut self.engine,
                r,
                crate::format::lang().is_rtl(),
                hovered,
                focused,
            ),
            Look::Disclosure(open) => {
                let p = theme::p();
                let style = theme::role::meta();
                if focused {
                    scene::stroke(
                        canvas,
                        root,
                        p.focus_ring,
                        theme::BORDER_W * 2.0,
                        &scene::rrect(r.x - 2.0, r.y - 1.0, r.w + 4.0, r.h + 2.0, 4.0),
                    );
                }
                let color = if hovered { p.text } else { p.accent };
                let line = style.metrics.line_height;
                crate::manage::cloud::progress::disclosure(
                    canvas,
                    root,
                    r.x + 2.0,
                    r.y + line * 0.5 - 4.0,
                    open,
                    color,
                );
                koru_ui::text_paragraph::draw_line(
                    canvas,
                    root,
                    &mut self.engine,
                    &placed.label,
                    r.x + 2.0 + crate::manage::cloud::progress::DISCLOSURE_W,
                    r.y,
                    style,
                    "theme::role::meta() — a disclosure",
                    color,
                );
            }
            Look::Link => {
                let p = theme::p();
                let style = theme::role::meta();
                if focused {
                    scene::stroke(
                        canvas,
                        root,
                        p.focus_ring,
                        theme::BORDER_W * 2.0,
                        &scene::rrect(r.x - 4.0, r.y - 1.0, r.w + 8.0, r.h + 2.0, 4.0),
                    );
                }
                koru_ui::text_paragraph::draw_line(
                    canvas,
                    root,
                    &mut self.engine,
                    &placed.label,
                    r.x,
                    r.y,
                    style,
                    "theme::role::meta() — a link",
                    if hovered { p.text } else { p.accent },
                );
            }
        }
    }
}
