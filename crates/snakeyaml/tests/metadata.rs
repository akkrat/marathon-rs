use snakeyaml::{node, tokens};
#[test]
fn collection_tokens_keep_event_spans() {
    let stream = node::load("&a [1, 2]");
    let toks = tokens::tokens(&stream);
    assert_eq!((toks.tokens[0].span.start.index, toks.tokens[0].span.end.index), (0, 4));
    assert_eq!((toks.tokens[3].span.start.index, toks.tokens[3].span.end.index), (8, 9));
}
#[test]
fn incomplete_collection_spans_contain_consumed_children() {
    use node::NodeKind;
    let stream = node::load("k: &a [one, {b: c\n");
    assert!(stream.error.is_some());
    let mut pending = vec![&stream.documents[0].root];
    while let Some(parent) = pending.pop() {
        let children: Vec<_> = match &parent.kind {
            NodeKind::Seq { items, complete } => {
                assert!(!complete);
                assert!(parent.closing.is_none());
                items.iter().collect()
            }
            NodeKind::Map { entries, complete } => {
                assert!(!complete);
                assert!(parent.closing.is_none());
                entries.iter().flat_map(|(k, v)| [k, v]).collect()
            }
            _ => continue,
        };
        assert_eq!(parent.span.end.index, 17);
        for child in children {
            if !matches!(child.kind, NodeKind::Cut) {
                assert!(parent.span.start.index <= child.span.start.index);
                assert!(parent.span.end.index >= child.span.end.index);
                pending.push(child);
            }
        }
    }
}
