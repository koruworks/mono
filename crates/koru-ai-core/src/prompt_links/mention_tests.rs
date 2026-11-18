// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn recipient_addresses_ignore_code_escaped_literals_and_repeated_mentions() {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let a_link = format!("[[koru://bot/{a}#A]]");
    let b_link = format!("[[koru://bot/{b}#B]]");
    assert_eq!(
        mentioned_bots(&format!("{a_link} {a_link} {b_link}")),
        vec![a, b]
    );
    assert_eq!(
        mentioned_bots(&format!("`{b_link}` {a_link}\n```\n{b_link}\n```")),
        vec![a]
    );
    assert!(mentioned_bots(&format!(r"typed \{a_link} and @Someone")).is_empty());
}
