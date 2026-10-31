//! outline layout.

use super::*;

impl<'a> Engine<'a> {
    // ---- Outline ----

    pub(crate) fn build_outline(&self) -> Vec<OutlineItem> {
        let mut roots: Vec<OutlineItem> = Vec::new();
        // Parent chain: each entry is (level, path within `roots`).
        let mut path: Vec<(u8, usize)> = Vec::new();
        for (level, title, page, y) in &self.headings {
            let level = *level;
            while path.last().is_some_and(|(parent, _)| *parent >= level) {
                path.pop();
            }
            let item = OutlineItem {
                title: title.clone(),
                destination: ofdbox_folio_ir::Destination {
                    page: *page,
                    point: Point::new(0.0, *y),
                },
                children: Vec::new(),
            };
            if let Some((_, index)) = path.last() {
                let parent = element_at(&mut roots, &path, *index);
                if let Some(parent) = parent {
                    parent.children.push(item);
                    let index = parent.children.len() - 1;
                    path.push((level, index));
                    continue;
                }
            }
            roots.push(item);
            path.push((level, roots.len() - 1));
        }
        roots
    }
}
