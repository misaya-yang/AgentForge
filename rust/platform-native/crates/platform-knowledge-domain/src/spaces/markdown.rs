//! BlockNote JSON is the source of truth. Markdown and HTML are projections.
use super::model::{SpaceError, SpaceResult};
use serde_json::{Value, json};
use std::io::{Cursor, Read};

pub fn project(format: &str, blocks: &Value, markdown: &str) -> SpaceResult<String> {
    match format {
        "binary" => Ok(String::new()),
        "markdown" => bounded(markdown),
        "blocknote_json" => bounded(&blocks_to_markdown(blocks)),
        _ => Err(SpaceError::Invalid("invalid content format")),
    }
}

pub fn blocks_to_markdown(blocks: &Value) -> String {
    let mut out = String::new();
    write_blocks(&mut out, block_list(blocks), 0);
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

pub fn blocks_to_html(blocks: &Value) -> String {
    let mut out = String::new();
    write_html(&mut out, block_list(blocks));
    out
}

pub fn markdown_to_html(markdown: &str) -> String {
    blocks_to_html(&markdown_to_blocks(markdown))
}

pub fn backlink_targets(markdown: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = markdown.as_bytes();
    let mut index = 0;
    while index + 4 < bytes.len() {
        if bytes[index] == b'[' && bytes[index + 1] == b'[' {
            if let Some(end) = markdown[index + 2..].find("]]") {
                let target = markdown[index + 2..index + 2 + end].trim();
                if !target.is_empty() && target.len() <= 500 && !found.iter().any(|item| item == target) {
                    found.push(target.to_owned());
                }
                index += end + 4;
                continue;
            }
        }
        index += 1;
    }
    found
}

pub fn markdown_to_blocks(markdown: &str) -> Value {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if line.starts_with("```") {
            let language = line.trim_start_matches('`').trim().to_owned();
            let mut body = String::new();
            index += 1;
            while index < lines.len() && !lines[index].starts_with("```") {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(lines[index]);
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            blocks.push(code_block(&language, &body));
            continue;
        }
        if line.starts_with("| ") {
            let mut rows = Vec::new();
            while index < lines.len() && lines[index].starts_with("| ") {
                if !lines[index].contains("---") {
                    rows.push(split_row(lines[index]));
                }
                index += 1;
            }
            blocks.push(table_block(&rows));
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            blocks.push(heading(1, rest));
        } else if let Some(rest) = line.strip_prefix("## ") {
            blocks.push(heading(2, rest));
        } else if let Some(rest) = line.strip_prefix("### ") {
            blocks.push(heading(3, rest));
        } else if let Some(rest) = line.strip_prefix("> ") {
            blocks.push(paragraph_block("quote", rest));
        } else if let Some(rest) = line.strip_prefix("- [x] ").or_else(|| line.strip_prefix("- [X] ")) {
            blocks.push(check_block(true, rest));
        } else if let Some(rest) = line.strip_prefix("- [ ] ") {
            blocks.push(check_block(false, rest));
        } else if let Some(rest) = line.strip_prefix("- ") {
            blocks.push(list_block("bulletListItem", rest));
        } else if let Some(rest) = numbered(line) {
            blocks.push(list_block("numberedListItem", rest));
        } else if let Some(url) = image_line(line) {
            blocks.push(image_block(url, ""));
        } else {
            blocks.push(paragraph_block("paragraph", line));
        }
        index += 1;
    }
    Value::Array(blocks)
}

pub fn html_to_blocks(html: &str) -> SpaceResult<Value> {
    if html.len() > 1_000_000 {
        return Err(SpaceError::Invalid("content is too large"));
    }
    let document = scraper::Html::parse_fragment(html);
    let mut blocks = Vec::new();
    for node in document.root_element().child_elements() {
        collect_html(node, &mut blocks);
    }
    Ok(Value::Array(blocks))
}

pub fn read_zip_pages(bytes: &[u8]) -> SpaceResult<Vec<(String, String, Value)>> {
    if bytes.len() > 8_000_000 {
        return Err(SpaceError::Invalid("content is too large"));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| SpaceError::Invalid("invalid zip"))?;
    let mut pages = Vec::new();
    for index in 0..archive.len().min(200) {
        let mut file = archive.by_index(index).map_err(|_| SpaceError::Invalid("invalid zip"))?;
        if !file.is_file() {
            continue;
        }
        let name = file.name().map_err(|_| SpaceError::Invalid("invalid zip"))?.replace('\\', "/");
        if name.starts_with("__MACOSX/") || name.split('/').any(|part| part.starts_with('.')) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if !(lower.ends_with(".md") || lower.ends_with(".html") || lower.ends_with(".htm")) {
            continue;
        }
        let mut body = String::new();
        file.read_to_string(&mut body).map_err(|_| SpaceError::Invalid("invalid zip"))?;
        if body.len() > 1_000_000 {
            return Err(SpaceError::Invalid("content is too large"));
        }
        let blocks = if lower.ends_with(".md") {
            markdown_to_blocks(&body)
        } else {
            html_to_blocks(&body)?
        };
        let title = page_title(&name);
        pages.push((name, title, blocks));
    }
    if pages.is_empty() {
        return Err(SpaceError::Invalid("zip has no pages"));
    }
    pages.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(pages)
}

pub fn line_diff(before: &str, after: &str) -> String {
    let left: Vec<&str> = before.split('\n').collect();
    let right: Vec<&str> = after.split('\n').collect();
    let mut table = vec![vec![0u32; right.len() + 1]; left.len() + 1];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            table[i][j] = if left[i] == right[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut out = String::new();
    let mut i = 0;
    let mut j = 0;
    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            out.push_str("  ");
            out.push_str(left[i]);
            out.push('\n');
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            out.push_str("- ");
            out.push_str(left[i]);
            out.push('\n');
            i += 1;
        } else {
            out.push_str("+ ");
            out.push_str(right[j]);
            out.push('\n');
            j += 1;
        }
    }
    while i < left.len() {
        out.push_str("- ");
        out.push_str(left[i]);
        out.push('\n');
        i += 1;
    }
    while j < right.len() {
        out.push_str("+ ");
        out.push_str(right[j]);
        out.push('\n');
        j += 1;
    }
    out
}

