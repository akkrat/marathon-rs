//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/remote/LineBufferTest.kt
use marathon_apple::cmd::line_buffer::LineBuffer;
use std::sync::{Arc, Mutex};
fn buffer() -> (Arc<LineBuffer>, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let received = lines.clone();
    (
        Arc::new(LineBuffer::new(move |line| {
            received.lock().unwrap().push(line);
            Box::pin(async { Ok(()) })
        })),
        lines,
    )
}
#[tokio::test]
async fn flushing_empty_buffer_should_not_produce_any_lines() {
    let (buffer, lines) = buffer();
    buffer.flush().await.unwrap();
    assert!(lines.lock().unwrap().is_empty());
}
#[tokio::test]
async fn appending_data_multiple_times_should_not_remove_previously_received_lines() {
    let (buffer, lines) = buffer();
    buffer.append(b"first one\nfirst two\nfirst three\n");
    buffer.append(b"second one\nsecond two\nsecond three\n");
    buffer.flush().await.unwrap();
    assert_eq!(lines.lock().unwrap().len(), 6);
}
#[tokio::test]
async fn flushing_incomplete_data_should_produce_only_terminated_lines() {
    let (buffer, lines) = buffer();
    buffer.append(b"first one\nfirst two\nfirst three");
    buffer.flush().await.unwrap();
    assert_eq!(lines.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn draining_incomplete_data_should_produce_both_terminated_and_unterminated_lines() {
    let (buffer, lines) = buffer();
    buffer.append(b"first one\nfirst two\nfirst three");
    buffer.drain().await.unwrap();
    assert_eq!(lines.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn closing_buffer_should_drain_all_data() {
    let (buffer, lines) = buffer();
    buffer.append(b"first one\nfirst two\nfirst three");
    buffer.close().await.unwrap();
    assert_eq!(lines.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn cr_lf_and_crlf_should_each_terminate_one_line() {
    let (buffer, lines) = buffer();
    buffer.append(b"first\rsecond\nthird\r\nfourth\r");
    buffer.flush().await.unwrap();
    buffer.drain().await.unwrap();
    assert_eq!(*lines.lock().unwrap(), ["first", "second", "third", "fourth"]);
}
#[tokio::test]
async fn crlf_split_across_chunks_and_flush_should_terminate_only_one_line() {
    let (buffer, lines) = buffer();
    buffer.append(b"first\r");
    buffer.flush().await.unwrap();
    buffer.append(b"");
    buffer.append_count(b"ignored", 0);
    buffer.flush().await.unwrap();
    buffer.append(b"\nsecond\r");
    buffer.flush().await.unwrap();
    buffer.append(b"\n");
    buffer.drain().await.unwrap();
    assert_eq!(*lines.lock().unwrap(), ["first", "second"]);
}
#[tokio::test]
async fn cr_should_not_consume_a_following_non_lf_character() {
    let (buffer, lines) = buffer();
    buffer.append(b"first\r");
    buffer.flush().await.unwrap();
    buffer.append(b"second\n\nthird");
    buffer.drain().await.unwrap();
    assert_eq!(*lines.lock().unwrap(), ["first", "second", "", "third"]);
}
#[tokio::test]
async fn append_should_normalize_only_the_requested_bytes() {
    let (buffer, lines) = buffer();
    buffer.append_count(b"first\rignored", 6);
    buffer.flush().await.unwrap();
    buffer.append_count(b"\nsecond\nignored", 8);
    buffer.drain().await.unwrap();
    assert_eq!(*lines.lock().unwrap(), ["first", "second"]);
}
#[tokio::test]
async fn literal_escaped_line_endings_should_remain_unchanged() {
    let (buffer, lines) = buffer();
    buffer.append(b"first\\r\\n|\\rsecond\n");
    buffer.drain().await.unwrap();
    assert_eq!(*lines.lock().unwrap(), ["first\\r\\n|\\rsecond"]);
}
#[tokio::test]
async fn flush_should_retain_whitespace_in_an_incomplete_line() {
    let (buffer, lines) = buffer();
    buffer.append(b"first\n  ");
    buffer.flush().await.unwrap();
    buffer.append(b"second\n");
    buffer.drain().await.unwrap();
    assert_eq!(*lines.lock().unwrap(), ["first", "  second"]);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_append_and_flush_should_deliver_every_line_exactly_once() {
    for _ in 0..5 {
        let (buffer, lines) = buffer();
        let writer = buffer.clone();
        let task = tokio::spawn(async move {
            for i in 0..20_000 {
                writer.append(format!("Test Case '{i}' passed\n").as_bytes());
                tokio::task::yield_now().await;
            }
        });
        while !task.is_finished() {
            buffer.flush().await.unwrap();
            tokio::task::yield_now().await;
        }
        task.await.unwrap();
        buffer.drain().await.unwrap();
        assert_eq!(
            *lines.lock().unwrap(),
            (0..20_000)
                .map(|i| format!("Test Case '{i}' passed"))
                .collect::<Vec<_>>()
        );
    }
}
// Ours: approved P4 plus malformed/truncated UTF-8, beyond the Kotlin rows.
#[tokio::test]
async fn utf8_is_decoded_incrementally_and_truncated_suffix_is_replaced() {
    let (buffer, lines) = buffer();
    for byte in "é😀\n".as_bytes() {
        buffer.append(&[*byte]);
        buffer.flush().await.unwrap();
    }
    buffer.append(&[0xc3]);
    buffer.drain().await.unwrap();
    assert_eq!(*lines.lock().unwrap(), ["é😀", "�"]);
}
