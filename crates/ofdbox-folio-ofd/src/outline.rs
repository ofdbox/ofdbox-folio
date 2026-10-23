//! OFD 大纲（`Outlines`）到 Fixed IR 的映射。
//!
//! GB/T 33190 §7.8：`OutlineElem` 可层层嵌套，跳转目标放在
//! `Actions/Action/Goto/Dest` 里，`PageID` 引用页面标识，坐标单位毫米、
//! 原点在左上角，与 Fixed IR 一致。

use std::collections::HashMap;

use ofdbox_core::{DocumentView, OutlineElemView, PageView};
use ofdbox_folio_ir::{Destination, OutlineItem, Point};

/// 大纲树最大嵌套深度，避免异常文件把解析拖入深递归。
const MAX_DEPTH: usize = 16;

/// 把文档大纲映射为 Fixed IR 的大纲树。
pub(crate) fn build_outline(document: &DocumentView, pages: &[PageView]) -> Vec<OutlineItem> {
    let Some(outlines) = document.outlines() else {
        return Vec::new();
    };
    let index: HashMap<String, usize> = pages
        .iter()
        .enumerate()
        .filter_map(|(position, page)| page.id().map(|id| (id.to_owned(), position)))
        .collect();
    outlines
        .outline_elems()
        .map(|elem| convert(elem, &index, 0))
        .collect()
}

/// 转换单个大纲节点。
fn convert(elem: OutlineElemView, index: &HashMap<String, usize>, depth: usize) -> OutlineItem {
    let title = elem
        .title()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("未命名")
        .to_owned();
    let children = if depth + 1 < MAX_DEPTH {
        elem.children()
            .map(|child| convert(child, index, depth + 1))
            .collect()
    } else {
        Vec::new()
    };
    // 分组节点可能自己没有跳转，退化到第一个能跳的子节点。
    let destination = destination(&elem, index)
        .or_else(|| children.iter().map(|child| child.destination).next())
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
fn destination(elem: &OutlineElemView, index: &HashMap<String, usize>) -> Option<Destination> {
    for action in elem.actions()?.actions() {
        let Some(dest) = action.goto().and_then(|goto| goto.dest()) else {
            continue;
        };
        let Some(page) = dest.page_id().and_then(|id| index.get(id).copied()) else {
            continue;
        };
        return Some(Destination {
            page,
            point: Point::new(dest.left().unwrap_or(0.0), dest.top().unwrap_or(0.0)),
        });
    }
    None
}