fn bounded(value: &str) -> SpaceResult<String> {
    if value.len() > 1_000_000 {
        Err(SpaceError::Invalid("content is too large"))
    } else {
        Ok(value.to_owned())
    }
}

fn block_list(value: &Value) -> Vec<&Value> {
    if let Some(rows) = value.as_array() {
        return rows.iter().collect();
    }
    for key in ["blocks", "content"] {
        if let Some(rows) = value.get(key).and_then(Value::as_array) {
            return rows.iter().collect();
        }
    }
    Vec::new()
}

fn write_blocks(out: &mut String, blocks: Vec<&Value>, depth: usize) {
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("paragraph");
        let text = inline_markdown(block.get("content").unwrap_or(&Value::Null));
        let indent = "  ".repeat(depth);
        match kind {
            "heading" => {
                let level = block
                    .pointer("/props/level")
                    .and_then(Value::as_i64)
                    .unwrap_or(1)
                    .clamp(1, 3) as usize;
                out.push_str(&"#".repeat(level));
                out.push(' ');
                out.push_str(&text);
                out.push_str("\n\n");
            }
            "bulletListItem" => {
                out.push_str(&indent);
                out.push_str("- ");
                out.push_str(&text);
                out.push('\n');
            }
            "numberedListItem" => {
                out.push_str(&indent);
                out.push_str("1. ");
                out.push_str(&text);
                out.push('\n');
            }
            "checkListItem" => {
                let checked = block.pointer("/props/checked").and_then(Value::as_bool).unwrap_or(false);
                out.push_str(&indent);
                out.push_str(if checked { "- [x] " } else { "- [ ] " });
                out.push_str(&text);
                out.push('\n');
            }
            "codeBlock" => {
                let language = block.pointer("/props/language").and_then(Value::as_str).unwrap_or("");
                out.push_str("```");
                out.push_str(language);
                out.push('\n');
                out.push_str(&text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("```\n\n");
            }
            "quote" => {
                out.push_str("> ");
                out.push_str(&text);
                out.push_str("\n\n");
            }
            "image" => {
                let url = block.pointer("/props/url").and_then(Value::as_str).unwrap_or("");
                let caption = block.pointer("/props/caption").and_then(Value::as_str).unwrap_or("");
                out.push_str("![");
                out.push_str(caption);
                out.push_str("](");
                out.push_str(url);
                out.push_str(")\n\n");
            }
            "table" => {
                out.push_str(&table_markdown(block.get("content").unwrap_or(&Value::Null)));
                out.push('\n');
            }
            "divider" => out.push_str("---\n\n"),
            _ => {
                if !text.is_empty() {
                    out.push_str(&text);
                    out.push_str("\n\n");
                }
            }
        }
        if let Some(children) = block.get("children").and_then(Value::as_array) {
            write_blocks(out, children.iter().collect(), depth + 1);
        }
    }
}

