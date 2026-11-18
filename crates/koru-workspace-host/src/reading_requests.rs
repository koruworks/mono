// SPDX-License-Identifier: MPL-2.0

//! ***Read all pages* and *Read again* in the right-click menus** (OCR Plan
//! 3.2; EXPERIENCE §4, §7; TECHNICAL §9, §10).
//!
//! | entry | where | shown when |
//! | --- | --- | --- |
//! | *Read all pages* | a file's menu under Explore → Files | the file stopped at the page limit, its bytes are on this computer, and it can be finished here |
//! | *Read again* | a file's menu, and a picture's in a page | the item has been read, or reading it failed, and it can be read here |
//!
//! **The Store decides, the menu shows.** `GET /v1/readings/{workspace}/{hash}`
//! answers [`ReadingOffers`] — whether each request would be accepted on this
//! computer now — so no surface reasons from status tokens, and a request is
//! refused exactly where it was not offered.
//!
//! **Nothing is fetched while a menu opens.** A view keeps the last answer per
//! item and opens its menu from that; opening it also asks the shell for a
//! fresh answer, fetched on a worker (as *Save reading to file…* fetches its
//! reading: `app.rs`, `fetch_reading_offers`). When the answer differs from
//! what the open menu shows, the menu takes it — the entries are a group at
//! the menu's end, so no row above them moves under the pointer.
//!
//! This module is the pure half: which entries an answer shows, in which
//! order, and their words in every language.

pub use koru_api_client::{ReadRequestKind, ReadingOffers};

/// **The entries a menu shows for one item**, in their order: *Read all
/// pages* before *Read again*. `None` — nothing known yet, or nothing read of
/// the item — shows neither. A picture is one page: it never offers *Read all
/// pages*, whatever an answer says.
pub fn entries(offers: Option<ReadingOffers>, picture: bool) -> Vec<ReadRequestKind> {
    let Some(offers) = offers else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if offers.read_all && !picture {
        out.push(ReadRequestKind::ReadAll);
    }
    if offers.read_again {
        out.push(ReadRequestKind::ReadAgain);
    }
    if offers.force_ocr {
        out.push(ReadRequestKind::ForceOcr);
    }
    out
}

/// The entry's words, in the person's language (EXPERIENCE §4's wording).
pub fn label(kind: ReadRequestKind, lang: koru_i18n::Lang) -> &'static str {
    koru_i18n::t(
        lang,
        match kind {
            ReadRequestKind::ReadAll => koru_i18n::Key::ReadingReadAllPages,
            ReadRequestKind::ReadAgain => koru_i18n::Key::ReadingReadAgain,
            ReadRequestKind::ForceOcr => koru_i18n::Key::ReadingForceOcr,
        },
    )
}

/// What an answer means once a request for the item has been sent: nothing
/// more is offered until the Store says otherwise. Kept by a view so the menu
/// opened next does not offer the same request twice.
pub fn after_request(_offers: ReadingOffers, _kind: ReadRequestKind) -> ReadingOffers {
    ReadingOffers::default()
}

#[cfg(test)]
mod tests;
