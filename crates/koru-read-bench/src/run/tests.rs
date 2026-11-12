// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_read::{ReadingStatus, Reason};

#[test]
fn a_page_is_counted_by_where_its_words_came_from() {
    assert_eq!(
        Category::of(Kind::Pdf, Source::TextLayer),
        Category::TextLayer
    );
    assert_eq!(
        Category::of(Kind::Pdf, Source::Recognized),
        Category::Recognized
    );
    assert_eq!(Category::of(Kind::Pdf, Source::Mixed), Category::Mixed);
    assert_eq!(
        Category::of(Kind::Picture, Source::Recognized),
        Category::Picture
    );
    assert_eq!(Category::of(Kind::Plain, Source::Plain), Category::Plain);
}

#[test]
fn an_end_is_labelled_by_status_reason_or_local_code() {
    let done = |status, reason| Event::Done {
        job: JobId(1),
        status,
        reason,
        pages_done: 1,
    };
    assert_eq!(end_label(&done(ReadingStatus::Read, None)), "read");
    assert_eq!(
        end_label(&done(ReadingStatus::Partial, Some(Reason::PageCap))),
        "partial/page-cap"
    );
    assert_eq!(
        end_label(&Event::Failed {
            job: JobId(1),
            code: FailureCode::NeedsReader
        }),
        "local: needs-reader"
    );
}

#[test]
fn the_spread_is_the_median_the_99th_percentile_and_the_worst() {
    assert_eq!(spread(&[]), None);
    assert_eq!(spread(&[5.0]), Some((5.0, 5.0, 5.0)));
    let values: Vec<f64> = (1..=101).map(f64::from).rev().collect();
    assert_eq!(spread(&values), Some((51.0, 100.0, 101.0)));
}

#[test]
fn totals_add_up_pages_and_files() {
    let mut totals = Totals::default();
    totals.page(Category::Picture, 100.0);
    totals.page(Category::Picture, 50.0);
    totals.end(
        Kind::Picture,
        &Event::Done {
            job: JobId(1),
            status: ReadingStatus::Read,
            reason: None,
            pages_done: 1,
        },
    );
    assert_eq!(totals.millis[&Category::Picture], [100.0, 50.0]);
    assert_eq!(totals.files[&Kind::Picture], 1);
    assert_eq!(totals.ends["read"], 1);
}
