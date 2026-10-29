//! OFD 大纲（`Outlines`）到 Fixed IR 的映射。
//!
//! GB/T 33190 §7.8：`OutlineElem` 可层层嵌套，跳转目标放在
//! `Actions/Action/Goto/Dest` 里，`PageID` 引用页面标识，坐标单位毫米、
//! 原点在左上角，与 Fixed IR 一致。

use std::collections::HashMap;

use ofdbox_core::model::definition::{ActionKind, GotoKind};
use ofdbox_core::model::document::{Document, OutlineElem, PageEntry};
use ofdbox_folio_ir::{Destination, OutlineItem, Point};

/// 大纲树最大嵌套深度，避免异常文件把解析拖入深递归。
const MAX_DEPTH: usize = 16;

/// 把文档大纲映射为 Fixed IR 的大纲树。
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

/// 转换单个大纲节点。
fn convert(elem: &OutlineElem, index: &HashMap<String, usize>, depth: usize) -> OutlineItem {
    let title = elem.title.trim();
    let title = if title.is_empty() {
        "未命名".to_owned()
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
    // 分组节点可能自己没有跳转，退化到第一个能跳的子节点。
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

/// 读取节点自身的跳转目标。
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
