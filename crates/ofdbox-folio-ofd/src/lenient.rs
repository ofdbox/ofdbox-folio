//! Lenient XML repair.
//!
//! Normalizes the non-standard constructs commonly found in real OFD files (unbound namespace
//! prefixes, missing required attributes/child elements, comma-separated numeric values, and so
//! on) into a form that the typed model of `ofdbox-core` can parse strictly.
//! Only "fill in gaps + normalize" is done; existing semantics are not rewritten, and unknown
//! elements/attributes are kept as-is (the model simply skips them).

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::{OfdDiagnostic, OfdError, OfdRepair};

/// Normalizes a single XML entry.
pub(crate) fn sanitize(
    entry: &str,
    bytes: &[u8],
) -> Result<(Vec<u8>, Vec<OfdDiagnostic>), OfdError> {
    let text = String::from_utf8_lossy(bytes);
    let root = parse(&text)?;
    let mut state = State {
        next_id: max_numeric_id(&root) + 1,
    };
    let mut diagnostics = Vec::new();
    let Some(root) = repair(root, entry, &mut state, &mut diagnostics)? else {
        return Err(OfdError::InvalidStructure(format!(
            "{entry}: root element was dropped after lenient parsing"
        )));
    };
    let mut output = String::with_capacity(text.len() + 64);
    output.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_node(&root, true, &mut output);
    Ok((output.into_bytes(), diagnostics))
}

#[derive(Debug)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
}

struct State {
    next_id: u32,
}

fn parse(text: &str) -> Result<Node, OfdError> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|error| OfdError::InvalidStructure(format!("XML parse failed: {error}")))?;
        match event {
            Event::Start(element) => {
                stack.push(node_from(&element)?);
            }
            Event::Empty(element) => {
                push_node(&mut stack, &mut root, node_from(&element)?);
            }
            Event::End(_) => {
                let node = stack.pop().ok_or_else(|| {
                    OfdError::InvalidStructure("XML has more end tags than start tags".to_owned())
                })?;
                push_node(&mut stack, &mut root, node);
            }
            Event::Text(value) => {
                if let Some(node) = stack.last_mut() {
                    let raw = value.into_inner();
                    let decoded = quick_xml::escape::unescape(raw.as_ref()).map_err(|error| {
                        OfdError::InvalidStructure(format!("XML text decoding failed: {error}"))
                    })?;
                    node.text.push_str(decoded.as_ref());
                }
            }
            Event::CData(value) => {
                if let Some(node) = stack.last_mut() {
                    let raw = value.into_inner();
                    node.text.push_str(raw.as_ref());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    root.ok_or_else(|| OfdError::InvalidStructure("XML has no root element".to_owned()))
}

fn node_from(element: &quick_xml::events::BytesStart<'_>) -> Result<Node, OfdError> {
    let name = local_name(element.name().as_ref());
    let mut attrs = Vec::new();
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| {
            OfdError::InvalidStructure(format!("XML attribute parsing failed: {error}"))
        })?;
        let key = local_name(attribute.key.as_ref());
        if key == "xmlns" || key.is_empty() {
            continue;
        }
        let value = attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|error| {
                OfdError::InvalidStructure(format!("XML attribute decoding failed: {error}"))
            })?
            .into_owned();
        attrs.push((key, value));
    }
    Ok(Node {
        name,
        attrs,
        children: Vec::new(),
        text: String::new(),
    })
}

fn push_node(stack: &mut Vec<Node>, root: &mut Option<Node>, node: Node) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else if root.is_none() {
        *root = Some(node);
    }
}

fn local_name(raw: &str) -> String {
    raw.rsplit(':').next().unwrap_or(raw).to_owned()
}

