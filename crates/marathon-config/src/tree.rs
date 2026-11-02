//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/ExecutionTimeSortingStrategyConfigurationDeserializer.kt
//! Jackson readTree materializes numbers and collapses duplicate object properties.
use crate::{
    binder::{Binder, JSON_MAPPING, Result, Value},
    binding::PathPart,
};
use jcompat::writers::JavaString;
use snakeyaml::{
    node::{Node, NodeKind, Style},
    tokens::{self, Tok},
};

pub struct Tree<'a> {
    binder: &'a Binder<'a>,
    root: Node,
    path: Vec<PathPart>,
}
impl<'a> Tree<'a> {
    pub(crate) fn new(value: Value<'a>) -> Result<Self> {
        enum Work<'n> {
            Visit(&'n Node),
            Finish(&'n Node, usize),
        }
        let mut work = vec![Work::Visit(value.node)];
        let mut built: Vec<Node> = Vec::new();
        while let Some(item) = work.pop() {
            match item {
                Work::Visit(node) => match &node.kind {
                    NodeKind::Seq { items, .. } => {
                        work.push(Work::Finish(node, items.len()));
                        for child in items.iter().rev() {
                            work.push(Work::Visit(child));
                        }
                    }
                    NodeKind::Map { entries, .. } => {
                        let visible = entries
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| !std::ptr::eq(node, value.node) || value.hidden_type != Some(*index));
                        work.push(Work::Finish(node, visible.clone().count()));
                        for (_, (_, child)) in visible.rev() {
                            work.push(Work::Visit(child));
                        }
                    }
                    _ => {
                        let token = tokens::subtree(node, None)
                            .tokens
                            .into_iter()
                            .next()
                            .ok_or_else(|| value.error(JSON_MAPPING, "missing tree token"))?;
                        let (text, style, tag) = match token.tok {
                            Tok::Str(s) => (s, Style::DoubleQuoted, None),
                            Tok::Int { value: Ok(v), .. } => (v.to_decimal().into(), Style::Plain, None),
                            Tok::Float { cleaned, .. } => {
                                let number = jcompat::jnum::parse_double(&cleaned)
                                    .ok_or_else(|| value.error(JSON_MAPPING, "invalid tree number"))?;
                                (
                                    jcompat::jnum::double_to_string(number).into(),
                                    Style::Plain,
                                    Some("tag:yaml.org,2002:float".into()),
                                )
                            }
                            Tok::True(_) => ("true".into(), Style::Plain, None),
                            Tok::False(_) => ("false".into(), Style::Plain, None),
                            Tok::Null(_) => ("null".into(), Style::Plain, None),
                            Tok::Embedded(b) => (
                                snakeyaml::resolve::base64_encode(&b).into(),
                                Style::Plain,
                                Some("tag:yaml.org,2002:binary".into()),
                            ),
                            _ => return Err(value.error(JSON_MAPPING, "invalid tree scalar")),
                        };
                        built.push(Node {
                            kind: NodeKind::Scalar { text, style },
                            anchor: None,
                            tag,
                            span: node.span,
                            opening: node.opening,
                            closing: None,
                        });
                    }
                },
                Work::Finish(node, count) => {
                    let children = built.split_off(built.len() - count);
                    let kind = match &node.kind {
                        NodeKind::Seq { .. } => NodeKind::Seq {
                            items: children,
                            complete: true,
                        },
                        NodeKind::Map { entries, .. } => {
                            let mut result: Vec<(Node, Node)> = Vec::new();
                            let visible = entries.iter().enumerate().filter(|(index, _)| {
                                !std::ptr::eq(node, value.node) || value.hidden_type != Some(*index)
                            });
                            for ((_, (key, _)), child) in visible.zip(children) {
                                let text = tokens::check_key(key)
                                    .map_err(|_| value.error(JSON_MAPPING, "invalid tree key"))?;
                                text.as_utf8()
                                    .map_err(|_| value.error(JSON_MAPPING, "D9: tree keys must be UTF-8"))?;
                                if let Some((_, old)) = result
                                    .iter_mut()
                                    .find(|(k, _)| matches!(&k.kind,NodeKind::Scalar {text:t,..} if t==text))
                                {
                                    *old = child;
                                } else {
                                    result.push((scalar_key(text, key), child));
                                }
                            }
                            NodeKind::Map {
                                entries: result,
                                complete: true,
                            }
                        }
                        _ => return Err(value.error(JSON_MAPPING, "expected tree container")),
                    };
                    built.push(Node {
                        kind,
                        anchor: None,
                        tag: None,
                        span: node.span,
                        opening: node.opening,
                        closing: node.closing,
                    });
                }
            }
        }
        let root = built.pop().ok_or_else(|| value.error(JSON_MAPPING, "empty tree"))?;
        Ok(Self {
            binder: value.binder,
            root,
            path: value.path,
        })
    }
    pub fn value(&self) -> Value<'_> {
        Value {
            binder: self.binder,
            node: &self.root,
            path: self.path.clone(),
            hidden_type: None,
            wrapper_end: false,
            parent: None,
        }
    }
    pub fn get(&self, name: &str) -> Result<Option<Value<'_>>> {
        self.value().get(name)
    }
    pub fn find_value(&self, name: &str) -> Result<Option<Value<'_>>> {
        self.value().find_value(name)
    }
    pub fn bind<T>(&self, bind: impl FnOnce(Value<'_>) -> Result<T>) -> Result<T> {
        bind(self.value())
    }
}
fn scalar_key(text: &JavaString, source: &Node) -> Node {
    Node {
        kind: NodeKind::Scalar {
            text: text.clone(),
            style: Style::DoubleQuoted,
        },
        anchor: None,
        tag: None,
        span: source.span,
        opening: source.opening,
        closing: None,
    }
}

