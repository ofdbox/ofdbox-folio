//! PDF 目录级结构：大纲与附件。

use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId};

use ofdbox_folio_ir::{AttachmentId, AttachmentInfo, Destination, OutlineItem, Point};

use crate::decode_pdf_string;
use crate::frontend;

const MAX_DEPTH: usize = 32;

/// 1 点等于多少毫米。
const PT_TO_MM: f64 = 25.4 / 72.0;

/// 解析大纲树；目标页解析失败时跳过该条目。
pub(crate) fn outline(document: &Document, pages: &[ObjectId]) -> Vec<OutlineItem> {
    let Some(catalog) = catalog(document) else {
        return Vec::new();
    };
    let Ok(outlines) = catalog.get(b"Outlines") else {
        return Vec::new();
    };
    let page_index = pages
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index))
        .collect::<HashMap<_, _>>();
    let mut visited = HashSet::new();
    let first = resolve(document, outlines)
        .and_then(|object| object.as_dict().ok())
        .and_then(|dictionary| dictionary.get(b"First").ok());
    let Some(first) = first else {
        return Vec::new();
    };
    siblings(document, first, &page_index, &mut visited, 0)
}

fn siblings(
    document: &Document,
    first: &Object,
    page_index: &HashMap<ObjectId, usize>,
    visited: &mut HashSet<ObjectId>,
    depth: usize,
) -> Vec<OutlineItem> {
    if depth > MAX_DEPTH {
        return Vec::new();
    }
    let mut items = Vec::new();
    let mut current = Some(first.clone());
    let mut guard = 0usize;
    while let Some(object) = current {
        if guard > 10_000 {
            break;
        }
        guard += 1;
        let Some((id, dictionary)) = resolve_with_id(document, &object) else {
            break;
        };
        if let Some(id) = id
            && !visited.insert(id)
        {
            break;
        }
        if let Some(item) = item(document, dictionary, page_index, visited, depth) {
            items.push(item);
        }
        current = dictionary.get(b"Next").ok().cloned();
    }
    items
}

fn item(
    document: &Document,
    dictionary: &Dictionary,
    page_index: &HashMap<ObjectId, usize>,
    visited: &mut HashSet<ObjectId>,
    depth: usize,
) -> Option<OutlineItem> {
    let title = dictionary
        .get(b"Title")
        .ok()
        .and_then(|object| resolve(document, object))
        .and_then(|object| object.as_str().ok())
        .map(decode_pdf_string)
        .filter(|title| !title.is_empty())?;
    let destination = dictionary
        .get(b"Dest")
        .ok()
        .or_else(|| {
            dictionary
                .get(b"A")
                .ok()
                .and_then(|object| resolve(document, object))
                .and_then(|object| object.as_dict().ok())
                .filter(|action| {
                    action
                        .get(b"S")
                        .ok()
                        .and_then(|object| object.as_name().ok())
                        == Some(b"GoTo")
                })
                .and_then(|action| action.get(b"D").ok())
        })
        .and_then(|object| destination(document, object, page_index))?;
    let children = dictionary
        .get(b"First")
        .ok()
        .map(|first| siblings(document, first, page_index, visited, depth + 1))
        .unwrap_or_default();
    Some(OutlineItem {
        title,
        destination,
        children,
    })
}

fn destination(
    document: &Document,
    object: &Object,
    page_index: &HashMap<ObjectId, usize>,
) -> Option<Destination> {
    let object = resolve(document, object)?;
    let values = match object {
        Object::Array(values) => values.clone(),
        Object::Name(name) => named_destination(document, name)?,
        Object::String(bytes, _) => named_destination(document, bytes)?,
        _ => return None,
    };
    let page_id = values.first()?.as_reference().ok()?;
    let page = *page_index.get(&page_id)?;
    let page_height = frontend::page_info(document, page_id)
        .ok()
        .map(|info| info.crop_box.height)
        .unwrap_or(0.0);
    // 目标区域坐标是 PDF 用户空间单位（点），IR 用毫米。
    let to_mm = |value: f64| value * PT_TO_MM;
    let point = match values.get(1).and_then(|object| object.as_name().ok()) {
        Some(b"XYZ") => {
            let x = values.get(2).and_then(number).map(to_mm).unwrap_or(0.0);
            let top = values
                .get(3)
                .and_then(number)
                .map(to_mm)
                .unwrap_or(page_height);
            Point::new(x, (page_height - top).max(0.0))
        }
        Some(b"FitH") | Some(b"FitBH") => {
            let top = values
                .get(2)
                .and_then(number)
                .map(to_mm)
                .unwrap_or(page_height);
            Point::new(0.0, (page_height - top).max(0.0))
        }
        Some(b"FitV") | Some(b"FitBV") => {
            let left = values.get(2).and_then(number).map(to_mm).unwrap_or(0.0);
            Point::new(left, 0.0)
        }
        Some(b"FitR") => {
            let left = values.get(2).and_then(number).map(to_mm).unwrap_or(0.0);
            Point::new(left, 0.0)
        }
        _ => Point::new(0.0, 0.0),
    };
    Some(Destination { page, point })
}

