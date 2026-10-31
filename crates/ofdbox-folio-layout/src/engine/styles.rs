//! styles layout.

use super::*;

impl<'a> Engine<'a> {
    // ---- Styles ----

    /// Resolve a paragraph style: the named style is the base and direct
    /// formatting overrides non-default values.
    pub(crate) fn resolve_paragraph_style(&self, style: &ParaStyle) -> ParaStyle {
        let mut resolved = self
            .named_paragraph_style(style.style_id.as_deref())
            .unwrap_or_else(|| self.document.styles.default_paragraph.clone());
        apply_paragraph_direct(&mut resolved, style);
        resolved.style_id = style.style_id.clone();
        resolved
    }

    pub(crate) fn named_paragraph_style(&self, id: Option<&str>) -> Option<ParaStyle> {
        let id = id?;
        let mut current = self.document.styles.paragraph.iter().find(|n| n.id == id)?;
        let mut chain = Vec::new();
        for _ in 0..8 {
            chain.push(current);
            let Some(parent) = current.based_on.as_deref() else {
                break;
            };
            let Some(next) = self
                .document
                .styles
                .paragraph
                .iter()
                .find(|named| named.id == parent)
            else {
                break;
            };
            if chain.iter().any(|item| item.id == next.id) {
                break;
            }
            current = next;
        }
        // Start from the default style, then layer non-default values in
        // parent → child order; fields left unset by a child inherit from its
        // parent.
        let mut resolved = self.document.styles.default_paragraph.clone();
        for named in chain.into_iter().rev() {
            apply_paragraph_direct(&mut resolved, &named.props);
        }
        Some(resolved)
    }

    /// Resolve a character style: the named character style is the base and
    /// direct formatting overrides non-default values.
    pub(crate) fn resolve_char_style(
        &self,
        style: &TextStyle,
        style_id: Option<&str>,
    ) -> TextStyle {
        let mut resolved = self
            .named_char_style(style_id)
            .unwrap_or_else(|| self.document.styles.default_character.clone());
        apply_text_direct(&mut resolved, style);
        resolved
    }

    pub(crate) fn named_char_style(&self, id: Option<&str>) -> Option<TextStyle> {
        let id = id?;
        let mut current = self
            .document
            .styles
            .character
            .iter()
            .find(|named| named.id == id)?;
        let mut chain = Vec::new();
        for _ in 0..8 {
            chain.push(current);
            let Some(parent) = current.based_on.as_deref() else {
                break;
            };
            let Some(next) = self
                .document
                .styles
                .character
                .iter()
                .find(|named| named.id == parent)
            else {
                break;
            };
            if chain.iter().any(|item| item.id == next.id) {
                break;
            }
            current = next;
        }
        let mut resolved = self.document.styles.default_character.clone();
        for named in chain.into_iter().rev() {
            apply_text_direct(&mut resolved, &named.props);
        }
        Some(resolved)
    }

    /// Register and deduplicate a text style, returning the style index.
    pub(crate) fn intern_style(&mut self, style: &TextStyle) -> usize {
        let key = StyleKey::of(style);
        if let Some(index) = self.style_index.get(&key) {
            return *index;
        }
        let index = self.styles.len();
        self.styles.push(style.clone());
        self.style_index.insert(key, index);
        index
    }
}
