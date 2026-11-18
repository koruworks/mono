// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Enough peers, spread over the id space, that two hash maps holding them
/// are unlikely to agree on an order by chance.
fn peers() -> Vec<(u64, i32)> {
    (1..=64u64)
        .map(|i| (i.wrapping_mul(0x9E37_79B9_7F4A_7C15), i as i32))
        .collect()
}

fn version(entries: impl IntoIterator<Item = (u64, i32)>) -> loro::VersionVector {
    entries.into_iter().collect()
}

#[test]
fn one_version_is_one_revision_whatever_order_its_peers_arrived_in() {
    let forward = version(peers());
    let backward = version(peers().into_iter().rev());
    // A map that grew past these peers and lost the extra ones again holds
    // the same version in a larger table, so in another order.
    let mut grown = version(
        peers()
            .into_iter()
            .chain((1..=512u64).map(|i| (u64::MAX - i, 1))),
    );
    grown.retain(|peer, _| peers().iter().any(|(kept, _)| kept == peer));

    assert_eq!(encode(&forward), encode(&backward));
    assert_eq!(encode(&forward), encode(&grown));
}

#[test]
fn the_revision_lists_its_peers_in_order() {
    let bytes = unhex(&encode(&version(peers().into_iter().rev()))).unwrap();
    // A postcard map and a sequence of pairs are the same bytes.
    let entries: Vec<(u64, i32)> = postcard::from_bytes(&bytes).unwrap();
    let mut sorted = peers();
    sorted.sort();
    assert_eq!(entries, sorted);
}

#[test]
fn the_revision_is_still_a_version_vector_loro_decodes() {
    let held = version(peers());
    let decoded = loro::VersionVector::decode(&unhex(&encode(&held)).unwrap()).unwrap();
    assert_eq!(decoded, held);
    // And Loro's own encoding, what a revision issued before the order was
    // fixed holds, decodes to the same version.
    assert_eq!(loro::VersionVector::decode(&held.encode()).unwrap(), held);
}
