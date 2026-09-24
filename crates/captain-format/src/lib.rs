//! Layer 2: turns authoring formats into [`FormattedText`] for the protocol layer.
//!
//! This crate never talks to the network and knows no TL types. The protocol layer sends
//! whatever [`FormattedText`] it gets, so a new syntax only has to be added here.

use captain_core::FormattedText;

/// Authoring syntax of a message body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Syntax {
    /// Text sent exactly as written, without styling.
    #[default]
    Plain,
}

/// Parses `input` written in `syntax` into text plus styling entities.
// TODO(tantai): add Markdown and HTML (bold, italic, code, links, spoilers, quotes). It waits
// until the protocol layer is verified against a live account, so format bugs and protocol bugs
// never have to be told apart at the same time.
pub fn parse(input: &str, syntax: Syntax) -> FormattedText {
    match syntax {
        Syntax::Plain => FormattedText::plain(input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_keeps_markup_characters_as_text() {
        let text = parse("*không* đậm", Syntax::Plain);
        assert_eq!(text.text, "*không* đậm");
        assert!(text.entities.is_empty());
    }
}