fn named_destination(document: &Document, name: &[u8]) -> Option<Vec<Object>> {
    let catalog = catalog(document)?;
    if let Ok(dests) = catalog.get(b"Dests")
        && let Some(dictionary) = resolve(document, dests).and_then(|object| object.as_dict().ok())
        && let Ok(value) = dictionary.get(name)
        && let Some(array) = resolve(document, value).and_then(|object| object.as_array().ok())
    {
        return Some(array.clone());
    }
    let names = catalog.get(b"Names").ok()?;
    let names = resolve(document, names)?.as_dict().ok()?;
    let dests = names.get(b"Dests").ok()?;
    let mut result = None;
    name_tree(document, dests, 0, &mut |key, value| {
        if result.is_none() && key == name {
            result = resolve(document, value)
                .and_then(|object| object.as_array().ok())
                .cloned();
        }
    });
    result
}

/// 解析 EmbeddedFiles 名称树。
pub(crate) fn attachments(
    document: &Document,
) -> (Vec<AttachmentInfo>, Vec<(AttachmentId, ObjectId)>) {
    let mut files = Vec::new();
    let Some(catalog) = catalog(document) else {
        return (Vec::new(), Vec::new());
    };
    let Some(names) = catalog
        .get(b"Names")
        .ok()
        .and_then(|object| resolve(document, object))
        .and_then(|object| object.as_dict().ok())
        .and_then(|names| names.get(b"EmbeddedFiles").ok())
    else {
        return (Vec::new(), Vec::new());
    };
    name_tree(document, names, 0, &mut |key, value| {
        if let Some(file) = file_spec(document, key, value) {
            files.push(file);
        }
    });
    let mut infos = Vec::with_capacity(files.len());
    let mut streams = Vec::with_capacity(files.len());
    for (index, (name, format, object, size)) in files.into_iter().enumerate() {
        let id = AttachmentId::new(index as u64 + 1);
        infos.push(AttachmentInfo {
            id,
            name,
            format,
            size,
            created: None,
        });
        streams.push((id, object));
    }
    (infos, streams)
}

type FileSpec = (String, Option<String>, ObjectId, u64);

fn file_spec(document: &Document, key: &[u8], value: &Object) -> Option<FileSpec> {
    let dictionary = resolve(document, value)?.as_dict().ok()?;
    let name = dictionary
        .get(b"UF")
        .ok()
        .or_else(|| dictionary.get(b"F").ok())
        .and_then(|object| resolve(document, object))
        .and_then(|object| object.as_str().ok())
        .map(decode_pdf_string)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            let key = decode_pdf_string(key);
            (!key.is_empty()).then_some(key)
        })?;
    let embedded = dictionary.get(b"EF").ok()?;
    let embedded = resolve(document, embedded)?.as_dict().ok()?;
    let file = embedded
        .get(b"F")
        .ok()
        .or_else(|| embedded.get(b"UF").ok())?;
    let (_, object) = document.dereference(file).ok()?;
    let stream = object.as_stream().ok()?;
    let id = file.as_reference().ok()?;
    let size = stream
        .decompressed_content()
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(stream.content.len() as u64);
    let format = stream
        .dict
        .get(b"Subtype")
        .ok()
        .or_else(|| dictionary.get(b"Subtype").ok())
        .and_then(|object| resolve(document, object))
        .and_then(|object| object.as_name().ok())
        .map(|name| {
            String::from_utf8_lossy(name)
                .replace("#2F", "/")
                .replace("#20", " ")
        })
        .or_else(|| {
            name.rsplit_once('.')
                .map(|(_, extension)| extension.to_ascii_lowercase())
        });
    Some((name, format, id, size))
}

fn name_tree(
    document: &Document,
    node: &Object,
    depth: usize,
    visit: &mut impl FnMut(&[u8], &Object),
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Some(dictionary) = resolve(document, node).and_then(|object| object.as_dict().ok()) else {
        return;
    };
    if let Ok(names) = dictionary.get(b"Names")
        && let Some(names) = resolve(document, names).and_then(|object| object.as_array().ok())
    {
        for pair in names.as_chunks::<2>().0 {
            let key = resolve(document, &pair[0]).and_then(|object| match object {
                Object::String(bytes, _) | Object::Name(bytes) => Some(bytes.as_slice()),
                _ => None,
            });
            if let Some(key) = key {
                visit(key, &pair[1]);
            }
        }
    }
    if let Ok(kids) = dictionary.get(b"Kids")
        && let Some(kids) = resolve(document, kids).and_then(|object| object.as_array().ok())
    {
        for kid in kids {
            name_tree(document, kid, depth + 1, visit);
        }
    }
}

fn catalog(document: &Document) -> Option<&Dictionary> {
    let root = document.trailer.get(b"Root").ok()?;
    resolve(document, root)?.as_dict().ok()
}

fn resolve<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Object> {
    document.dereference(object).ok().map(|(_, object)| object)
}

fn resolve_with_id<'a>(
    document: &'a Document,
    object: &'a Object,
) -> Option<(Option<ObjectId>, &'a Dictionary)> {
    let (id, object) = document.dereference(object).ok()?;
    Some((id, object.as_dict().ok()?))
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}
