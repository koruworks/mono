// SPDX-License-Identifier: MPL-2.0

//! The worker's half without a thread, and the re-mapping without a window:
//! what an attachment becomes, and where it lands after the person kept
//! working.

use std::sync::{Arc, Mutex};

use koru_core::WorkspaceId;
use koru_core::node::Node;
use koru_core::schema::Parent;

use super::*;

/// The store, in memory: tests may not touch the disk or reach a store.
#[derive(Clone, Default)]
struct Memory(Arc<Mutex<Vec<Vec<u8>>>>);

impl BlobPut for Memory {
    fn put(&self, bytes: &[u8]) -> Result<BlobHash, String> {
        let mut held = self.0.lock().unwrap();
        held.push(bytes.to_vec());
        Ok(BlobHash(format!("mem:{}", held.len())))
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn job(sources: Vec<ImageSource>, store: &Memory, mip_side: Option<ImageMipSide>) -> ImageJob {
    ImageJob {
        ticket: ImageTicket(7),
        sources,
        blobs: Box::new(store.clone()),
        mip_side,
    }
}

fn bytes(bytes: Vec<u8>) -> ImageSource {
    ImageSource::Bytes {
        bytes,
        width: None,
        height: None,
        name: None,
    }
}

#[test]
fn the_size_goes_out_before_the_decode_and_the_image_after_it() {
    let store = Memory::default();
    let mut events = Vec::new();
    // A view that would draw this 64px-wide picture 16px wide gets its mip
    // built here, off the loop.
    let side: ImageMipSide = Arc::new(|_, _| 16);
    run(job(vec![bytes(png(64, 32))], &store, Some(side)), |e| {
        events.push(e)
    });
    assert_eq!(events.len(), 2);
    let ImportEvent::Sized { ticket, sizes } = &events[0] else {
        panic!("the header is read first");
    };
    assert_eq!(
        (*ticket, sizes.as_slice()),
        (ImageTicket(7), &[(64, 32)][..])
    );
    let ImportEvent::Done {
        images, failures, ..
    } = &events[1]
    else {
        panic!("then the finished image");
    };
    assert!(failures.is_empty());
    assert_eq!(images.len(), 1);
    let image = &images[0].image;
    assert_eq!(image.hash, "mem:1", "the address is the store's");
    assert_eq!(image.mime, "image/png");
    assert_eq!((image.width, image.height), (Some(64), Some(32)));
    let (size, mip) = images[0].mip.as_ref().expect("a display-sized mip");
    assert_eq!(*size, 16);
    assert_eq!((mip.image.width, mip.image.height), (16, 8));
    assert_eq!(store.0.lock().unwrap().len(), 1);
}

#[test]
fn bytes_that_are_not_a_picture_never_reach_the_store() {
    let store = Memory::default();
    let mut events = Vec::new();
    run(
        job(
            vec![bytes(b"not a picture".to_vec()), bytes(png(4, 4))],
            &store,
            None,
        ),
        |e| events.push(e),
    );
    let Some(ImportEvent::Done {
        images, failures, ..
    }) = events.pop()
    else {
        panic!("a finished event");
    };
    assert_eq!(images.len(), 1, "the good one still lands");
    assert_eq!(failures.len(), 1);
    assert_eq!(
        store.0.lock().unwrap().len(),
        1,
        "only the picture was stored"
    );
}

#[test]
fn a_declared_display_size_wins_over_the_pixels() {
    let store = Memory::default();
    let mut events = Vec::new();
    run(
        job(
            vec![ImageSource::Bytes {
                bytes: png(40, 40),
                width: Some(20),
                height: Some(10),
                name: Some("chart".into()),
            }],
            &store,
            None,
        ),
        |e| events.push(e),
    );
    let ImportEvent::Sized { sizes, .. } = &events[0] else {
        panic!("sized first");
    };
    assert_eq!(sizes, &[(20, 10)], "the placeholder takes the row's shape");
    let ImportEvent::Done { images, .. } = &events[1] else {
        panic!("then done");
    };
    assert_eq!(
        (images[0].image.width, images[0].image.height),
        (Some(20), Some(10))
    );
    assert_eq!(images[0].image.name.as_deref(), Some("chart"));
}

#[test]
fn the_report_says_what_was_not_changed() {
    let failures = [Failure("not a recognised image (3 bytes)".into())];
    let none = failure_report(koru_i18n::Lang::En, false, &failures);
    assert_eq!(none.title, "Couldn’t add that image");
    assert!(none.body.contains("not changed"), "{}", none.body);
    assert_eq!(
        none.details.as_deref(),
        Some("not a recognised image (3 bytes)")
    );
    let some = failure_report(koru_i18n::Lang::En, true, &failures);
    assert!(
        some.body.contains("other images were added"),
        "{}",
        some.body
    );
}

// ------------------------------------------------------------- re-mapping

fn doc(lines: &[&str]) -> (WorkspaceDoc, Vec<NodeId>) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let ids = lines
        .iter()
        .map(|line| {
            ws.insert(Parent::Root, Node::paragraph(*line).unwrap())
                .unwrap()
        })
        .collect();
    (ws, ids)
}

fn paste_below(anchor: NodeId) -> ImagePlace {
    ImagePlace::Paste(Some(PasteTarget {
        anchor,
        replacing: Vec::new(),
    }))
}

#[test]
fn typing_around_the_anchor_does_not_move_it() {
    let (mut ws, ids) = doc(&["one", "two"]);
    let recorded = RecordedPlace::record(&ws, paste_below(ids[1]));
    // Lines typed before the anchor, and on it, while the image loads.
    ws.insert_after(ids[0], Node::paragraph("typed meanwhile").unwrap())
        .unwrap();
    ws.set_text(ids[1], "two, edited").unwrap();
    assert_eq!(map_place(&ws, &recorded), paste_below(ids[1]));
}

#[test]
fn a_deleted_anchor_hands_its_place_to_the_line_before_it() {
    let (mut ws, ids) = doc(&["one", "two", "three"]);
    let recorded = RecordedPlace::record(&ws, paste_below(ids[1]));
    ws.delete(ids[1]).unwrap();
    assert_eq!(map_place(&ws, &recorded), paste_below(ids[0]));
    ws.delete(ids[0]).unwrap();
    assert_eq!(
        map_place(&ws, &recorded),
        ImagePlace::Paste(None),
        "with nothing left to stand on, it goes at the end — never nowhere"
    );
}

#[test]
fn a_replacement_forgets_the_rows_that_have_already_gone() {
    let (mut ws, ids) = doc(&["one", "two", "three"]);
    let place = ImagePlace::Paste(Some(PasteTarget {
        anchor: ids[0],
        replacing: vec![ids[0], ids[1]],
    }));
    let recorded = RecordedPlace::record(&ws, place);
    ws.delete(ids[1]).unwrap();
    assert_eq!(
        map_place(&ws, &recorded),
        ImagePlace::Paste(Some(PasteTarget {
            anchor: ids[0],
            replacing: vec![ids[0]],
        }))
    );
}

#[test]
fn a_replaced_row_somebody_wrote_in_is_kept() {
    let (mut ws, ids) = doc(&["one", "two", "three"]);
    let place = ImagePlace::Paste(Some(PasteTarget {
        anchor: ids[1],
        replacing: vec![ids[1], ids[2]],
    }));
    let recorded = RecordedPlace::record(&ws, place);
    ws.set_text(ids[2], "three, rewritten").unwrap();
    assert_eq!(
        map_place(&ws, &recorded),
        ImagePlace::Paste(Some(PasteTarget {
            anchor: ids[1],
            replacing: vec![ids[1]],
        }))
    );
    ws.set_text(ids[1], "two, rewritten").unwrap();
    assert_eq!(
        map_place(&ws, &recorded),
        paste_below(ids[0]),
        "nothing replaced: the images stand where the placeholder did"
    );
}

#[test]
fn image_converts_its_line_only_while_nobody_has_written_on_it() {
    let (mut ws, ids) = doc(&["/image"]);
    let recorded = RecordedPlace::record(&ws, ImagePlace::Convert(ids[0]));
    assert_eq!(map_place(&ws, &recorded), ImagePlace::Convert(ids[0]));
    ws.set_text(ids[0], "a caption somebody typed").unwrap();
    assert_eq!(
        map_place(&ws, &recorded),
        paste_below(ids[0]),
        "what they typed is kept and the picture goes below it"
    );
}
