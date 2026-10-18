//! Interface text stays in Unicode logical order. Bidirectional shaping, wrapping,
//! caret hit testing and selection are handled by the vendored epaint layout engine.
use std::borrow::Cow;

/// Preserve the source string for painting, copying and accessibility alike.
pub fn visual(text: &str) -> Cow<'_, str> {
    Cow::Borrowed(text)
}

pub fn visual_auto(text: &str) -> Cow<'_, str> {
    Cow::Borrowed(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labels_and_document_values_keep_logical_order() {
        for text in ["", "واحد اثنين.pdf", "حفظ باسم…", "سلام Hello 123", "سَلَام\nنص آخر", "علی رضایی"] {
            assert_eq!(visual(text), text);
            assert_eq!(visual_auto(text), text);
            assert!(matches!(visual(text), Cow::Borrowed(_)));
        }
    }
}
