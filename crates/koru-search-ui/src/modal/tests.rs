// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn the_modal_owns_every_draw_record_and_releases_its_scope() {
    for scale in [1.0, 1.5, 2.0] {
        let root = Affine::scale(scale);
        let viewport = (900.0, 650.0);
        let modal = SearchModal::new(viewport);
        let mut search = SearchSurface::new();
        search.set_viewport(modal.bounds().w, modal.bounds().h);
        let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, viewport.0, viewport.1));
        modal.draw(&mut search, &mut Scene::new(), root, true, (0.0, 0.0));
        inspect::leaf("after modal", root, scene::rect(0.0, 0.0, 1.0, 1.0), |_| {});
        let snapshot = capture.finish();
        assert_eq!(snapshot.records[0].name, "SearchModal");
        assert_eq!(snapshot.records[0].kind, "component");
        assert!(
            snapshot
                .records
                .iter()
                .any(|record| record.name == "KoruTextInput")
        );
        for record in &snapshot.records[1..snapshot.records.len() - 1] {
            let mut parent = record.parent.expect("search drawing has a parent");
            while parent != 0 {
                parent = snapshot.records[parent]
                    .parent
                    .expect("owned by SearchModal");
            }
        }
        assert_eq!(snapshot.records.last().unwrap().parent, None);
    }
}
