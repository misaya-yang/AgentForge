//! Remaining base jsonpath-ng grammar and contextual values. No extended-parser
//! filters or operators are invented: the source uses jsonpath_ng.parse.
use super::{
    Parser, RestMappingError as Error, Selector, contains_opaque, python_truthy, slice_indices,
};
use serde_json::Value;
use std::{borrow::Cow, rc::Rc};
type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
enum Expr {
    Root,
    This,
    Parent,
    Select(Selector),
    Child(Box<Self>, Box<Self>),
    Descendants(Box<Self>, Box<Self>),
    Union(Box<Self>, Box<Self>),
    Intersect,
    Where(Box<Self>, Box<Self>, bool),
}
struct Grammar<'a> {
    parser: Parser<'a>,
}
impl<'a> Grammar<'a> {
    fn atom(&mut self) -> Result<Expr> {
        self.parser.space();
        if self.parser.take('$') {
            return Ok(Expr::Root);
        }
        if self.parser.take('(') {
            let result = self.expression(0)?;
            if !self.parser.take(')') {
                return Err(Error::UnsupportedJsonPath);
            }
            return Ok(result);
        }
        if self.parser.take('[') {
            return Ok(Expr::Select(self.parser.bracket()?));
        }
        if self.parser.take('`') {
            let mut value = String::new();
            let mut escape = false;
            loop {
                let c = self
                    .parser
                    .text
                    .chars()
                    .next()
                    .ok_or(Error::UnsupportedJsonPath)?;
                self.parser.text = &self.parser.text[c.len_utf8()..];
                if escape {
                    value.push(c);
                    escape = false;
                } else if c == '\\' {
                    escape = true;
                } else if c == '`' {
                    break;
                } else {
                    value.push(c);
                }
            }
            return match value.as_str() {
                "this" => Ok(Expr::This),
                "parent" => Ok(Expr::Parent),
                _ => Err(Error::UnsupportedJsonPath),
            };
        }
        if self.parser.text.starts_with('-')
            || self.parser.text.starts_with(|c: char| c.is_ascii_digit())
        {
            let count = self
                .parser
                .text
                .bytes()
                .enumerate()
                .take_while(|(i, b)| b.is_ascii_digit() || (*i == 0 && *b == b'-'))
                .count();
            let token = &self.parser.text[..count];
            if token == "-" {
                return Err(Error::UnsupportedJsonPath);
            }
            let digits = token.trim_start_matches('-').trim_start_matches('0');
            let number = if digits.is_empty() {
                "0".to_owned()
            } else if token.starts_with('-') {
                format!("-{digits}")
            } else {
                digits.to_owned()
            };
            self.parser.text = &self.parser.text[count..];
            return Ok(Expr::Select(Selector::Fields(vec![number])));
        }
        Ok(Expr::Select(Selector::Fields(self.parser.fields()?)))
    }
    fn expression(&mut self, min: u8) -> Result<Expr> {
        let mut left = self.atom()?;
        loop {
            self.parser.space();
            if self.parser.text.starts_with('[') {
                if min > 0 {
                    break;
                }
                self.parser.take('[');
                left = Expr::Child(
                    Box::new(left),
                    Box::new(Expr::Select(self.parser.bracket()?)),
                );
                continue;
            }
            let mut operation = None;
            for (token, precedence) in [
                ("..", 2),
                (".", 3),
                ("|", 4),
                ("&", 5),
                ("wherenot", 7),
                ("where", 6),
            ] {
                if let Some(rest) = self.parser.text.strip_prefix(token) {
                    if token.starts_with('w')
                        && rest
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    {
                        continue;
                    }
                    operation = Some((token, precedence));
                    break;
                }
            }
            let Some((token, precedence)) = operation else {
                break;
            };
            if precedence < min {
                break;
            }
            self.parser.text = &self.parser.text[token.len()..];
            let right = self.expression(precedence + 1)?;
            left = match token {
                "." => Expr::Child(Box::new(left), Box::new(right)),
                ".." => Expr::Descendants(Box::new(left), Box::new(right)),
                "|" => Expr::Union(Box::new(left), Box::new(right)),
                "&" => Expr::Intersect,
                "where" => Expr::Where(Box::new(left), Box::new(right), false),
                "wherenot" => Expr::Where(Box::new(left), Box::new(right), true),
                _ => unreachable!(),
            };
        }
        Ok(left)
    }
}
#[derive(Clone)]
struct Node<'a> {
    value: Cow<'a, Value>,
    parent: Option<Rc<Node<'a>>>,
    wrapped: Option<Rc<Node<'a>>>,
    missing_context: bool,
}
impl<'a> Node<'a> {
    fn absent() -> Rc<Self> {
        Rc::new(Self {
            value: Cow::Owned(Value::Null),
            parent: None,
            wrapped: None,
            missing_context: true,
        })
    }
    fn normalize(node: Rc<Self>) -> Rc<Self> {
        if node.missing_context {
            Rc::new(Self {
                value: Cow::Owned(Value::Null),
                parent: None,
                wrapped: None,
                missing_context: false,
            })
        } else {
            node
        }
    }
    fn derived(parent: &Rc<Self>, value: Cow<'a, Value>) -> Rc<Self> {
        Rc::new(Self {
            value,
            parent: Some(parent.clone()),
            wrapped: None,
            missing_context: false,
        })
    }
    fn field(parent: &Rc<Self>, field: &str) -> Option<Rc<Self>> {
        match &parent.value {
            Cow::Borrowed(v) => v
                .get(field)
                .map(|v| Self::derived(parent, Cow::Borrowed(v))),
            Cow::Owned(v) => v
                .get(field)
                .map(|v| Self::derived(parent, Cow::Owned(v.clone()))),
        }
    }
    fn index(parent: &Rc<Self>, index: usize) -> Option<Rc<Self>> {
        if let Some(original) = &parent.wrapped {
            if index == 0 {
                return Some(Self::derived(parent, original.value.clone()));
            }
            return None;
        }
        match &parent.value {
            Cow::Borrowed(v) => v
                .get(index)
                .map(|v| Self::derived(parent, Cow::Borrowed(v))),
            Cow::Owned(v) => v
                .get(index)
                .map(|v| Self::derived(parent, Cow::Owned(v.clone()))),
        }
    }
    fn children(parent: &Rc<Self>) -> Vec<Rc<Self>> {
        match parent.value.as_ref() {
            Value::Array(a) => (0..a.len())
                .filter_map(|i| Self::index(parent, i))
                .collect(),
            Value::Object(o) => o
                .keys()
                .filter_map(|key| Self::field(parent, key))
                .collect(),
            _ => Vec::new(),
        }
    }
    fn check(&self, opaque: &[&Value]) -> Result<()> {
        if let Cow::Borrowed(value) = &self.value {
            if opaque.iter().any(|v| std::ptr::eq(*v, *value)) {
                return Err(Error::NonJsonRequestValue);
            }
        }
        Ok(())
    }
    fn contains_opaque(&self, opaque: &[&Value]) -> bool {
        if let Some(original) = &self.wrapped {
            return original.contains_opaque(opaque);
        }
        match &self.value {
            Cow::Borrowed(value) => contains_opaque(value, opaque),
            Cow::Owned(_) => false,
        }
    }
}
impl Expr {
    fn find<'a>(&self, node: Rc<Node<'a>>, opaque: &[&Value]) -> Result<Vec<Rc<Node<'a>>>> {
        node.check(opaque)?;
        match self {
            Self::Root => {
                let mut root = Node::normalize(node);
                while let Some(parent) = &root.parent {
                    root = parent.clone();
                }
                Ok(vec![root])
            }
            Self::This => Ok(vec![Node::normalize(node)]),
            Self::Parent => Ok(vec![node.parent.clone().unwrap_or_else(Node::absent)]),
            Self::Intersect => Err(Error::JsonPathEvaluation),
            Self::Union(left, right) => {
                let mut values = left.find(node.clone(), opaque)?;
                values.extend(right.find(node, opaque)?);
                Ok(values)
            }
            Self::Child(left, right) => {
                let mut result = Vec::new();
                for sub in left.find(node, opaque)? {
                    result.extend(right.find(sub, opaque)?);
                }
                Ok(result)
            }
            Self::Where(left, right, negative) => {
                let mut result = Vec::new();
                for sub in left.find(node, opaque)? {
                    if right.find(sub.clone(), opaque)?.is_empty() == *negative {
                        result.push(sub);
                    }
                }
                Ok(result)
            }
            Self::Descendants(left, right) => {
                let mut result = Vec::new();
                for sub in left.find(node, opaque)? {
                    let mut pending = vec![sub];
                    while let Some(next) = pending.pop() {
                        if next.missing_context {
                            return Err(Error::JsonPathEvaluation);
                        }
                        result.extend(right.find(next.clone(), opaque)?);
                        pending.extend(Node::children(&next).into_iter().rev());
                    }
                }
                Ok(result)
            }
            Self::Select(selector) => match selector {
                Selector::Fields(fields) => {
                    if let Value::Object(object) = node.value.as_ref() {
                        let keys = if fields.iter().any(|s| s == "*") {
                            object.keys().cloned().collect()
                        } else {
                            fields.clone()
                        };
                        Ok(keys
                            .iter()
                            .filter_map(|key| Node::field(&node, key))
                            .collect())
                    } else {
                        Ok(Vec::new())
                    }
                }
                Selector::Indices(indices) => {
                    if !python_truthy(&node.value) {
                        return Ok(Vec::new());
                    }
                    let length = match node.value.as_ref() {
                        Value::Array(v) => v.len(),
                        Value::Object(v) => v.len(),
                        Value::String(v) => v.chars().count(),
                        _ => return Err(Error::JsonPathEvaluation),
                    };
                    let mut result = Vec::new();
                    for index in indices {
                        if length as i128 <= *index as i128 {
                            continue;
                        }
                        let actual = if *index < 0 {
                            length as i128 + *index as i128
                        } else {
                            *index as i128
                        };
                        if actual < 0 {
                            return Err(Error::JsonPathEvaluation);
                        }
                        let index =
                            usize::try_from(actual).map_err(|_| Error::JsonPathEvaluation)?;
                        match node.value.as_ref() {
                            Value::Array(_) => {
                                if let Some(child) = Node::index(&node, index) {
                                    result.push(child);
                                }
                            }
                            Value::String(s) => {
                                let c = s.chars().nth(index).ok_or(Error::JsonPathEvaluation)?;
                                result.push(Node::derived(
                                    &node,
                                    Cow::Owned(Value::String(c.to_string())),
                                ));
                            }
                            _ => return Err(Error::JsonPathEvaluation),
                        }
                    }
                    Ok(result)
                }
                Selector::Slice(start, end, step) => {
                    if node.value.is_null() {
                        return Ok(Vec::new());
                    }
                    if *step == 0 {
                        return Err(Error::JsonPathEvaluation);
                    }
                    let node = if node.value.is_array() {
                        node
                    } else {
                        Rc::new(Node {
                            value: Cow::Owned(Value::Array(vec![node.value.as_ref().clone()])),
                            parent: node.parent.clone(),
                            wrapped: Some(node),
                            missing_context: false,
                        })
                    };
                    let length = node
                        .value
                        .as_array()
                        .ok_or(Error::JsonPathEvaluation)?
                        .len();
                    Ok(slice_indices(length, *start, *end, *step)
                        .filter_map(|i| Node::index(&node, i))
                        .collect())
                }
                Selector::Descendants(_) => {
                    unreachable!("grammar builds explicit descendant expressions")
                }
            },
        }
    }
}
pub(super) fn evaluate(path: &str, data: &Value, opaque: &[&Value]) -> Result<Value> {
    let mut grammar = Grammar {
        parser: Parser::new(path),
    };
    let expression = grammar.expression(0)?;
    grammar.parser.space();
    if !grammar.parser.text.is_empty() {
        return Err(Error::UnsupportedJsonPath);
    }
    let node = Rc::new(Node {
        value: Cow::Borrowed(data),
        parent: None,
        wrapped: None,
        missing_context: false,
    });
    let results = expression.find(node, opaque)?;
    if results.iter().any(|n| n.missing_context) {
        return Err(Error::JsonPathEvaluation);
    }
    if results.iter().any(|n| n.contains_opaque(opaque)) {
        return Err(Error::NonJsonRequestValue);
    }
    Ok(match results.len() {
        0 => Value::Null,
        1 => results[0].value.as_ref().clone(),
        _ => Value::Array(results.iter().map(|n| n.value.as_ref().clone()).collect()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contextual_paths_preserve_opaque_request_provenance() {
        let data = serde_json::json!({"a":{"b":1},"opaque":{"x":2}});
        let opaque = [&data["opaque"]];
        assert_eq!(
            evaluate("$.(a | missing).b", &data, &opaque).unwrap(),
            serde_json::json!(1)
        );
        assert_eq!(
            evaluate("$.a[*].`parent`[0].b", &data, &opaque).unwrap(),
            serde_json::json!(1)
        );
        for path in [
            "$.a.`parent`",
            "$.opaque.`this`",
            "$.a[*].`parent`[0].`parent`.`parent`",
        ] {
            assert_eq!(
                evaluate(path, &data, &opaque),
                Err(Error::NonJsonRequestValue),
                "{path}"
            );
        }
    }
}