fn write_html(out: &mut String, blocks: Vec<&Value>) {
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("paragraph");
        let text = inline_html(block.get("content").unwrap_or(&Value::Null));
        match kind {
            "heading" => {
                let level = block.pointer("/props/level").and_then(Value::as_i64).unwrap_or(1).clamp(1, 3);
                out.push_str(&format!("<h{level}>{text}</h{level}>"));
            }
            "bulletListItem" => out.push_str(&format!("<ul><li>{text}</li></ul>")),
            "numberedListItem" => out.push_str(&format!("<ol><li>{text}</li></ol>")),
            "checkListItem" => {
                let checked = block.pointer("/props/checked").and_then(Value::as_bool).unwrap_or(false);
                out.push_str(&format!(
                    "<ul><li><input type=\"checkbox\" disabled{}> {text}</li></ul>",
                    if checked { " checked" } else { "" }
                ));
            }
            "codeBlock" => {
                let language = escape(block.pointer("/props/language").and_then(Value::as_str).unwrap_or(""));
                out.push_str(&format!("<pre><code data-language=\"{language}\">{text}</code></pre>"));
            }
            "quote" => out.push_str(&format!("<blockquote><p>{text}</p></blockquote>")),
            "image" => {
                let url = escape(block.pointer("/props/url").and_then(Value::as_str).unwrap_or(""));
                let caption = escape(block.pointer("/props/caption").and_then(Value::as_str).unwrap_or(""));
                out.push_str(&format!("<figure><img src=\"{url}\" alt=\"{caption}\"></figure>"));
            }
            "table" => out.push_str(&table_html(block.get("content").unwrap_or(&Value::Null))),
            _ => {
                if !text.is_empty() {
                    out.push_str(&format!("<p>{text}</p>"));
                }
            }
        }
        if let Some(children) = block.get("children").and_then(Value::as_array) {
            write_html(out, children.iter().collect());
        }
    }
}

fn inline_markdown(content: &Value) -> String {
    let Some(parts) = content.as_array() else {
        return content.as_str().unwrap_or("").to_owned();
    };
    let mut out = String::new();
    for part in parts {
        if part.get("type").and_then(Value::as_str) == Some("link") {
            let href = part.get("href").and_then(Value::as_str).unwrap_or("");
            out.push('[');
            out.push_str(&inline_markdown(part.get("content").unwrap_or(&Value::Null)));
            out.push_str("](");
            out.push_str(href);
            out.push(')');
            continue;
        }
        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
        let styles = part.get("styles").cloned().unwrap_or(Value::Null);
        let mut wrapped = text.to_owned();
        if styles.get("code").and_then(Value::as_bool) == Some(true) {
            wrapped = format!("`{wrapped}`");
        }
        if styles.get("bold").and_then(Value::as_bool) == Some(true) {
            wrapped = format!("**{wrapped}**");
        }
        if styles.get("italic").and_then(Value::as_bool) == Some(true) {
            wrapped = format!("*{wrapped}*");
        }
        if styles.get("strike").and_then(Value::as_bool) == Some(true) {
            wrapped = format!("~~{wrapped}~~");
        }
        out.push_str(&wrapped);
    }
    out
}

fn inline_html(content: &Value) -> String {
    let Some(parts) = content.as_array() else {
        return escape(content.as_str().unwrap_or(""));
    };
    let mut out = String::new();
    for part in parts {
        if part.get("type").and_then(Value::as_str) == Some("link") {
            let href = escape(part.get("href").and_then(Value::as_str).unwrap_or(""));
            out.push_str(&format!(
                "<a href=\"{href}\">{}</a>",
                inline_html(part.get("content").unwrap_or(&Value::Null))
            ));
            continue;
        }
        let mut text = escape(part.get("text").and_then(Value::as_str).unwrap_or(""));
        let styles = part.get("styles").cloned().unwrap_or(Value::Null);
        if styles.get("code").and_then(Value::as_bool) == Some(true) {
            text = format!("<code>{text}</code>");
        }
        if styles.get("bold").and_then(Value::as_bool) == Some(true) {
            text = format!("<strong>{text}</strong>");
        }
        if styles.get("italic").and_then(Value::as_bool) == Some(true) {
            text = format!("<em>{text}</em>");
        }
        if styles.get("strike").and_then(Value::as_bool) == Some(true) {
            text = format!("<s>{text}</s>");
        }
        out.push_str(&text);
    }
    out
}

