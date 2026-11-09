//! Native differential helper using the production YAML parser and resolver.
use serde_json::{Value, json};
use snakeyaml::{
    node::{self, Node, NodeKind, Style},
    resolve::{Implicit, resolve_implicit},
};
use std::{
    collections::HashMap,
    error::Error,
    io::{self, Read, Write},
};

fn resolved(text: &str) -> &'static str {
    match resolve_implicit(text) {
        Implicit::Bool => "bool",
        Implicit::Int => "int",
        Implicit::Float => "float",
        Implicit::Null => "null",
        Implicit::Str => "str",
    }
}

fn tree(node: &Node, anchors: &mut HashMap<String, Value>) -> Result<Value, Box<dyn Error>> {
    let tag = node.tag.as_deref();
    let base = "tag:yaml.org,2002:";
    let value = match &node.kind {
        NodeKind::Alias { name } => {
            return anchors
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unknown or recursive alias: {name}").into());
        }
        NodeKind::Cut => return Err("incomplete YAML node".into()),
        NodeKind::Seq { items, complete } => {
            if !complete {
                return Err("incomplete YAML sequence".into());
            }
            if tag.is_some_and(|t| t.starts_with(base) && t != "tag:yaml.org,2002:seq") {
                return Err("invalid sequence tag".into());
            }
            let values = items
                .iter()
                .map(|item| tree(item, anchors))
                .collect::<Result<Vec<_>, _>>()?;
            json!(["seq", tag.filter(|t| *t != "tag:yaml.org,2002:seq"), values])
        }
        NodeKind::Map { entries, complete } => {
            if !complete {
                return Err("incomplete YAML mapping".into());
            }
            if tag.is_some_and(|t| t.starts_with(base) && t != "tag:yaml.org,2002:map") {
                return Err("invalid mapping tag".into());
            }
            let mut values = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                values.push([tree(key, anchors)?, tree(value, anchors)?]);
            }
            json!(["map", tag.filter(|t| *t != "tag:yaml.org,2002:map"), values])
        }
        NodeKind::Scalar { text, style } => {
            if matches!(tag, Some("tag:yaml.org,2002:map" | "tag:yaml.org,2002:seq")) {
                return Err("invalid scalar tag".into());
            }
            let text = text.clone();
            let style = match style {
                Style::Plain => "plain",
                Style::SingleQuoted => "single-quoted",
                Style::DoubleQuoted => "double-quoted",
                Style::Literal => "literal",
                Style::Folded => "folded",
            };
            json!(["scalar-source", tag, style, text, resolved(&text)])
        }
    };
    if let Some(name) = &node.anchor {
        anchors.insert(name.clone(), value.clone());
    }
    Ok(value)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let output = if args == ["--classify"] {
        json!(resolved(&input))
    } else if args.is_empty() {
        let stream = node::load(&input);
        if let Some(error) = stream.error {
            return Err(format!("invalid YAML: {}", error.message).into());
        }
        stream
            .documents
            .iter()
            .map(|doc| tree(&doc.root, &mut HashMap::new()))
            .collect::<Result<Vec<_>, _>>()?
            .into()
    } else {
        return Err("usage: yaml_tree [--classify]".into());
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, &output)?;
    writeln!(out)?;
    Ok(())
}
