// SPDX-License-Identifier: MPL-2.0

//! Render selected agent pills for a provider without losing their identity
//! in the stored chat message.

use koru_link::KoruLink;

/// Replace selected skill/command links at their exact positions in a stored
/// message. Literal link-shaped words were escaped by the composer and remain
/// words. Document links keep their existing spelling and explicit context.
pub fn render_prompt_text(markdown: &str) -> String {
    let mut rendered = String::with_capacity(markdown.len());
    let mut from = 0;
    for token in koru_link::wrapped(markdown) {
        rendered.push_str(&koru_link::unescape_literal_links(
            &markdown[from..token.range.start],
        ));
        match token.link {
            KoruLink::Skill { name } => {
                rendered.push('$');
                rendered.push_str(&name);
            }
            KoruLink::Command { name } => {
                rendered.push('/');
                rendered.push_str(&name);
            }
            _ => rendered.push_str(&markdown[token.range.clone()]),
        }
        from = token.range.end;
    }
    rendered.push_str(&koru_link::unescape_literal_links(&markdown[from..]));
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_commands_keep_position_and_literal_markers_stay_words() {
        let stored = r"literal $review-pr then [[koru://skill/review-pr]] and \[[koru://skill/review-pr]] then [[koru://command/issue%2342]]";
        assert_eq!(
            render_prompt_text(stored),
            "literal $review-pr then $review-pr and [[koru://skill/review-pr]] then /issue#42"
        );
    }
}
