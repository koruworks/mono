// SPDX-License-Identifier: MPL-2.0

//! **The welcome modal's own tokens** (Screen 1): every size the choice card
//! and its leaves use that no shared token names — tagged metrics and spacing
//! aliases so the inspector names each measurement.

use koru_draw::theme::{Metric, Space, Tagged};

/// Maximum card width (matches the 560 px onboarding dialog).
pub(crate) const WELCOME_CARD_MAX_W: Metric = Metric::new("WELCOME_CARD_MAX_W", 560.0);

/// Steady minimum height shared across the onboarding modal cards (Screen 1,
/// Checkout pending/incomplete, and Screen 2) so switching options or steps
/// does not jump or flicker vertically.
pub(crate) const WELCOME_MODAL_MIN_H: Metric = Metric::new("WELCOME_MODAL_MIN_H", 556.0);

/// Horizontal gutter kept between the card and the viewport edges.
pub(crate) const WELCOME_CARD_MARGIN: Tagged<Space> = Tagged::new("WELCOME_CARD_MARGIN", Space::S4);

/// Inner padding around the card's sections.
pub(crate) const WELCOME_CARD_PAD: Tagged<Space> = Tagged::new("WELCOME_CARD_PAD", Space::S4);

/// The floating panel's corner radius.
pub(crate) const WELCOME_CARD_RADIUS: Metric = Metric::new("WELCOME_CARD_RADIUS", 14.0);

/// Vertical rhythm between the header, choices, and footer.
pub(crate) const WELCOME_SECTION_GAP: Tagged<Space> = Tagged::new("WELCOME_SECTION_GAP", Space::S3);

/// Vertical rhythm inside the header and footer blocks.
pub(crate) const WELCOME_BLOCK_GAP: Tagged<Space> = Tagged::new("WELCOME_BLOCK_GAP", Space::S2);

/// Horizontal gap between the Koru logo and heading.
pub(crate) const WELCOME_LOGO_GAP: Tagged<Space> = Tagged::new("WELCOME_LOGO_GAP", Space::S2_5);

/// Size of the Koru logo mark beside "Welcome to Koru".
pub(crate) const WELCOME_LOGO_SIZE: Metric = Metric::new("WELCOME_LOGO_SIZE", 28.0);

/// Rounded border radius for the Koru Cloud card and checkout summary.
pub(crate) const WELCOME_OPTION_RADIUS: Metric = Metric::new("WELCOME_OPTION_RADIUS", 12.0);

/// Padding inside an option card header.
pub(crate) const WELCOME_OPTION_PAD_X: Tagged<Space> =
    Tagged::new("WELCOME_OPTION_PAD_X", Space::S4);
pub(crate) const WELCOME_OPTION_PAD_Y: Tagged<Space> =
    Tagged::new("WELCOME_OPTION_PAD_Y", Space::S2);
/// Vertical padding inside the Koru Cloud card.
pub(crate) const WELCOME_CLOUD_PAD_Y: Tagged<Space> = Tagged::new("WELCOME_CLOUD_PAD_Y", Space::S3);

/// Gap between title and subtitle inside an option card.
pub(crate) const WELCOME_OPTION_TEXT_GAP: Tagged<Space> =
    Tagged::new("WELCOME_OPTION_TEXT_GAP", Space::S1);

/// Horizontal gap inside option and plan rows.
pub(crate) const WELCOME_ROW_GAP: Tagged<Space> = Tagged::new("WELCOME_ROW_GAP", Space::S2_5);

/// Height and spacing of the Google / Apple provider buttons and Continue CTA.
pub(crate) const WELCOME_PROVIDER_H: Metric = Metric::new("WELCOME_PROVIDER_H", 38.0);
pub(crate) const WELCOME_PROVIDER_GAP: Tagged<Space> =
    Tagged::new("WELCOME_PROVIDER_GAP", Space::S2);
pub(crate) const WELCOME_PROVIDER_RADIUS: Metric = Metric::new("WELCOME_PROVIDER_RADIUS", 8.0);
pub(crate) const WELCOME_PROVIDER_MARK: Metric = Metric::new("WELCOME_PROVIDER_MARK", 18.0);
pub(crate) const WELCOME_PROVIDER_MARK_GAP: Metric = Metric::new("WELCOME_PROVIDER_MARK_GAP", 10.0);

/// Height of the inline text links ("Compare plans", "or continue with email", "Change").
pub(crate) const WELCOME_LINK_H: Metric = Metric::new("WELCOME_LINK_H", 22.0);
pub(crate) const WELCOME_LINK_PAD_X: Metric = Metric::new("WELCOME_LINK_PAD_X", 2.0);
pub(crate) const WELCOME_LINK_ARROW_SIZE: Metric = Metric::new("WELCOME_LINK_ARROW_SIZE", 11.0);
pub(crate) const WELCOME_LINK_ARROW_GAP: Metric = Metric::new("WELCOME_LINK_ARROW_GAP", 4.0);