fn max_numeric_id(node: &Node) -> u32 {
    let mut max = 0;
    for (name, value) in &node.attrs {
        if matches!(name.as_str(), "ID" | "TemplateID" | "ResourceID" | "PageID")
            && let Ok(parsed) = value.trim().parse::<u32>()
        {
            max = max.max(parsed);
        }
    }
    for child in &node.children {
        max = max.max(max_numeric_id(child));
    }
    max
}

fn repair(
    mut node: Node,
    entry: &str,
    state: &mut State,
    diagnostics: &mut Vec<OfdDiagnostic>,
) -> Result<Option<Node>, OfdError> {
    clean_numbers(&mut node);
    let name = node.name.clone();

    // An image without a resource reference cannot be rendered: drop the whole block and record a diagnostic.
    if name == "ImageObject" && !has_attr(&node, "ResourceID") {
        record(entry, OfdRepair::DropObject, &name, diagnostics);
        return Ok(None);
    }

    let mut children = Vec::with_capacity(node.children.len());
    for child in std::mem::take(&mut node.children) {
        if let Some(child) = repair(child, entry, state, diagnostics)? {
            children.push(child);
        }
    }
    node.children = children;

    match name.as_str() {
        "Res" => ensure_attr(&mut node, "BaseLoc", ".", entry, diagnostics),
        "Layer"
        | "PageBlock"
        | "Font"
        | "ColorSpace"
        | "DrawParam"
        | "CompositeGraphicUnit"
        | "MultiMedia" => ensure_id(&mut node, entry, state, diagnostics),
        "TextObject" | "PathObject" | "ImageObject" | "CompositeObject" => {
            ensure_id(&mut node, entry, state, diagnostics);
            ensure_attr(&mut node, "Boundary", "0 0 0 0", entry, diagnostics);
        }
        "Path" | "Text" => {
            // Path/Text elements inside a clipping area have no ID (CT_Path/CT_Text in the XSD carry no ID).
            ensure_attr(&mut node, "Boundary", "0 0 0 0", entry, diagnostics);
            if name == "Text" {
                ensure_attr(&mut node, "Font", "0", entry, diagnostics);
                ensure_attr(&mut node, "Size", "0", entry, diagnostics);
            }
        }
        "Action" => ensure_attr(&mut node, "Event", "DO", entry, diagnostics),
        "Dest" => {
            ensure_attr(&mut node, "Type", "XYZ", entry, diagnostics);
            ensure_attr(&mut node, "PageID", "0", entry, diagnostics);
        }
        "Annot" => {
            ensure_id(&mut node, entry, state, diagnostics);
            ensure_attr(&mut node, "Type", "Link", entry, diagnostics);
            ensure_attr(&mut node, "Creator", "", entry, diagnostics);
            ensure_attr(&mut node, "LastModDate", "", entry, diagnostics);
        }
        "OutlineElem" => ensure_attr(&mut node, "Title", "", entry, diagnostics),
        "Template" => ensure_attr(&mut node, "TemplateID", "0", entry, diagnostics),
        "Print" => ensure_attr(&mut node, "Printable", "true", entry, diagnostics),
        _ => {}
    }

    match name.as_str() {
        "CommonData" => ensure_page_area(&mut node, entry, diagnostics),
        "PageArea" => ensure_physical_box(&mut node, entry, diagnostics),
        "PathObject" | "Path" => ensure_child(&mut node, "AbbreviatedData", entry, diagnostics),
        "MultiMedia" => ensure_child(&mut node, "MediaFile", entry, diagnostics),
        _ => {}
    }

    Ok(Some(node))
}

fn clean_numbers(node: &mut Node) {
    const NUMERIC: &[&str] = &[
        "Boundary",
        "CTM",
        "DeltaX",
        "DeltaY",
        "DashPattern",
        "Value",
        "Position",
        "StartPoint",
        "EndPoint",
        "PhysicalBox",
    ];
    for (name, value) in &mut node.attrs {
        if NUMERIC.contains(&name.as_str()) && (value.contains(',') || value.contains(';')) {
            *value = value.replace([',', ';'], " ");
        }
    }
}

