//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt
//! library semantics, checked against the JVM-generated priority-queue table.

use std::cell::RefCell;

use jcompat::JavaPriorityQueue;
use serde_json::{Value, json};

#[derive(Clone)]
struct Item {
    id: i64,
    key: i64,
    eq: i64,
}

impl PartialEq for Item {
    fn eq(&self, other: &Self) -> bool {
        self.eq == other.eq
    }
}

fn item(value: &Value) -> Item {
    Item {
        id: value[0].as_i64().unwrap(),
        key: value[1].as_i64().unwrap(),
        eq: value[2].as_i64().unwrap(),
    }
}

#[test]
fn priority_queue_oracle_results_arrays_and_comparator_calls() {
    let table = include_str!("../../../fixtures/oracle/jcompat/priority_queue.jsonl");
    let mut lines = table.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut rows = 0;
    let mut operations = 0;
    let mut comparisons = 0;
    for line in lines {
        let row: Value = serde_json::from_str(line).unwrap();
        let trace = RefCell::new(Vec::new());
        let mut queue = JavaPriorityQueue::new(|a: &Item, b: &Item| {
            let order = a.key.cmp(&b.key);
            let sign = match order {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            trace.borrow_mut().push(json!([a.id, b.id, sign]));
            order
        });
        for (index, step) in row["steps"].as_array().unwrap().iter().enumerate() {
            trace.borrow_mut().clear();
            let args = step["args"].as_array().unwrap();
            let result = match step["op"].as_str().unwrap() {
                "offer" => json!(queue.offer(item(&args[0]))),
                "addAll" => json!(queue.add_all(args.iter().map(item))),
                "poll" => json!(queue.poll().map(|item| item.id)),
                "peek" => json!(queue.peek().map(|item| item.id)),
                "remove" => json!(queue.remove(&item(&args[0]))),
                "removeAll" => {
                    json!(queue.remove_all(&args.iter().map(item).collect::<Vec<_>>()))
                }
                "iterator" => json!(queue.iter().map(|item| item.id).collect::<Vec<_>>()),
                "clear" => {
                    queue.clear();
                    Value::Null
                }
                op => panic!("unhandled oracle operation {op}"),
            };
            let context = format!("{} step {index} {}", row["id"], step["op"]);
            assert_eq!(result, step["result"], "result: {context}");
            assert_eq!(json!(*trace.borrow()), step["cmp"], "trace: {context}");
            let heap = queue.iter().map(|item| item.id).collect::<Vec<_>>();
            assert_eq!(json!(heap), step["heap"], "heap: {context}");
            assert_eq!(queue.len(), heap.len(), "size: {context}");
            assert_eq!(queue.is_empty(), heap.is_empty(), "emptiness: {context}");
            operations += 1;
            comparisons += trace.borrow().len();
        }
        rows += 1;
    }
    assert_eq!(rows, header["rows"].as_u64().unwrap());
    assert_eq!(rows, 240);
    assert_eq!(operations, 5824);
    assert_eq!(comparisons, 7104);
}
