//! Native storage-format text/Markdown projection, preserving source whitespace rules.
use ego_tree::iter::Edge;
use scraper::{Html, Node};
pub fn extract(content: &str, markdown: bool) -> String {
    let document = Html::parse_fragment(content);
    let mut out = String::new();
    let mut ignored = 0usize;
    let mut code = false;
    let mut lists: Vec<String> = vec![];
    let mut link = String::new();
    let mut table = false;
    let mut cells: Vec<String> = vec![];
    let mut macro_name = String::new();
    for edge in document.tree.root().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Text(t) => {
                    if ignored > 0 {
                        continue;
                    }
                    if code {
                        out.push_str(t);
                    } else {
                        let cleaned = t.split_whitespace().collect::<Vec<_>>().join(" ");
                        if table {
                            if !cleaned.is_empty() {
                                cells.push(cleaned);
                            }
                        } else {
                            out.push_str(&cleaned);
                        }
                    }
                }
                Node::Comment(c) if code => {
                    if let Some(c) = c.strip_prefix("[CDATA[").and_then(|s| s.strip_suffix("]]")) {
                        out.push_str(c);
                    }
                }
                Node::Element(e) => {
                    let tag = e.name();
                    if ["ac:placeholder", "ac:parameter", "ri:page"].contains(&tag) {
                        ignored += 1;
                        continue;
                    }
                    if ignored > 0 {
                        continue;
                    }
                    match tag {
                        "ac:structured-macro" => {
                            macro_name = e.attr("ac:name").unwrap_or("").into();
                            match macro_name.as_str() {
                                "toc" => ignored += 1,
                                "panel" => out.push('\n'),
                                "info" | "warning" | "note" | "tip" => {
                                    let mut label = macro_name.clone();
                                    label[0..1].make_ascii_uppercase();
                                    out.push_str(&if markdown {
                                        format!("\n> **{label}:** ")
                                    } else {
                                        format!("\n[{label}] ")
                                    });
                                }
                                _ => {}
                            }
                        }
                        "ac:plain-text-body" if macro_name == "code" => {
                            code = true;
                            out.push_str(if markdown { "\n```\n" } else { "\n" });
                        }
                        "ac:image" => {
                            if markdown {
                                out.push_str("\n[Image]");
                            }
                        }
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            out.push_str("\n\n");
                            if markdown {
                                out.push_str(
                                    &"#".repeat(tag.as_bytes()[1] as usize - b'0' as usize),
                                );
                                out.push(' ');
                            }
                        }
                        "p" | "table" => {
                            out.push_str("\n\n");
                            if tag == "table" {
                                table = true;
                            }
                        }
                        "br" => out.push('\n'),
                        "strong" | "b" if markdown => out.push_str("**"),
                        "em" | "i" if markdown => out.push('*'),
                        "code" if markdown && !code => out.push('`'),
                        "pre" => {
                            code = true;
                            out.push_str(if markdown { "\n```\n" } else { "\n" });
                        }
                        "ul" | "ol" => lists.push(tag.into()),
                        "li" => {
                            out.push('\n');
                            out.push_str(&"  ".repeat(lists.len().saturating_sub(1)));
                            out.push_str(if !markdown {
                                "* "
                            } else if lists.last().is_some_and(|s| s == "ol") {
                                "1. "
                            } else {
                                "- "
                            });
                        }
                        "a" => {
                            link = e.attr("href").unwrap_or("").into();
                            if markdown {
                                out.push('[');
                            }
                        }
                        "tr" => cells.clear(),
                        "blockquote" => out.push_str(if markdown { "\n> " } else { "\n" }),
                        "hr" => out.push_str(if markdown { "\n\n---\n\n" } else { "\n\n" }),
                        _ => {}
                    }
                }
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(e) = node.value() {
                    let tag = e.name();
                    if ["ac:placeholder", "ac:parameter", "ri:page"].contains(&tag) {
                        ignored = ignored.saturating_sub(1);
                        continue;
                    }
                    if tag == "ac:structured-macro" {
                        if macro_name == "toc" {
                            ignored = ignored.saturating_sub(1);
                        }
                        macro_name.clear();
                    }
                    if ignored > 0 {
                        continue;
                    }
                    match tag {
                        "strong" | "b" if markdown => out.push_str("**"),
                        "em" | "i" if markdown => out.push('*'),
                        "code" if markdown && !code => out.push('`'),
                        "pre" | "ac:plain-text-body" if code => {
                            code = false;
                            out.push_str(if markdown { "\n```\n" } else { "\n" });
                        }
                        "ul" | "ol" => {
                            lists.pop();
                            if lists.is_empty() {
                                out.push('\n');
                            }
                        }
                        "a" => {
                            if markdown && !link.is_empty() {
                                out.push_str(&format!("]({link})"));
                            }
                            link.clear();
                        }
                        "tr" => {
                            if !cells.is_empty() {
                                out.push_str(&format!("| {} |\n", cells.join(" | ")));
                            }
                            cells.clear();
                        }
                        "table" => table = false,
                        _ => {}
                    }
                }
            }
        }
    }
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.split('\n')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .into()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_text_and_markdown() {
        let html =
            "<h1>Title</h1><p>Hello <strong>world</strong></p><ul><li>One</li><li>Two</li></ul>";
        assert_eq!(extract(html, false), "Title\n\nHelloworld\n* One\n* Two");
        assert_eq!(
            extract(html, true),
            "# Title\n\nHello**world**\n- One\n- Two"
        );
    }
    #[test]
    fn source_macros() {
        assert_eq!(
            extract(
                "<ac:structured-macro ac:name=\"note\"><ac:rich-text-body><p>Remember</p></ac:rich-text-body></ac:structured-macro>",
                false
            ),
            "[Note]\n\nRemember"
        );
    }
}

