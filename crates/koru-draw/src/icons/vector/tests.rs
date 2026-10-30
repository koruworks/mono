// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn warmed_paths_keep_the_same_cached_allocation() {
    let addresses: Vec<_> = (0..ICON_DEFS.len())
        .map(|i| std::ptr::from_ref(parsed(i)))
        .collect();
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        for (i, address) in addresses.iter().enumerate() {
            assert_eq!(
                *address,
                std::ptr::from_ref(std::hint::black_box(parsed(i)))
            );
        }
    }
    eprintln!(
        "{} warmed parsed-path retrievals: {:?}",
        ICON_DEFS.len() * 1000,
        start.elapsed()
    );
}
