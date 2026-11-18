use super::OrderedQueue;
use crate::model::Test;
use std::{collections::BTreeMap, hint::black_box, time::Instant};

#[test]
#[ignore = "local timing harness"]
fn compare_scan_and_indexed_class_batches() {
    for size in [10_000, 50_000] {
        let input: Vec<_> = (0..size)
            .map(|i| Test {
                pkg: "pkg".into(),
                clazz: format!("Class{i}"),
                method: "test".into(),
                meta_properties: Vec::new(),
            })
            .collect();
        let scan_input = input.clone();

        // The baseline used a single ordered map and scanned it once to select and again to remove.
        let mut scan = BTreeMap::new();
        let start = Instant::now();
        for (sequence, test) in (0u64..).zip(scan_input) {
            scan.insert(((), sequence), test);
        }
        let scan_build = start.elapsed();

        let mut indexed = OrderedQueue::new(Box::new(|_: &Test| ()));
        let start = Instant::now();
        indexed.add_all(input);
        let indexed_build = start.elapsed();

        let start = Instant::now();
        let mut indexed_batches = 0;
        while !indexed.is_empty() {
            black_box(indexed.take_head_class());
            indexed_batches += 1;
        }
        let indexed_drain = start.elapsed();

        let start = Instant::now();
        let mut scan_batches = 0;
        while let Some((_, head)) = scan.first_key_value() {
            let class = head.to_class_name('.');
            let group: Vec<_> = scan
                .values()
                .filter(|test| test.to_class_name('.') == class)
                .cloned()
                .collect();
            scan.retain(|_, test| !group.contains(test));
            black_box(group);
            scan_batches += 1;
        }
        let scan_drain = start.elapsed();

        assert_eq!(scan_batches, size);
        assert_eq!(indexed_batches, size);
        println!(
            "{size} classes | insertion {scan_build:.3?} -> {indexed_build:.3?} | drain {scan_drain:.3?} -> {indexed_drain:.3?}"
        );
    }
}
