// SPDX-License-Identifier: MPL-2.0

use super::*;

fn offers(read_all: bool, read_again: bool) -> Option<ReadingOffers> {
    Some(ReadingOffers {
        read_all,
        read_again,
    })
}

/// **When each entry is shown** (EXPERIENCE §4): *Read all pages* only where
/// the Store offers it, on a file; *Read again* wherever it offers that, on a
/// file or a picture; neither before anything is known.
#[test]
fn each_entry_is_shown_only_where_the_store_offers_it() {
    use ReadRequestKind::{ReadAgain, ReadAll};
    assert!(
        entries(None, false).is_empty(),
        "nothing known: nothing shown"
    );
    assert!(entries(offers(false, false), false).is_empty());
    assert_eq!(entries(offers(true, false), false), vec![ReadAll]);
    assert_eq!(entries(offers(false, true), false), vec![ReadAgain]);
    assert_eq!(
        entries(offers(true, true), false),
        vec![ReadAll, ReadAgain],
        "Read all pages first"
    );
    // A picture: Read again only.
    assert_eq!(entries(offers(true, true), true), vec![ReadAgain]);
    assert!(entries(offers(true, false), true).is_empty());
    assert_eq!(entries(offers(false, true), true), vec![ReadAgain]);
}

/// A request sent is not offered again by the menu opened next.
#[test]
fn a_request_sent_is_not_offered_twice() {
    let both = ReadingOffers {
        read_all: true,
        read_again: true,
    };
    assert_eq!(
        after_request(both, ReadRequestKind::ReadAll),
        ReadingOffers {
            read_all: false,
            read_again: true
        }
    );
    assert_eq!(
        after_request(both, ReadRequestKind::ReadAgain),
        ReadingOffers {
            read_all: true,
            read_again: false
        }
    );
}

/// **Both entries exist in all eight languages**, each its own words: none
/// empty, none an English fall-back outside English, and the two never the
/// same sentence.
#[test]
fn both_entries_read_in_all_eight_languages() {
    for lang in koru_i18n::Lang::ALL {
        let all = label(ReadRequestKind::ReadAll, lang);
        let again = label(ReadRequestKind::ReadAgain, lang);
        assert!(
            !all.trim().is_empty() && !again.trim().is_empty(),
            "{lang:?}"
        );
        assert_ne!(all, again, "{lang:?}");
        if lang != koru_i18n::Lang::En {
            assert_ne!(all, "Read all pages", "{lang:?}");
            assert_ne!(again, "Read again", "{lang:?}");
        }
    }
    assert_eq!(
        label(ReadRequestKind::ReadAll, koru_i18n::Lang::En),
        "Read all pages"
    );
    assert_eq!(
        label(ReadRequestKind::ReadAgain, koru_i18n::Lang::En),
        "Read again"
    );
}
