//! Mapping of OFD outlines (`Outlines`) to Fixed IR.
//!
//! GB/T 33190 §7.8: `OutlineElem` can nest arbitrarily, the jump target lives in
//! `Actions/Action/Goto/Dest`, `PageID` references the page identity, and coordinates are in
//! millimeters with the origin at the top-left corner, consistent with Fixed IR.

use std::collections::HashMap;

use ofdbox_core::model::definition::{ActionKind, GotoKind};
use ofdbox_core::model::document::{Document, OutlineElem, PageEntry};
use ofdbox_folio_ir::{Destination, OutlineItem, Point};

/// Maximum nesting depth of the outline tree, so that malformed files cannot drag parsing into deep recursion.
const MAX_DEPTH: usize = 16;

/// Maps a document outline to the Fixed IR outline tree.
pub(crate) fn build_outline(document: &Document, pages: &[PageEntry]) -> Vec<OutlineItem> {
    let Some(outlines) = document.outlines.as_ref() else {
        return Vec::new();
    };
    let index: HashMap<String, usize> = pages
        .iter()
        .enumerate()
        .map(|(position, page)| (page.id.clone(), position))
        .collect();
    outlines
        .outline_elems
        .iter()
        .map(|elem| convert(elem, &index, 0))
        .collect()
}

/// Converts a single outline node.
fn convert(elem: &OutlineElem, index: &HashMap<String, usize>, depth: usize) -> OutlineItem {
    let title = elem.title.trim();
    let title = if title.is_empty() {
        "Untitled".to_owned()
    } else {
        title.to_owned()
    };
    let children = if depth + 1 < MAX_DEPTH {
        elem.children
            .iter()
            .map(|child| convert(child, index, depth + 1))
            .collect()
    } else {
        Vec::new()
    };
    // A grouping node may have no jump target of its own; fall back to the first child that can jump.
    let destination = destination(elem, index)
        .or_else(|| children.first().map(|child| child.destination))
        .unwrap_or(Destination {
            page: 0,
            point: Point::new(0.0, 0.0),
        });
    OutlineItem {
        title,
        destination,
        children,
    }
}

/// Reads the jump target of the node itself.
fn destination(elem: &OutlineElem, index: &HashMap<String, usize>) -> Option<Destination> {
    let actions = elem.actions.as_ref()?;
    for action in &actions.actions {
        let ActionKind::Goto(goto) = &action.kind else {
            continue;
        };
        let GotoKind::Dest(dest) = &goto.kind else {
            continue;
        };
        let Some(page) = index.get(&dest.page_id.to_string()).copied() else {
            continue;
        };
        return Some(Destination {
            page,
            point: Point::new(dest.left.unwrap_or(0.0), dest.top.unwrap_or(0.0)),
        });
    }
    None
}
