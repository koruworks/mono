// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn every_published_managed_route_is_available_to_each_provider_owner() {
    let registry = builtin_providers(Arc::new(crate::NoopAttachmentPathResolver));
    let mut expected: Vec<_> = koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS
        .iter()
        .map(|r| r.kind)
        .collect();
    let mut actual: Vec<_> = registry.registrations().map(|r| r.kind).collect();
    expected.sort_unstable();
    actual.sort_unstable();
    assert_eq!(actual, expected);
}
