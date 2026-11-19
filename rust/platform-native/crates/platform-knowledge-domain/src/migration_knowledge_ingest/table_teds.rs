//! Ordered tree-edit TEDS over HTML table structure, spans and normalized cell
//! text. Unlike positional-cell TEDS-lite, insertion/deletion can promote children.
//! The bounded evaluator is offline; it rejects oversized inputs instead of sampling.
use super::{IngestError, Result};
use scraper::{ElementRef, Html, Selector};
use std::collections::BTreeMap;
#[derive(Clone)]
struct Node {
    tag: String,
    span: (usize, usize),
    text: String,
    left: usize,
}
fn fail() -> IngestError {
    IngestError::Validation("TEDS requires one bounded HTML table".into())
}
fn tree(html: &str) -> Result<Vec<Node>> {
    if html.len() > 64 * 1024 {
        return Err(fail());
    }
    let document = Html::parse_fragment(html);
    let selector = Selector::parse("table").map_err(|_| fail())?;
    let mut tables = document.select(&selector);
    let root = tables.next().ok_or_else(fail)?;
    if tables.next().is_some() {
        return Err(fail());
    }
    let mut nodes = vec![Node {
        tag: String::new(),
        span: (1, 1),
        text: String::new(),
        left: 0,
    }];
    fn visit(element: ElementRef<'_>, nodes: &mut Vec<Node>, depth: usize) -> Result<usize> {
        if depth > 32 || nodes.len() > 512 {
            return Err(fail());
        }
        let tag = element.value().name().to_owned();
        let cell = matches!(tag.as_str(), "td" | "th");
        let mut first = None;
        if !cell {
            for child in element.children().filter_map(ElementRef::wrap) {
                let index = visit(child, nodes, depth + 1)?;
                first.get_or_insert(nodes[index].left);
            }
        }
        let span = |key: &str| -> Result<usize> {
            match element.value().attr(key) {
                None => Ok(1),
                Some(s) => s
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0 && *n <= 1000)
                    .ok_or_else(fail),
            }
        };
        let text = if cell {
            element
                .text()
                .collect::<Vec<_>>()
                .join("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            String::new()
        };
        if text.chars().count() > 1024 {
            return Err(fail());
        }
        let index = nodes.len();
        nodes.push(Node {
            tag,
            span: (span("rowspan")?, span("colspan")?),
            text,
            left: first.unwrap_or(index),
        });
        Ok(index)
    }
    visit(root, &mut nodes, 0)?;
    if nodes.len() > 513 {
        return Err(fail());
    }
    Ok(nodes)
}
fn rename(a: &Node, b: &Node) -> f64 {
    if a.tag != b.tag || a.span != b.span {
        return 1.;
    }
    if !matches!(a.tag.as_str(), "td" | "th") || a.text == b.text {
        return 0.;
    }
    let chars: Vec<_> = b.text.chars().collect();
    let mut prior: Vec<_> = (0..=chars.len()).collect();
    for (i, x) in a.text.chars().enumerate() {
        let mut next = vec![i + 1];
        for (j, y) in chars.iter().enumerate() {
            next.push(
                (prior[j + 1] + 1)
                    .min(next[j] + 1)
                    .min(prior[j] + usize::from(x != *y)),
            );
        }
        prior = next;
    }
    prior[chars.len()] as f64 / a.text.chars().count().max(chars.len()).max(1) as f64
}
fn roots(nodes: &[Node]) -> Vec<usize> {
    let mut last = BTreeMap::new();
    for (i, n) in nodes.iter().enumerate().skip(1) {
        last.insert(n.left, i);
    }
    let mut roots: Vec<_> = last.into_values().collect();
    roots.sort();
    roots
}
pub fn score_html(expected: &str, actual: &str) -> Result<f64> {
    let a = tree(expected)?;
    let b = tree(actual)?;
    let n = a.len() - 1;
    let m = b.len() - 1;
    let mut distance = vec![vec![0f64; m + 1]; n + 1];
    // Zhang-Shasha forest recurrence. Keeping tree and forest matrices distinct is
    // necessary: deleting a row costs one node, not its entire subtree.
    for i in roots(&a) {
        for j in roots(&b) {
            let ai = a[i].left;
            let bj = b[j].left;
            let mut forest = vec![vec![0f64; j - bj + 2]; i - ai + 2];
            for x in ai..=i {
                forest[x - ai + 1][0] = forest[x - ai][0] + 1.;
            }
            for y in bj..=j {
                forest[0][y - bj + 1] = forest[0][y - bj] + 1.;
            }
            for x in ai..=i {
                for y in bj..=j {
                    let row = x - ai + 1;
                    let col = y - bj + 1;
                    let replacement = if a[x].left == ai && b[y].left == bj {
                        forest[row - 1][col - 1] + rename(&a[x], &b[y])
                    } else {
                        forest[a[x].left - ai][b[y].left - bj] + distance[x][y]
                    };
                    forest[row][col] = (forest[row - 1][col] + 1.)
                        .min(forest[row][col - 1] + 1.)
                        .min(replacement);
                    if a[x].left == ai && b[y].left == bj {
                        distance[x][y] = forest[row][col];
                    }
                }
            }
        }
    }
    Ok((1. - distance[n][m] / n.max(m) as f64).clamp(0., 1.))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_tree_metric_detects_structure_spans_and_text() {
        let a = "<table><tr><td>alpha</td><td>beta</td></tr></table>";
        assert_eq!(score_html(a, a).unwrap(), 1.);
        let b = "<table><tr><td colspan=2>alpha</td><td>beta</td></tr></table>";
        assert!(score_html(a, b).unwrap() < 1.);
        let c = "<table><tr><td>alpha</td></tr><tr><td>beta</td></tr></table>";
        assert!(score_html(a, c).unwrap() < 1.);
        assert_eq!(score_html(a, c).unwrap(), score_html(c, a).unwrap());
    }
    #[test]
    fn row_deletion_can_promote_its_children() {
        let a = tree("<table><tbody><tr><td>a</td></tr></tbody></table>").unwrap();
        let b = tree("<table><tbody><tr><td>b</td></tr></tbody></table>").unwrap();
        assert_eq!(a.len(), 5);
        assert_eq!(b.len(), 5);
        assert!(
            (score_html(
                "<table><tr><td>a</td></tr></table>",
                "<table><tr><td>b</td></tr></table>"
            )
            .unwrap()
                - 0.75)
                .abs()
                < 1e-9
        );
        assert!(score_html("missing table", "<table></table>").is_err());
    }
}