/// Ordered image references with the five most recent ordinary text segments.
pub fn image_contexts(content: &str) -> Vec<(String, String, String)> {
    let document = Html::parse_fragment(content);
    let mut recent = std::collections::VecDeque::new();
    let mut image = false;
    let mut ignore = 0usize;
    let mut out = Vec::new();
    for edge in document.tree.root().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Text(text) if ignore == 0 => {
                    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !text.is_empty() {
                        recent.push_back(text);
                        if recent.len() > 5 {
                            recent.pop_front();
                        }
                    }
                }
                Node::Element(e) => match e.name() {
                    "ac:image" => image = true,
                    "ac:parameter" | "ac:placeholder" | "ac:plain-text-body" => ignore += 1,
                    "ri:attachment" if image => {
                        let filename = e.attr("ri:filename").unwrap_or("");
                        if !filename.is_empty() {
                            out.push((
                                e.attr("ri:content-id").unwrap_or("").into(),
                                filename.into(),
                                recent.iter().cloned().collect::<Vec<_>>().join(" "),
                            ));
                        }
                    }
                    _ => {}
                },
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(e) = node.value() {
                    match e.name() {
                        "ac:image" => image = false,
                        "ac:parameter" | "ac:placeholder" | "ac:plain-text-body" => {
                            ignore = ignore.saturating_sub(1)
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod image_tests {
    use super::*;
    #[test]
    fn references_preserve_order_and_recent_context() {
        let refs = image_contexts(
            "<p>One</p><p>Two</p><ac:image><ri:attachment ri:filename=\"first.png\" ri:content-id=\"42\"/></ac:image><p>Three</p><ac:image><ri:attachment ri:filename=\"second.png\"/></ac:image>",
        );
        assert_eq!(
            refs,
            vec![
                ("42".into(), "first.png".into(), "One Two".into()),
                ("".into(), "second.png".into(), "One Two Three".into())
            ]
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImageReference {
    pub filename: String,
    pub content_type: Option<String>,
    pub attachment_id: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub alt_text: Option<String>,
    pub title: Option<String>,
    pub context_text: String,
}
impl ImageReference {
    pub fn is_embeddable(&self) -> bool {
        if let Some(content_type) = self.content_type.as_ref().filter(|s| !s.is_empty()) {
            [
                "image/png",
                "image/jpeg",
                "image/jpg",
                "image/gif",
                "image/bmp",
                "image/webp",
            ]
            .contains(&content_type.to_lowercase().as_str())
        } else {
            self.filename.rsplit_once('.').is_some_and(|(_, ext)| {
                ["png", "jpg", "jpeg", "gif", "bmp", "webp"].contains(&ext.to_lowercase().as_str())
            })
        }
    }
}
/// Source utility metadata, including explicit content-type precedence and the
/// source's shared dimension conversion failure boundary.
pub fn extract_image_references(content: &str) -> Vec<ImageReference> {
    let document = Html::parse_fragment(content);
    let mut recent = std::collections::VecDeque::new();
    let mut attributes: Option<(String, String, Option<String>, Option<String>)> = None;
    let mut ignored = 0usize;
    let mut table = false;
    let mut code = false;
    let mut out = Vec::new();
    let optional = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(str::to_owned);
    for edge in document.tree.root().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Text(text) if ignored == 0 && !table && !code => {
                    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !clean.is_empty() {
                        recent.push_back(clean);
                        if recent.len() > 5 {
                            recent.pop_front();
                        }
                    }
                }
                Node::Element(e) => {
                    let tag = e.name();
                    if ["ac:placeholder", "ac:parameter", "ri:page"].contains(&tag) {
                        ignored += 1;
                        continue;
                    }
                    if ignored > 0 {
                        continue;
                    }
                    match tag {
                        "table" => table = true,
                        "pre" | "ac:plain-text-body" => code = true,
                        "ac:image" => {
                            attributes = Some((
                                e.attr("ac:width").unwrap_or("").into(),
                                e.attr("ac:height").unwrap_or("").into(),
                                optional(e.attr("ac:alt")),
                                optional(e.attr("ac:title")),
                            ))
                        }
                        "ri:attachment" => {
                            if let Some((w, h, alt, title)) = &attributes {
                                if let Some(filename) = optional(e.attr("ri:filename")) {
                                    let width = if w.is_empty() {
                                        None
                                    } else {
                                        w.trim().parse().ok()
                                    };
                                    let height =
                                        if (!w.is_empty() && width.is_none()) || h.is_empty() {
                                            None
                                        } else {
                                            h.trim().parse().ok()
                                        };
                                    out.push(ImageReference {
                                        filename,
                                        content_type: optional(e.attr("ri:content-type")),
                                        attachment_id: optional(e.attr("ri:content-id")),
                                        width,
                                        height,
                                        alt_text: alt.clone(),
                                        title: title.clone(),
                                        context_text: recent
                                            .iter()
                                            .cloned()
                                            .collect::<Vec<_>>()
                                            .join(" "),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(e) = node.value() {
                    match e.name() {
                        "ac:placeholder" | "ac:parameter" | "ri:page" => {
                            ignored = ignored.saturating_sub(1)
                        }
                        "ac:image" if ignored == 0 => attributes = None,
                        "table" if ignored == 0 => table = false,
                        "pre" | "ac:plain-text-body" if ignored == 0 => code = false,
                        _ => {}
                    }
                }
            }
        }
    }
    out
}
pub fn extract_embeddable_images(content: &str) -> Vec<ImageReference> {
    extract_image_references(content)
        .into_iter()
        .filter(ImageReference::is_embeddable)
        .collect()
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Heading {
    pub level: u8,
    pub text: String,
}
pub fn extract_headings(content: &str) -> Vec<Heading> {
    // Keep raw entity text, matching the source's regex rather than HTML decoding.
    let tags = regex::Regex::new(r"<[^>]+>").unwrap();
    let heading = regex::Regex::new(r"(?is)<h([1-6])[^>]*>(.*?)</h([1-6])>").unwrap();
    heading
        .captures_iter(content)
        .filter_map(|m| {
            if m[1] != m[3] {
                return None;
            }
            let text = tags
                .replace_all(&m[2], "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            (!text.is_empty()).then(|| Heading {
                level: m[1].parse().unwrap(),
                text,
            })
        })
        .collect()
}
pub fn estimate_reading_time(
    content: &str,
    words_per_minute: usize,
) -> Result<usize, &'static str> {
    if words_per_minute == 0 {
        return Err("words_per_minute must not be zero");
    }
    let text = extract(content, false);
    let english = regex::Regex::new("[a-zA-Z]+")
        .unwrap()
        .find_iter(&text)
        .count();
    let chinese = text
        .chars()
        .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
        .count();
    Ok(((english + chinese) / words_per_minute).max(1))
}
#[cfg(test)]
mod utility_tests {
    use super::*;
    #[test]
    fn image_metadata_and_type_precedence() {
        let images = extract_image_references(
            r#"<p>Context</p><ac:image ac:width="bad" ac:height="12" ac:alt="Alt"><ri:attachment ri:filename="a.PNG" ri:content-type="image/svg+xml"/></ac:image>"#,
        );
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].height, None);
        assert_eq!(images[0].context_text, "Context");
        assert_eq!(images[0].alt_text.as_deref(), Some("Alt"));
        assert!(!images[0].is_embeddable());
        assert_eq!(
            extract_embeddable_images("<ac:image><ri:attachment ri:filename='a.PNG'/></ac:image>")
                .len(),
            1
        );
    }
    #[test]
    fn headings_and_reading_use_source_text_rules() {
        assert_eq!(
            extract_headings("<H2> A <b>B</b> &amp; C </H2>"),
            vec![Heading {
                level: 2,
                text: "A B &amp; C".into()
            }]
        );
        assert_eq!(estimate_reading_time("<p>one two 中文</p>", 2), Ok(2));
        assert!(estimate_reading_time("", 0).is_err());
    }
}
