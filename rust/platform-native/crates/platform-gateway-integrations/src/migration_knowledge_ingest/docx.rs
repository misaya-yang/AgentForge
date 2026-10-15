//! Bounded python-docx-compatible document/paragraph/table projection.
//! Nested tables are absent from _Cell.text, while grid-span cells repeat and
//! vertical continuations resolve the prior row at the physical grid offset.
use super::parser::{MAX_PARTS, MAX_TEXT_BYTES, MAX_TEXT_CHARS, sanitize_text};
use super::{IngestError, Result};
use quick_xml::{events::Event, name::ResolveResult};
use std::{collections::BTreeMap, sync::Arc};
const WORD: &[u8] = b"http://schemas.openxmlformats.org/wordprocessingml/2006/main";
fn bad(message: impl Into<String>) -> IngestError {
    IngestError::Parser(message.into())
}
#[derive(Default)]
struct Node {
    name: Vec<u8>,
    word: bool,
    val: Option<String>,
    kind: Option<String>,
    text: String,
    children: Vec<usize>,
}
impl Node {
    fn is(&self, name: &[u8]) -> bool {
        self.word && self.name == name
    }
}
#[derive(Default)]
struct Text {
    value: String,
    chars: usize,
    parts: usize,
}
impl Text {
    fn append(&mut self, value: &str, separator: &str) -> Result<()> {
        if value.is_empty() {
            return Ok(());
        }
        let separator = if self.value.is_empty() { "" } else { separator };
        let chars = self
            .chars
            .saturating_add(value.chars().count())
            .saturating_add(separator.chars().count());
        let bytes = self
            .value
            .len()
            .saturating_add(value.len())
            .saturating_add(separator.len());
        if chars > MAX_TEXT_CHARS {
            return Err(IngestError::Validation(format!(
                "extracted text exceeds the {MAX_TEXT_CHARS} character limit"
            )));
        }
        if bytes > MAX_TEXT_BYTES {
            return Err(IngestError::Validation(format!(
                "extracted text exceeds the {MAX_TEXT_BYTES} byte limit"
            )));
        }
        self.value.push_str(separator);
        self.value.push_str(value);
        self.chars = chars;
        Ok(())
    }
    fn block(&mut self, value: &str, separator: &str) -> Result<()> {
        if value.is_empty() {
            return Ok(());
        }
        if self.parts >= MAX_PARTS {
            return Err(IngestError::Validation(format!(
                "extracted text exceeds the {MAX_PARTS} fragment limit"
            )));
        }
        self.append(value, separator)?;
        self.parts += 1;
        Ok(())
    }
}
fn tree(data: &[u8]) -> Result<Vec<Node>> {
    let mut reader = quick_xml::NsReader::from_reader(data);
    let mut nodes = vec![Node::default()];
    let mut stack = vec![0usize];
    loop {
        let event = reader
            .read_event()
            .map_err(|_| bad("DOCX XML is malformed"))?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                if nodes.len() > 100_000 || stack.len() > 128 {
                    return Err(bad("DOCX XML exceeds the parser structure budget"));
                }
                let (namespace, local) = reader.resolver().resolve_element(element.name());
                if matches!(&namespace, ResolveResult::Unknown(_)) {
                    return Err(bad("DOCX XML contains an unbound namespace prefix"));
                }
                let mut node = Node {
                    name: local.as_ref().to_vec(),
                    word: matches!(namespace,ResolveResult::Bound(ns) if ns.as_ref()==WORD),
                    ..Node::default()
                };
                for attribute in element.attributes() {
                    let attribute =
                        attribute.map_err(|_| bad("DOCX XML attribute is malformed"))?;
                    let (namespace, local) = reader.resolver().resolve_attribute(attribute.key);
                    if matches!(&namespace, ResolveResult::Unknown(_)) {
                        return Err(bad("DOCX XML contains an unbound namespace prefix"));
                    }
                    if matches!(namespace,ResolveResult::Bound(ns) if ns.as_ref()==WORD) {
                        let target = match local.as_ref() {
                            b"val" => Some(&mut node.val),
                            b"type" => Some(&mut node.kind),
                            _ => None,
                        };
                        if let Some(target) = target {
                            *target = Some(
                                attribute
                                    .decode_and_unescape_value(reader.decoder())
                                    .map_err(|_| bad("DOCX XML attribute is malformed"))?
                                    .into_owned(),
                            );
                        }
                    }
                }
                let index = nodes.len();
                nodes.push(node);
                nodes[*stack.last().unwrap()].children.push(index);
                if !empty {
                    stack.push(index);
                }
            }
            Event::Text(text) => {
                let text = text
                    .decode()
                    .map_err(|_| bad("DOCX XML text escape is invalid"))?;
                nodes[*stack.last().unwrap()].text.push_str(&text);
            }
            Event::CData(text) => {
                let text = text
                    .decode()
                    .map_err(|_| bad("DOCX XML text escape is invalid"))?;
                nodes[*stack.last().unwrap()].text.push_str(&text);
            }
            Event::GeneralRef(reference) => {
                let value = if let Some(c) = reference
                    .resolve_char_ref()
                    .map_err(|_| bad("DOCX XML character reference is invalid"))?
                {
                    c.to_string()
                } else {
                    match reference
                        .decode()
                        .map_err(|_| bad("DOCX XML reference is invalid"))?
                        .as_ref()
                    {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => return Err(bad("DOCX XML entity is unsupported")),
                    }
                    .into()
                };
                nodes[*stack.last().unwrap()].text.push_str(&value);
            }
            Event::End(_) => {
                if stack.len() <= 1 {
                    return Err(bad("DOCX XML is malformed"));
                }
                stack.pop();
            }
            Event::DocType(_) => {
                return Err(bad("DOCX XML document type declarations are not supported"));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err(bad("DOCX XML is malformed"));
    }
    Ok(nodes)
}
fn child(nodes: &[Node], parent: usize, name: &[u8]) -> Option<usize> {
    nodes[parent]
        .children
        .iter()
        .copied()
        .find(|index| nodes[*index].is(name))
}
fn decimal(
    nodes: &[Node],
    parent: usize,
    property: &[u8],
    field: &[u8],
    default: i64,
) -> Result<i64> {
    let value = child(nodes, parent, property).and_then(|p| child(nodes, p, field));
    match value {
        None => Ok(default),
        Some(index) => nodes[index]
            .val
            .as_deref()
            .ok_or_else(|| bad("Failed to parse DOCX: required w:val attribute is missing"))?
            .parse::<i64>()
            .map_err(|_| bad("Failed to parse DOCX: invalid integer table property")),
    }
}
fn run(nodes: &[Node], index: usize, text: &mut Text) -> Result<()> {
    for child in &nodes[index].children {
        let node = &nodes[*child];
        if !node.word {
            continue;
        }
        let value = match node.name.as_slice() {
            b"t" => node.text.as_str(),
            b"tab" | b"ptab" => "\t",
            b"cr" => "\n",
            b"noBreakHyphen" => "-",
            b"br" if node.kind.as_deref().unwrap_or("textWrapping") == "textWrapping" => "\n",
            _ => "",
        };
        text.append(value, "")?;
    }
    Ok(())
}
fn paragraph(nodes: &[Node], index: usize) -> Result<String> {
    let mut text = Text::default();
    for item in &nodes[index].children {
        if nodes[*item].is(b"r") {
            run(nodes, *item, &mut text)?;
        } else if nodes[*item].is(b"hyperlink") {
            for item in &nodes[*item].children {
                if nodes[*item].is(b"r") {
                    run(nodes, *item, &mut text)?;
                }
            }
        }
    }
    Ok(text.value)
}
fn cell_text(nodes: &[Node], index: usize) -> Result<String> {
    let mut out = Text::default();
    let mut first = true;
    for p in &nodes[index].children {
        if nodes[*p].is(b"p") {
            if !first {
                out.append("\n", "")?;
            }
            first = false;
            out.append(&paragraph(nodes, *p)?, "")?;
        }
    }
    Ok(out.value.trim().replace('|', "\\|").replace('\n', " "))
}
#[derive(Clone)]
struct Cell {
    text: Arc<str>,
    span: i64,
}
fn table(nodes: &[Node], index: usize, out: &mut Text) -> Result<()> {
    let mut previous: BTreeMap<i64, Cell> = BTreeMap::new();
    let mut first = true;
    let mut row_index = 0;
    for row in &nodes[index].children {
        if nodes[*row].is(b"tr") {
            let mut offset = decimal(nodes, *row, b"trPr", b"gridBefore", 0)?;
            let mut current = BTreeMap::new();
            let mut cells = vec![];
            for tc in &nodes[*row].children {
                if nodes[*tc].is(b"tc") {
                    let span = decimal(nodes, *tc, b"tcPr", b"gridSpan", 1)?;
                    let merge =
                        child(nodes, *tc, b"tcPr").and_then(|pr| child(nodes, pr, b"vMerge"));
                    let continuation = merge.is_some_and(|m| {
                        nodes[m].val.as_deref().unwrap_or("continue") == "continue"
                    });
                    if merge.is_some_and(|m| {
                        !matches!(nodes[m].val.as_deref(), None | Some("continue" | "restart"))
                    }) {
                        return Err(bad("Failed to parse DOCX: invalid vertical merge value"));
                    }
                    let cell = if continuation {
                        if row_index == 0 {
                            return Err(bad(
                                "Failed to parse DOCX: no tr above topmost tr in w:tbl",
                            ));
                        }
                        previous.get(&offset).cloned().ok_or_else(|| {
                            bad(format!(
                                "Failed to parse DOCX: no `tc` element at grid_offset={offset}"
                            ))
                        })?
                    } else {
                        Cell {
                            text: Arc::from(cell_text(nodes, *tc)?),
                            span,
                        }
                    };
                    current.insert(offset, cell.clone());
                    cells.push(cell);
                    offset = offset.checked_add(span).ok_or_else(|| {
                        bad("Failed to parse DOCX: table grid offset exceeds integer range")
                    })?;
                }
            }
            let count = cells
                .iter()
                .try_fold(0usize, |n, c| n.checked_add(c.span.max(0) as usize))
                .ok_or_else(|| bad("DOCX table expansion exceeds the text budget"))?;
            // Each projected cell costs at least a separator. Reject before large
            // merged spans can allocate an unbounded list, even for empty cells.
            if count > MAX_TEXT_CHARS / 3 {
                return Err(IngestError::Validation(format!(
                    "extracted text exceeds the {MAX_TEXT_CHARS} character limit"
                )));
            }
            if count > 0 {
                let mut line = Text::default();
                line.append("| ", "")?;
                let mut emitted = false;
                for cell in cells {
                    for _ in 0..cell.span.max(0) {
                        if emitted {
                            line.append(" | ", "")?;
                        }
                        line.append(&cell.text, "")?;
                        emitted = true;
                    }
                }
                line.append(" |", "")?;
                out.block(&line.value, if first { "\n\n" } else { "\n" })?;
                if first {
                    let mut line = Text::default();
                    line.append("| ", "")?;
                    for i in 0..count {
                        line.append("---", if i == 0 { "" } else { " | " })?;
                    }
                    line.append(" |", "")?;
                    out.block(&line.value, "\n")?;
                    first = false;
                }
            }
            previous = current;
            row_index += 1;
        }
    }
    Ok(())
}
pub(super) fn extract_document_xml(data: &[u8]) -> Result<String> {
    let nodes = tree(data)?;
    let document = child(&nodes, 0, b"document")
        .ok_or_else(|| bad("Failed to parse DOCX: document root is missing"))?;
    let body = child(&nodes, document, b"body")
        .ok_or_else(|| bad("Failed to parse DOCX: document body is missing"))?;
    let mut out = Text::default();
    for block in &nodes[body].children {
        if nodes[*block].is(b"p") {
            out.block(paragraph(&nodes, *block)?.trim(), "\n")?;
        } else if nodes[*block].is(b"tbl") {
            table(&nodes, *block, &mut out)?;
        }
    }
    let normalized = out.value.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = regex::Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&normalized, "\n\n");
    if normalized.trim().is_empty() {
        return Err(bad("DOCX parsed but no text extracted"));
    }
    sanitize_text(normalized.trim())
}