/// Focus ring metrics shared across custom welcome leaves.
pub(crate) const WELCOME_RING_W: Metric = Metric::new("WELCOME_RING_W", 2.0);
pub(crate) const WELCOME_RING_GAP: Metric = Metric::new("WELCOME_RING_GAP", 2.0);

/// Underline-tab Monthly / Yearly billing selector metrics.
pub(crate) const WELCOME_SEGMENT_H: Metric = Metric::new("WELCOME_SEGMENT_H", 26.0);
pub(crate) const WELCOME_SEGMENT_GAP: Tagged<Space> = Tagged::new("WELCOME_SEGMENT_GAP", Space::S4);
pub(crate) const WELCOME_SEGMENT_BADGE_GAP: Metric = Metric::new("WELCOME_SEGMENT_BADGE_GAP", 6.0);
pub(crate) const WELCOME_SEGMENT_UNDERLINE_H: Metric =
    Metric::new("WELCOME_SEGMENT_UNDERLINE_H", 2.0);

/// Checkout status modal top icon (spinner / card badge) size.
pub(crate) const CHECKOUT_ICON_SIZE: Metric = Metric::new("CHECKOUT_ICON_SIZE", 36.0);

/// Browser sign-in modal header badge pair (`[Koru] • • • [Provider]`).
pub(crate) const BROWSER_BADGE_SIZE: Metric = Metric::new("BROWSER_BADGE_SIZE", 56.0);
pub(crate) const BROWSER_BADGE_RADIUS: Metric = Metric::new("BROWSER_BADGE_RADIUS", 14.0);
pub(crate) const BROWSER_BADGE_MARK: Metric = Metric::new("BROWSER_BADGE_MARK", 26.0);
pub(crate) const BROWSER_BADGE_DOT_GAP: Metric = Metric::new("BROWSER_BADGE_DOT_GAP", 14.0);
pub(crate) const BROWSER_BADGE_DOT_STEP: Metric = Metric::new("BROWSER_BADGE_DOT_STEP", 11.0);
pub(crate) const BROWSER_BADGE_DOT_RADIUS: Metric = Metric::new("BROWSER_BADGE_DOT_RADIUS", 3.0);

/// Centered subtitle width cap in the browser sign-in modal so the copy wraps
/// into two balanced lines.
pub(crate) const BROWSER_BODY_MAX_W: Metric = Metric::new("BROWSER_BODY_MAX_W", 410.0);

/// Status pill (`Waiting for Google...`) dimensions.
pub(crate) const BROWSER_STATUS_H: Metric = Metric::new("BROWSER_STATUS_H", 36.0);
pub(crate) const BROWSER_STATUS_RADIUS: Metric = Metric::new("BROWSER_STATUS_RADIUS", 18.0);
pub(crate) const BROWSER_STATUS_PAD_X: Metric = Metric::new("BROWSER_STATUS_PAD_X", 16.0);
pub(crate) const BROWSER_STATUS_SPINNER: Metric = Metric::new("BROWSER_STATUS_SPINNER", 16.0);
pub(crate) const BROWSER_STATUS_GAP: Metric = Metric::new("BROWSER_STATUS_GAP", 9.0);

/// Leading external-link glyph size inside *Open the browser again*.
pub(crate) const BROWSER_BUTTON_ICON: Metric = Metric::new("BROWSER_BUTTON_ICON", 16.0);
pub(crate) const BROWSER_BUTTON_ICON_GAP: Metric = Metric::new("BROWSER_BUTTON_ICON_GAP", 8.0);
pub(crate) const BROWSER_BUTTON_H: Metric = Metric::new("BROWSER_BUTTON_H", 38.0);
pub(crate) const BROWSER_FOOTER_GAP: Tagged<Space> = Tagged::new("BROWSER_FOOTER_GAP", Space::S1);

/// Thin horizontal divider above the *Already have an account? Sign in* row,
/// and the extra vertical margin added above and below it.
pub(crate) const WELCOME_DIVIDER_H: Metric = Metric::new("WELCOME_DIVIDER_H", 1.0);
pub(crate) const WELCOME_DIVIDER_STROKE: Metric = Metric::new("WELCOME_DIVIDER_STROKE", 0.5);
pub(crate) const WELCOME_DIVIDER_MARGIN: Tagged<Space> =
    Tagged::new("WELCOME_DIVIDER_MARGIN", Space::S2);