fn has_attr(node: &Node, name: &str) -> bool {
    node.attrs.iter().any(|(key, _)| key == name)
}

fn ensure_attr(
    node: &mut Node,
    name: &str,
    value: &str,
    entry: &str,
    diagnostics: &mut Vec<OfdDiagnostic>,
) {
    if has_attr(node, name) {
        return;
    }
    node.attrs.push((name.to_owned(), value.to_owned()));
    record(entry, OfdRepair::InjectAttribute, &node.name, diagnostics);
}

fn ensure_id(
    node: &mut Node,
    entry: &str,
    state: &mut State,
    diagnostics: &mut Vec<OfdDiagnostic>,
) {
    if has_attr(node, "ID") {
        return;
    }
    let id = state.next_id.to_string();
    state.next_id += 1;
    node.attrs.push(("ID".to_owned(), id));
    record(entry, OfdRepair::InjectId, &node.name, diagnostics);
}

fn ensure_child(node: &mut Node, name: &str, entry: &str, diagnostics: &mut Vec<OfdDiagnostic>) {
    if node.children.iter().any(|child| child.name == name) {
        return;
    }
    node.children.push(Node {
        name: name.to_owned(),
        attrs: Vec::new(),
        children: Vec::new(),
        text: String::new(),
    });
    record(entry, OfdRepair::InjectChild, &node.name, diagnostics);
}

fn ensure_page_area(node: &mut Node, entry: &str, diagnostics: &mut Vec<OfdDiagnostic>) {
    if node.children.iter().any(|child| child.name == "PageArea") {
        return;
    }
    node.children.push(Node {
        name: "PageArea".to_owned(),
        attrs: Vec::new(),
        children: vec![Node {
            name: "PhysicalBox".to_owned(),
            attrs: Vec::new(),
            children: Vec::new(),
            text: "0 0 210 297".to_owned(),
        }],
        text: String::new(),
    });
    record(entry, OfdRepair::InjectChild, "CommonData", diagnostics);
}

fn ensure_physical_box(node: &mut Node, entry: &str, diagnostics: &mut Vec<OfdDiagnostic>) {
    if node
        .children
        .iter()
        .any(|child| child.name == "PhysicalBox")
    {
        return;
    }
    node.children.push(Node {
        name: "PhysicalBox".to_owned(),
        attrs: Vec::new(),
        children: Vec::new(),
        text: "0 0 210 297".to_owned(),
    });
    record(entry, OfdRepair::InjectChild, "PageArea", diagnostics);
}

fn record(entry: &str, repair: OfdRepair, element: &str, diagnostics: &mut Vec<OfdDiagnostic>) {
    diagnostics.push(OfdDiagnostic {
        entry: entry.to_owned(),
        repair,
        detail: format!("<{element}> is missing required content; default values were filled in"),
    });
}

fn write_node(node: &Node, root: bool, output: &mut String) {
    output.push_str("<ofd:");
    output.push_str(&node.name);
    if root {
        output.push_str(" xmlns:ofd=\"http://www.ofdspec.org/2016\"");
    }
    for (name, value) in &node.attrs {
        output.push(' ');
        output.push_str(name);
        output.push_str("=\"");
        escape_attr(value, output);
        output.push('"');
    }
    if node.children.is_empty() && node.text.is_empty() {
        output.push_str("/>");
        return;
    }
    output.push('>');
    if !node.text.is_empty() {
        escape_text(&node.text, output);
    }
    for child in &node.children {
        write_node(child, false, output);
    }
    output.push_str("</ofd:");
    output.push_str(&node.name);
    output.push('>');
}

fn escape_text(value: &str, output: &mut String) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            other => output.push(other),
        }
    }
}

fn escape_attr(value: &str, output: &mut String) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '"' => output.push_str("&quot;"),
            other => output.push(other),
        }
    }
}