fn table_markdown(content: &Value) -> String {
    let rows = table_rows(content);
    if rows.is_empty() {
        return String::new();
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        out.push_str("| ");
        for cell in 0..width {
            out.push_str(row.get(cell).map(String::as_str).unwrap_or(""));
            out.push_str(" | ");
        }
        out.push('\n');
        if index == 0 {
            out.push_str("| ");
            for _ in 0..width {
                out.push_str("--- | ");
            }
            out.push('\n');
        }
    }
    out
}

fn table_html(content: &Value) -> String {
    let rows = table_rows(content);
    let mut out = String::from("<table>");
    for (index, row) in rows.iter().enumerate() {
        let tag = if index == 0 { "th" } else { "td" };
        out.push_str("<tr>");
        for cell in row {
            out.push_str(&format!("<{tag}>{}</{tag}>", escape(cell)));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

fn table_rows(content: &Value) -> Vec<Vec<String>> {
    let rows = content.get("rows").and_then(Value::as_array).or_else(|| content.as_array());
    let Some(rows) = rows else {
        return Vec::new();
    };
    rows.iter()
        .map(|row| {
            let cells = row.get("cells").and_then(Value::as_array).or_else(|| row.as_array());
            cells
                .map(|cells| {
                    cells
                        .iter()
                        .map(|cell| {
                            if cell.is_array() || cell.get("content").is_some() {
                                inline_markdown(cell)
                            } else {
                                cell.as_str().unwrap_or("").to_owned()
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn text_content(text: &str) -> Value {
    json!([{"type":"text","text":text,"styles":{}}])
}

fn block(kind: &str, props: Value, content: Value) -> Value {
    json!({"id":"","type":kind,"props":props,"content":content,"children":[]})
}

fn heading(level: i64, text: &str) -> Value {
    block("heading", json!({"level":level}), text_content(text.trim()))
}

fn paragraph_block(kind: &str, text: &str) -> Value {
    block(kind, json!({}), text_content(text.trim()))
}

fn list_block(kind: &str, text: &str) -> Value {
    block(kind, json!({}), text_content(text.trim()))
}

fn check_block(checked: bool, text: &str) -> Value {
    block("checkListItem", json!({"checked":checked}), text_content(text.trim()))
}

fn code_block(language: &str, body: &str) -> Value {
    block("codeBlock", json!({"language":language}), text_content(body))
}

fn image_block(url: &str, caption: &str) -> Value {
    block("image", json!({"url":url,"caption":caption}), Value::Null)
}

fn table_block(rows: &[Vec<String>]) -> Value {
    let content = json!({
        "type":"tableContent",
        "rows": rows.iter().map(|row| json!({"cells": row.iter().map(|cell| text_content(cell)).collect::<Vec<_>>()})).collect::<Vec<_>>()
    });
    block("table", json!({}), content)
}

fn numbered(line: &str) -> Option<&str> {
    let (digits, rest) = line.split_once(". ")?;
    if digits.chars().all(|ch| ch.is_ascii_digit()) && !digits.is_empty() {
        Some(rest)
    } else {
        None
    }
}

fn image_line(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix("![")?;
    let (_, url) = rest.split_once("](")?;
    url.strip_suffix(')')
}

fn split_row(line: &str) -> Vec<String> {
    line.trim().trim_matches('|').split('|').map(|cell| cell.trim().to_owned()).collect()
}

fn page_title(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    let title = stem.replace(['_', '-'], " ").trim().to_owned();
    if title.is_empty() { "page".into() } else { title }
}

fn collect_html(node: scraper::ElementRef<'_>, blocks: &mut Vec<Value>) {
    let name = node.value().name();
    let text = node.text().collect::<String>();
    let trimmed = text.trim();
    match name {
        "h1" => blocks.push(heading(1, trimmed)),
        "h2" => blocks.push(heading(2, trimmed)),
        "h3" | "h4" | "h5" | "h6" => blocks.push(heading(3, trimmed)),
        "p" => {
            if !trimmed.is_empty() {
                blocks.push(paragraph_block("paragraph", trimmed));
            }
        }
        "blockquote" => {
            if !trimmed.is_empty() {
                blocks.push(paragraph_block("quote", trimmed));
            }
        }
        "pre" => blocks.push(code_block("", trimmed)),
        "ul" | "ol" => {
            let kind = if name == "ol" { "numberedListItem" } else { "bulletListItem" };
            for child in node.child_elements() {
                if child.value().name() == "li" {
                    let item = child.text().collect::<String>();
                    let checked = item.contains("[x]") || item.contains("[X]");
                    let cleaned = item.trim().trim_start_matches("[x]").trim_start_matches("[X]").trim_start_matches("[ ]").trim();
                    if item.contains("[ ]") || item.contains("[x]") || item.contains("[X]") {
                        blocks.push(check_block(checked, cleaned));
                    } else if !cleaned.is_empty() {
                        blocks.push(list_block(kind, cleaned));
                    }
                }
            }
        }
        "img" => {
            let url = node.value().attr("src").unwrap_or("");
            let caption = node.value().attr("alt").unwrap_or("");
            if !url.is_empty() {
                blocks.push(image_block(url, caption));
            }
        }
        "table" => {
            let mut rows = Vec::new();
            for row in node.descendants() {
                let Some(element) = scraper::ElementRef::wrap(row) else { continue };
                if element.value().name() != "tr" {
                    continue;
                }
                let cells = element
                    .child_elements()
                    .filter(|cell| cell.value().name() == "td" || cell.value().name() == "th")
                    .map(|cell| cell.text().collect::<String>().trim().to_owned())
                    .collect::<Vec<_>>();
                if !cells.is_empty() {
                    rows.push(cells);
                }
            }
            if !rows.is_empty() {
                blocks.push(table_block(&rows));
            }
        }
        "html" | "body" | "div" | "article" | "section" | "figure" => {
            for child in node.child_elements() {
                collect_html(child, blocks);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spaces_markdown_projects_blocks_and_skips_identical_hash_input() {
        let blocks = json!([
            {"id":"h","type":"heading","props":{"level":2},"content":[{"type":"text","text":"计划","styles":{}}],"children":[]},
            {"id":"p","type":"paragraph","props":{},"content":[{"type":"text","text":"唯一短语 space-rag-needle","styles":{"bold":true}}],"children":[]},
            {"id":"c","type":"codeBlock","props":{"language":"rust"},"content":[{"type":"text","text":"fn main() {}","styles":{}}],"children":[]},
            {"id":"t","type":"table","props":{},"content":{"type":"tableContent","rows":[{"cells":[[{"type":"text","text":"列","styles":{}}]]}]},"children":[]},
            {"id":"i","type":"image","props":{"url":"https://example.test/a.png","caption":"图"},"children":[]}
        ]);
        let markdown = project("blocknote_json", &blocks, "").unwrap();
        assert!(markdown.contains("## 计划"));
        assert!(markdown.contains("**唯一短语 space-rag-needle**"));
        assert!(markdown.contains("```rust"));
        assert!(markdown.contains("| 列 |"));
        assert!(markdown.contains("](https://example.test/a.png)"));
        assert_eq!(project("blocknote_json", &blocks, "ignored").unwrap(), markdown);
        assert!(backlink_targets("见 [[计划]] 和 [[计划]]").len() == 1);
    }

    #[test]
    fn spaces_markdown_round_trips_html_and_zip_pages() {
        let blocks = markdown_to_blocks("# 标题\n\n一段 [[页面]]\n\n- 子项\n");
        let html = blocks_to_html(&blocks);
        assert!(html.contains("<h1>标题</h1>"));
        let imported = html_to_blocks(&html).unwrap();
        assert!(blocks_to_markdown(&imported).contains("标题"));
        let diff = line_diff("旧\n", "新\n");
        assert!(diff.contains("- 旧"));
        assert!(diff.contains("+ 新"));
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("notes/alpha.md", options).unwrap();
            std::io::Write::write_all(&mut writer, b"# Alpha\n\nzip-phrase\n").unwrap();
            writer.finish().unwrap();
        }
        let pages = read_zip_pages(cursor.get_ref()).unwrap();
        assert_eq!(pages.len(), 1);
        assert!(pages[0].1.contains("alpha"));
        assert!(blocks_to_markdown(&pages[0].2).contains("zip-phrase"));
    }
}
