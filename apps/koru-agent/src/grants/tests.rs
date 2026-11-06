// SPDX-License-Identifier: MPL-2.0

use super::identity::tests::{Fake, KORU, WINDOW, at};
use super::*;

fn token(n: u8) -> String {
    format!("grant-{n}")
}

/// The path the Agent takes, with the operating system written down: verify
/// the peer, then mint for it. [`super::mint`] is the same two steps against
/// [`identity::Os`] and the global ledger.
fn mint_for(
    authority: &mut GrantAuthority,
    probe: &Fake,
    peer_pid: Option<u32>,
    token: String,
) -> Result<koru_agent_wire::FirstPartyGrant, Refusal> {
    let peer = identity::verify(peer_pid, probe)?;
    Ok(authority.mint(peer, 1_000, token))
}

fn koru_peer() -> KoruPeer {
    identity::verify(Some(WINDOW), &Fake::koru_window()).expect("the koru window")
}

/// **The anchor, stated as a test.** A grant is minted for a connection whose
/// process is the `koru` beside the Agent and for nothing else — not for a
/// process running another binary, not for a Koru of another installation, and
/// not for a connection whose pid or image the platform could not report. A
/// mechanism that fell back to trusting the caller when it could not check
/// would be a mechanism that never checked.
#[test]
fn a_grant_is_minted_only_for_the_koru_app_beside_this_agent() {
    let machine = Fake::koru_window()
        .with_process(9999, at("/usr/local/bin", "some-mcp-server"))
        .with_process(8888, at("/Applications/Koru Nightly", KORU));
    let mut authority = GrantAuthority::default();

    let grant = mint_for(&mut authority, &machine, Some(WINDOW), token(1))
        .expect("the Koru window was refused a grant");
    assert!(authority.validate(grant.expose(), 1_001));

    assert_eq!(
        mint_for(&mut authority, &machine, Some(9999), token(2)),
        Err(Refusal::NotThisKoru),
        "a grant was minted for a process that is not Koru"
    );
    assert_eq!(
        mint_for(&mut authority, &machine, Some(8888), token(3)),
        Err(Refusal::NotThisKoru),
        "a grant was minted for another installation's Koru"
    );
    assert_eq!(
        mint_for(&mut authority, &machine, None, token(4)),
        Err(Refusal::Unverified),
        "an unreadable peer pid was treated as trusted"
    );
    assert_eq!(
        mint_for(&mut authority, &machine, Some(5555), token(5)),
        Err(Refusal::Unverified),
        "a pid whose image could not be read was treated as trusted"
    );
    for refused in 2..=5 {
        assert!(
            !authority.validate(&token(refused), 1_001),
            "a refused mint left a live token behind"
        );
    }
}

/// A grant is spent by being validated. The Gateway redeems it once at
/// admission; a token that leaks afterwards buys nothing.
#[test]
fn a_grant_is_good_once_and_then_never_again() {
    let mut authority = GrantAuthority::default();
    let grant = authority.mint(koru_peer(), 0, token(1));

    assert!(authority.validate(grant.expose(), 1));
    assert!(
        !authority.validate(grant.expose(), 2),
        "a replayed grant was accepted a second time"
    );
    assert!(!authority.validate("never-minted", 3));
}

/// Expiry is enforced on the way in, not by a timer: the authority has no
/// clock of its own, so every answer is computed against the moment it is
/// asked.
#[test]
fn a_grant_stops_working_when_its_window_closes() {
    let mut authority = GrantAuthority::default();
    let grant = authority.mint(koru_peer(), 0, token(1));
    assert_eq!(grant.expires_at_millis, GRANT_LIFETIME_MILLIS);

    assert!(!authority.validate(grant.expose(), GRANT_LIFETIME_MILLIS + 1));
    // And an expired one is gone rather than lingering to be swept later.
    assert!(!authority.validate(grant.expose(), 1));
}

/// The token never reaches a log, a trace, or a test failure through `Debug` —
/// the same rule `LinkCode` and the launch bootstrap follow, and for the same
/// reason: whole wire messages get formatted routinely.
#[test]
fn a_grant_does_not_print_its_token() {
    let mut authority = GrantAuthority::default();
    let grant = authority.mint(koru_peer(), 0, "the-secret".into());
    let printed = format!("{grant:?}");
    assert!(!printed.contains("the-secret"), "{printed}");
    assert!(printed.contains("expires_at_millis"), "{printed}");
}