// These are JsonNode conversions, separate from strict treeToValue/field binding.
// read_tree validates tokens before exposing them; containers use JsonNode defaults.
impl Value<'_> {
    fn tree_token(&self) -> Option<Tok> {
        if matches!(self.node.kind, NodeKind::Map { .. } | NodeKind::Seq { .. }) {
            None
        } else {
            self.token().ok().map(|t| t.tok)
        }
    }
    /// JsonNode.asText: null is "null", containers are empty, binary is base64.
    pub fn as_text(&self) -> JavaString {
        use snakeyaml::resolve::base64_encode;
        match self.tree_token() {
            Some(Tok::Str(s)) => s,
            Some(Tok::Int { value: Ok(n), .. }) => n.to_decimal().into(),
            Some(Tok::Float { cleaned, .. }) => {
                jcompat::jnum::double_to_string(jcompat::jnum::parse_double(&cleaned).unwrap_or(0.0)).into()
            }
            Some(Tok::True(_)) => "true".into(),
            Some(Tok::False(_)) => "false".into(),
            Some(Tok::Null(_)) => "null".into(),
            Some(Tok::Embedded(bytes)) => base64_encode(&bytes).into(),
            _ => "".into(),
        }
    }
    /// JsonNode.asInt: booleans become 0/1, text parses leniently, all other
    /// non-numeric nodes become zero. Numeric integers narrow like Java.
    pub fn as_int(&self) -> i32 {
        match self.tree_token() {
            Some(Tok::Int { value: Ok(n), .. }) => jcompat::jnum::bigint_long_value(&n.to_decimal()) as i32,
            Some(Tok::Float { cleaned, .. }) => jcompat::jnum::parse_double(&cleaned).unwrap_or(0.0) as i32,
            Some(Tok::Str(s)) => jcompat::jnum::jackson_parse_as_int(&s.to_string_lossy()),
            Some(Tok::True(_)) => 1,
            _ => 0,
        }
    }
    /// JsonNode.asLong: like asInt with Java long narrowing/saturation.
    pub fn as_long(&self) -> i64 {
        match self.tree_token() {
            Some(Tok::Int { value: Ok(n), .. }) => jcompat::jnum::bigint_long_value(&n.to_decimal()),
            Some(Tok::Float { cleaned, .. }) => jcompat::jnum::parse_double(&cleaned).unwrap_or(0.0) as i64,
            Some(Tok::Str(s)) => jcompat::jnum::jackson_parse_as_long(&s.to_string_lossy()),
            Some(Tok::True(_)) => 1,
            _ => 0,
        }
    }
    /// JsonNode.asDouble: invalid text/containers/null/binary become zero.
    pub fn as_double(&self) -> f64 {
        match self.tree_token() {
            Some(Tok::Int { value: Ok(n), .. }) => jcompat::jnum::parse_double(&n.to_decimal()).unwrap_or(0.0),
            Some(Tok::Float { cleaned, .. }) => jcompat::jnum::parse_double(&cleaned).unwrap_or(0.0),
            Some(Tok::Str(s)) => jcompat::jnum::parse_double(&s.to_string_lossy()).unwrap_or(0.0),
            Some(Tok::True(_)) => 1.0,
            _ => 0.0,
        }
    }
    /// Jackson `JsonNode.toString()`: scalars render exactly (strings quoted
    /// and escaped); containers render as compact JSON (WP 2.2 S3, which for
    /// these trees matches Jackson's own compact form).
    pub fn node_string(&self) -> String {
        enum Work<'n> {
            Node(&'n Node),
            Text(&'static str),
            Key(String),
        }
        let mut out = String::new();
        let mut work = vec![Work::Node(self.node)];
        while let Some(item) = work.pop() {
            match item {
                Work::Text(s) => out.push_str(s),
                Work::Key(k) => {
                    escape_json_string(&k, &mut out);
                    out.push(':');
                }
                Work::Node(node) => match &node.kind {
                    NodeKind::Map { entries, .. } => {
                        work.push(Work::Text("}"));
                        for (i, (key, child)) in entries.iter().enumerate().rev() {
                            work.push(Work::Node(child));
                            let text = tokens::check_key(key).map(|s| s.to_string_lossy()).unwrap_or_default();
                            work.push(Work::Key(text));
                            if i > 0 {
                                work.push(Work::Text(","));
                            }
                        }
                        work.push(Work::Text("{"));
                    }
                    NodeKind::Seq { items, .. } => {
                        work.push(Work::Text("]"));
                        for (i, child) in items.iter().enumerate().rev() {
                            work.push(Work::Node(child));
                            if i > 0 {
                                work.push(Work::Text(","));
                            }
                        }
                        work.push(Work::Text("["));
                    }
                    _ => {
                        let value = Value {
                            binder: self.binder,
                            node,
                            path: Vec::new(),
                            hidden_type: None,
                            wrapper_end: false,
                            parent: None,
                        };
                        match value.tree_token() {
                            Some(Tok::Str(s)) => escape_json_string(&s.to_string_lossy(), &mut out),
                            Some(Tok::Int { value: Ok(n), .. }) => out.push_str(&n.to_decimal()),
                            Some(Tok::Float { cleaned, .. }) => out.push_str(&jcompat::jnum::double_to_string(
                                jcompat::jnum::parse_double(&cleaned).unwrap_or(0.0),
                            )),
                            Some(Tok::True(_)) => out.push_str("true"),
                            Some(Tok::False(_)) => out.push_str("false"),
                            Some(Tok::Embedded(b)) => {
                                escape_json_string(&snakeyaml::resolve::base64_encode(&b), &mut out)
                            }
                            _ => out.push_str("null"),
                        }
                    }
                },
            }
        }
        out
    }
    /// JsonNode.asBoolean: nonzero integers and lowercase trimmed "true" are
    /// true. Floating-point nodes and other non-boolean nodes return false.
    pub fn as_boolean(&self) -> bool {
        match self.tree_token() {
            Some(Tok::True(_)) => true,
            Some(Tok::Int { value: Ok(n), .. }) => !matches!(n.to_decimal().as_str(), "0" | "-0"),
            Some(Tok::Str(s)) => snakeyaml::resolve::java_trim(&s.to_string_lossy()) == "true",
            _ => false,
        }
    }
}

fn escape_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            // Jackson's JsonGenerator writes control escapes with UPPERCASE hex.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
