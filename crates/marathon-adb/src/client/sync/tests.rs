//! Port of adam 0.5.8 `adam/src/test/kotlin/com/malinskiy/adam/request/sync/{v1,v2,compat}/*Test.kt`
//! and `.../io/{AsyncFileReaderTest,AsyncFileWriterTest}.kt`.
//! Copyright (C) 2021 Anton Malinskiy; Apache-2.0.

use super::*;
use crate::client::stub;
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/adam/fixture/sample.yaml")
}
fn fields(v2: bool) -> Vec<u8> {
    let mut b = Vec::new();
    if v2 {
        b.extend_from_slice(&0u32.to_le_bytes());
        for n in [114u64, 111221] {
            b.extend_from_slice(&n.to_le_bytes());
        }
        for n in [123u32, 2, 0, 1000] {
            b.extend_from_slice(&n.to_le_bytes());
        }
        for n in [420u64, 1589042331, 1589042332, 1589042333] {
            b.extend_from_slice(&n.to_le_bytes());
        }
    } else {
        for n in [123u32, 420, 1589042331] {
            b.extend_from_slice(&n.to_le_bytes());
        }
    }
    b
}
fn expected_v2(name: Option<String>) -> FileEntry {
    FileEntry::V2(FileEntryV2 {
        error: 0,
        dev: 114,
        ino: 111221,
        mode: 123,
        nlink: 2,
        uid: 0,
        gid: 1000,
        size: 420,
        atime: instant_of(1589042331, 0).unwrap(),
        mtime: instant_of(1589042332, 0).unwrap(),
        ctime: instant_of(1589042333, 0).unwrap(),
        name,
    })
}
fn assert_v2_fields(entry: &FileEntry) {
    let FileEntry::V2(entry) = entry else {
        panic!("expected v2 entry")
    };
    assert_eq!(entry.error, 0);
    assert_eq!(entry.dev, 114);
    assert_eq!(entry.ino, 111221);
    assert_eq!(entry.mode, 123);
    assert_eq!(entry.nlink, 2);
    assert_eq!(entry.uid, 0);
    assert_eq!(entry.gid, 1000);
    assert_eq!(entry.size, 420);
    assert_eq!(entry.atime.seconds(), 1589042331);
    assert_eq!(entry.mtime.seconds(), 1589042332);
    assert_eq!(entry.ctime.seconds(), 1589042333);
}
async fn stat(v2: bool) {
    let server = stub::session(move |mut s| async move {
        s.expect_cmd("host:transport:serial").await;
        s.accept().await;
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(if v2 { b"LST2" } else { b"STAT" }, "/sdcard/testfile")
            .await;
        s.respond(if v2 { b"LST2" } else { b"STAT" }).await;
        if v2 {
            s.respond(&fields(true)).await;
        } else {
            for n in [0x744u32, 128, 10000] {
                s.respond(&n.to_le_bytes()).await;
            }
        }
    })
    .await;
    let request = if v2 {
        StatFileRequest::v2("/sdcard/testfile", vec![Feature::StatV2])
    } else {
        StatFileRequest::new("/sdcard/testfile")
    };
    let out = server.client().execute(request, Some("serial")).await.unwrap();
    if v2 {
        assert_eq!(out, expected_v2(None));
        assert_v2_fields(&out);
        assert_eq!(out.name(), None);
    } else {
        assert_eq!(out.mode(), 0x744);
        assert_eq!(out.size(), 128);
        assert_eq!(out.mtime().seconds(), 10000);
    }
    server.finish().await;
}
async fn list(v2: bool) {
    let server = stub::session(move |mut s| async move {
        s.expect_cmd("host:transport:serial").await;
        s.accept().await;
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(if v2 { b"LIS2" } else { b"LIST" }, "/sdcard/").await;
        s.respond(if v2 { b"DNT2" } else { b"DENT" }).await;
        s.respond(&fields(v2)).await;
        s.respond(&9u32.to_le_bytes()).await;
        s.respond(b"some-fileDONE").await;
    })
    .await;
    let request = if v2 {
        ListFileRequest::v2("/sdcard/", vec![Feature::LsV2])
    } else {
        ListFileRequest::new("/sdcard/")
    };
    let out = server.client().execute(request, Some("serial")).await.unwrap();
    let expected = if v2 {
        expected_v2(Some("some-file".into()))
    } else {
        FileEntry::V1(FileEntryV1 {
            name: Some("some-file".into()),
            mode: 123,
            size: 420,
            mtime: instant_of(1589042331, 0).unwrap(),
        })
    };
    assert_eq!(out, vec![expected]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name(), Some("some-file"));
    if v2 {
        assert_v2_fields(&out[0]);
    } else {
        assert_eq!(out[0].mode(), 123);
        assert_eq!(out[0].size(), 420);
        assert_eq!(out[0].mtime().seconds(), 1589042331);
    }
    server.finish().await;
}
async fn pull(v2: bool, kind: u8) {
    let bytes = tokio::fs::read(fixture()).await.unwrap();
    let expected = bytes.clone();
    let size = bytes.len();
    let server = stub::session(move |mut s| async move {
        s.expect_cmd("host:transport:serial").await;
        s.accept().await;
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(b"STAT", "/sdcard/testfile").await;
        s.respond(b"STAT").await;
        for n in [420u32, size as u32, 0] {
            s.respond(&n.to_le_bytes()).await;
        }
        s.expect_sync(if v2 { b"RCV2" } else { b"RECV" }, "/sdcard/testfile")
            .await;
        if v2 {
            s.expect_bytes(b"RCV2\0\0\0\0").await;
        }
        match kind {
            0 | 1 => {
                for data in bytes.chunks(if kind == 0 { size } else { 100 }) {
                    s.respond(b"DATA").await;
                    s.respond(&(data.len() as u32).to_le_bytes()).await;
                    s.respond(data).await;
                }
                s.respond(b"DONE\0\0\0\0").await;
                s.discard().await;
            }
            2 => {
                s.respond(b"FAIL\x0b\0\0\0lorem ipsum").await;
            }
            3 => {
                s.respond(b"DATA\x01\0\x01\0").await;
            }
            _ => {
                s.respond(b"SENDSEND").await;
            }
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("file");
    let request = if v2 {
        PullFileRequest::v2("/sdcard/testfile", local.clone(), vec![Feature::SendrecvV2], None)
    } else {
        PullFileRequest::new("/sdcard/testfile", local.clone(), None)
    };
    let completion = request.completion();
    let (logging, events) = crate::client::log::capture::logging(true);
    let mut stream = server
        .client_with(Some(logging))
        .execute_channel(request, Some("serial"))
        .unwrap();
    let mut progress = vec![];
    let mut failure = None;
    while let Some(update) = stream.next().await {
        match update {
            Ok(v) => progress.push(v),
            Err(e) => failure = Some(e),
        }
    }
    completion.close().await.unwrap();
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, message)| message == "Exception during cleanup. Ignoring")
    );
    if kind < 2 {
        assert!(failure.is_none());
        assert_eq!(progress.last(), Some(&1.0));
        assert_eq!(tokio::fs::read(local).await.unwrap(), expected);
    } else if kind == 2 {
        assert_eq!(
            failure,
            Some(AdbError::PullFailed(
                "Failed to pull file /sdcard/testfile: lorem ipsum".into()
            ))
        );
    } else {
        assert!(matches!(failure, Some(AdbError::UnsupportedSyncProtocol(_))));
    }
    server.finish().await;
}
async fn push(v2: bool, fail: bool) {
    let bytes = tokio::fs::read(fixture()).await.unwrap();
    let server = stub::session(move |mut s| async move {
        s.expect_cmd("host:transport:serial").await;
        s.accept().await;
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(
            if v2 { b"SND2" } else { b"SEND" },
            if v2 { "/sdcard/testfile" } else { "/sdcard/testfile,511" },
        )
        .await;
        if v2 {
            s.expect_bytes(b"SND2\xff\x01\0\0\0\0\0\0").await;
        }
        let mut content = vec![];
        loop {
            let h = s.read_exact(8).await;
            if &h[..4] == b"DONE" {
                break;
            }
            assert_eq!(&h[..4], b"DATA");
            let n = u32_at(&h, 4) as usize;
            assert!(n <= 65528);
            content.extend(s.read_exact(n).await);
        }
        assert_eq!(content, bytes);
        if fail {
            s.reject("CAFEBABE").await;
        } else {
            s.accept().await;
        }
    })
    .await;
    let mut request = PushFileRequest::prepare(fixture(), "/sdcard/testfile", "0777")
        .await
        .unwrap();
    if v2 {
        request = request.with_v2(vec![Feature::SendrecvV2], false);
    }
    let completion = request.completion().unwrap();
    let mut stream = server.client().execute_channel(request, Some("serial")).unwrap();
    let mut progress = vec![];
    let mut failure = None;
    while let Some(update) = stream.next().await {
        match update {
            Ok(v) => progress.push(v),
            Err(e) => failure = Some(e),
        }
    }
    completion.close().await.unwrap();
    if fail {
        assert_eq!(
            failure,
            Some(AdbError::PushFailed(
                "adb didn't acknowledge the file transfer: CAFEBABE".into()
            ))
        );
    } else {
        assert!(failure.is_none());
        assert_eq!(progress, vec![1.0, 1.0]);
    }
    server.finish().await;
}
macro_rules! version_tests {
    ($module:ident,$v2:expr) => {
        mod $module {
            use super::*;
            mod stat_file_request_test {
                use super::*;
                #[tokio::test]
                async fn test_returns_proper_content() {
                    stat($v2).await;
                }
            }
            mod list_file_request_test {
                use super::*;
                #[tokio::test]
                async fn test_serialize() {
                    list($v2).await;
                }
            }
            mod pull_file_request_test {
                use super::*;
                #[test]
                fn test_serialize() {
                    assert_eq!(
                        PullFileRequest::new("/sdcard/testfile", PathBuf::from("/tmp/testfile"), None).serialize(),
                        b"0005sync:"
                    );
                }
                #[tokio::test]
                async fn test_single_chunk_happy_path() {
                    pull($v2, 0).await;
                }
                #[tokio::test]
                async fn test_multiple_chunks_happy_path() {
                    pull($v2, 1).await;
                }
                #[tokio::test]
                async fn test_transport_fail() {
                    pull($v2, 2).await;
                }
                #[tokio::test]
                async fn test_transport_packet_size_failure() {
                    pull($v2, 3).await;
                }
                #[tokio::test]
                async fn test_transport_unsupported_packet() {
                    pull($v2, 4).await;
                }
            }
            mod push_file_request_test {
                use super::*;
                #[tokio::test]
                async fn test_serialize() {
                    let r = PushFileRequest::prepare(fixture(), "/x", "0777").await.unwrap();
                    assert_eq!(r.serialize(), b"0005sync:");
                    r.completion().unwrap().close().await.unwrap();
                }
                #[tokio::test]
                async fn test_single_chunk_happy_path() {
                    push($v2, false).await;
                }
                #[tokio::test]
                async fn test_transport_failure_on_done() {
                    push($v2, true).await;
                }
            }
        }
    };
}
version_tests!(v1, false);
version_tests!(v2, true);
mod compat_stat_file_request_test {
    use super::*;
    #[tokio::test]
    async fn test_v1() {
        assert!(!CompatStatFileRequest::new("/x", vec![Feature::LsV2]).0.v2);
        stat(false).await;
    }
    #[tokio::test]
    async fn test_returns_proper_content() {
        assert!(CompatStatFileRequest::new("/x", vec![Feature::StatV2]).0.v2);
        stat(true).await;
    }
}
mod compat_list_file_request_test {
    use super::*;
    #[tokio::test]
    async fn test_v1() {
        assert!(!CompatListFileRequest::new("/x", vec![Feature::StatV2]).0.v2);
        list(false).await;
    }
    #[tokio::test]
    async fn test_v2() {
        assert!(CompatListFileRequest::new("/x", vec![Feature::LsV2]).0.v2);
        list(true).await;
    }
}
mod compat_pull_file_request_test {
    use super::*;
    #[tokio::test]
    async fn test_v1() {
        assert!(
            !CompatPullFileRequest::new("/x", PathBuf::new(), vec![Feature::StatV2], None)
                .0
                .v2
        );
        pull(false, 0).await;
    }
    #[tokio::test]
    async fn test_v2() {
        assert!(
            CompatPullFileRequest::new("/x", PathBuf::new(), vec![Feature::SendrecvV2], None)
                .0
                .v2
        );
        pull(true, 0).await;
    }
}
mod compat_push_file_request_test {
    use super::*;
    async fn run(v2: bool) {
        let r = CompatPushFileRequest::prepare(
            fixture(),
            "/x",
            if v2 {
                vec![Feature::SendrecvV2]
            } else {
                vec![Feature::StatV2]
            },
            "0777",
        )
        .await
        .unwrap();
        assert_eq!(r.0.v2, v2);
        r.0.completion().unwrap().close().await.unwrap();
        push(v2, false).await;
    }
    #[tokio::test]
    async fn test_v1() {
        run(false).await;
    }
    #[tokio::test]
    async fn test_v2() {
        run(true).await;
    }
}
mod async_file_reader_test {
    use super::*;
    #[tokio::test]
    async fn test_extension() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x");
        tokio::fs::write(&p, b"Something interesting!").await.unwrap();
        let r = AsyncFileReader::prepare(&p).await.unwrap();
        assert_eq!(r.read(65536).await.unwrap(), b"Something interesting!");
        assert!(r.read(65536).await.unwrap().is_empty());
        r.close().await.unwrap();
    }
}
mod async_file_writer_test {
    use super::*;
    #[tokio::test]
    async fn test_write() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("folder-that-doesnt-exist/file-that-doesnt-exist.test");
        let w = AsyncFileWriter::new(p.clone());
        w.start().await.unwrap();
        w.write(b"Something interesting!").await.unwrap();
        w.close().await.unwrap();
        assert_eq!(tokio::fs::read(p).await.unwrap(), b"Something interesting!");
    }
}

#[tokio::test]
async fn limits_and_features_are_independent() {
    assert!(StatFileRequest::new("é".repeat(1024)).validate().success);
    assert!(!ListFileRequest::new("😀".repeat(513)).validate().success);
    let r = PushFileRequest::prepare(fixture(), "é".repeat(513), "0777")
        .await
        .unwrap();
    assert!(!r.validate().success);
    r.completion().unwrap().close().await.unwrap();
    let r = PushFileRequest::prepare(fixture(), "/x", "0777")
        .await
        .unwrap()
        .with_v2(vec![Feature::SendrecvV2], true);
    assert_eq!(
        r.validate().message,
        Some(ValidationResponse::missing_feature(Feature::SendrecvV2DryRunSend))
    );
    r.completion().unwrap().close().await.unwrap();
}
#[tokio::test]
async fn list_unexpected_tag_ends_with_accumulated_entries() {
    let server = stub::session(|mut s| async move {
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(b"LIST", "/x").await;
        s.respond(b"WHAT").await;
    })
    .await;
    assert!(
        server
            .client()
            .execute(ListFileRequest::new("/x"), None)
            .await
            .unwrap()
            .is_empty()
    );
    server.finish().await;
}
#[tokio::test]
async fn dropped_pull_can_be_joined_and_next_cancellation_loses_nothing() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let server = stub::session(|mut s| async move {
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(b"RECV", "/x").await;
        s.respond(b"DATA\x03\0\0\0a").await;
        tx.send(()).unwrap();
        wait.await.unwrap();
        s.respond(b"bc").await;
        s.discard().await;
    })
    .await;
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("x");
    let request = PullFileRequest::new("/x", path.clone(), Some(3));
    let completion = request.completion();
    let mut stream = server.client().execute_channel(request, None).unwrap();
    tokio::select! {r=stream.next()=>panic!("premature {r:?}"),_=rx=>{}}
    release.send(()).unwrap();
    assert_eq!(stream.next().await, Some(Ok(1.0)));
    drop(stream);
    completion.close().await.unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"abc");
    server.finish().await;
}
mod list_files_request_test {
    use super::listing::*;
    use super::*;
    #[tokio::test]
    async fn test_returns_proper_content() {
        let response = "-rwxrwx--x 2 root sdcard_rw 4096 2020-10-24 16:29 Alarms\nbrwxrwx--x 2 root sdcard_rw 4096 2020-10-24 16:29 Android\nlrwxrwx--x 2 root sdcard_rw 4096 2020-10-24 16:29 DCIM -> ../Camera\ncrwxrwx--x 2 root sdcard_rw 4096 2020-12-01 19:11 Download\nsrwxrwx--x 2 root sdcard_rw 4096 2020-10-24 16:29 Movies\nprwxrwx--x 2 root sdcard_rw 4096 2020-10-24 16:29 Music\ndrwxrwx--x 2 root sdcard_rw 4096 2020-10-24 16:29 Ringtones\nOrwxrwx--x 2 root sdcard_rw 4096 2020-10-24 16:29 XXX\nx0";
        let s = stub::session(move |mut s| async move {
            s.expect_cmd("host:transport:serial").await;
            s.accept().await;
            s.expect_cmd("shell:ls -l /sdcard/;echo x$?").await;
            s.accept().await;
            s.respond(response.as_bytes()).await;
        })
        .await;
        let out = s
            .client()
            .execute(ListFilesRequest::new("/sdcard/"), Some("serial"))
            .await
            .unwrap();
        s.finish().await;
        let expected = vec![
            (
                "-rwxrwx--x",
                "Alarms",
                AndroidFileType::RegularFile,
                None,
                "2020-10-24",
                "16:29",
            ),
            (
                "brwxrwx--x",
                "Android",
                AndroidFileType::BlockSpecialFile,
                None,
                "2020-10-24",
                "16:29",
            ),
            (
                "lrwxrwx--x",
                "DCIM",
                AndroidFileType::SymbolicLink,
                Some("../Camera"),
                "2020-10-24",
                "16:29",
            ),
            (
                "crwxrwx--x",
                "Download",
                AndroidFileType::CharacterSpecialFile,
                None,
                "2020-12-01",
                "19:11",
            ),
            (
                "srwxrwx--x",
                "Movies",
                AndroidFileType::SocketLink,
                None,
                "2020-10-24",
                "16:29",
            ),
            (
                "prwxrwx--x",
                "Music",
                AndroidFileType::Fifo,
                None,
                "2020-10-24",
                "16:29",
            ),
            (
                "drwxrwx--x",
                "Ringtones",
                AndroidFileType::Directory,
                None,
                "2020-10-24",
                "16:29",
            ),
        ]
        .into_iter()
        .map(|(permissions, name, file_type, link, date, time)| AndroidFile {
            permissions: permissions.into(),
            name: name.into(),
            file_type,
            link: link.map(str::to_string),
            date: date.into(),
            time: time.into(),
            owner: "root".into(),
            group: "sdcard_rw".into(),
            directory: "/sdcard/".into(),
            size: 4096,
        })
        .collect::<Vec<_>>();
        assert_eq!(out, expected);
    }
    #[test]
    fn symlink_requires_one_arrow_and_bad_size_is_zero() {
        let r = ListFilesRequest::new("/x");
        assert!(matches!(
            r.parse("lrwxrwx--x root root 1 2020-10-24 16:29 a -> b -> c"),
            Err(AdbError::Runtime(_))
        ));
        assert_eq!(
            r.parse("-rwxrwx--x root root 1, 2 2020-10-24 16:29 x").unwrap()[0].size,
            0
        );
    }
}

#[tokio::test]
async fn n18_is_observable_through_stat_and_list() {
    for list in [false, true] {
        let server = stub::session(move |mut s| async move {
            s.expect_cmd("sync:").await;
            s.accept().await;
            s.expect_sync(if list { b"LIS2" } else { b"LST2" }, "/x").await;
            s.respond(if list { b"DNT2" } else { b"LST2" }).await;
            let mut f = fields(true);
            f[4..12].copy_from_slice(&(1u64 << 32).to_le_bytes());
            f[36..44].copy_from_slice(&(1u64 << 32).to_le_bytes());
            s.respond(&f).await;
            if list {
                s.respond(b"\x01\0\0\0aWHAT").await;
            }
        })
        .await;
        let entry = if list {
            server
                .client()
                .execute(ListFileRequest::v2("/x", vec![Feature::LsV2]), None)
                .await
                .unwrap()
                .remove(0)
        } else {
            server
                .client()
                .execute(StatFileRequest::v2("/x", vec![Feature::StatV2]), None)
                .await
                .unwrap()
        };
        assert_eq!(entry.size(), 1);
        let FileEntry::V2(e) = entry else { panic!() };
        assert_eq!(e.dev, 1);
        server.finish().await;
    }
}
#[tokio::test]
async fn zero_size_pull_preserves_absent_file_and_has_no_final_progress() {
    let s = stub::session(|mut s| async move {
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(b"RECV", "/x").await;
        s.respond(b"DONE\0\0\0\0").await;
    })
    .await;
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("absent");
    let request = PullFileRequest::new("/x", path.clone(), Some(0));
    let completion = request.completion();
    let mut stream = s.client().execute_channel(request, None).unwrap();
    assert!(stream.next().await.is_none());
    completion.close().await.unwrap();
    assert!(!tokio::fs::try_exists(path).await.unwrap());
    s.finish().await;
}
#[tokio::test]
async fn progress_is_not_clamped() {
    let s = stub::session(|mut s| async move {
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(b"RECV", "/x").await;
        s.respond(b"DATA\x03\0\0\0abcDONE\0\0\0\0").await;
    })
    .await;
    let d = tempfile::tempdir().unwrap();
    let request = PullFileRequest::new("/x", d.path().join("x"), Some(2));
    let completion = request.completion();
    let mut stream = s.client().execute_channel(request, None).unwrap();
    assert_eq!(stream.next().await, Some(Ok(1.5)));
    assert!(stream.next().await.is_none());
    completion.close().await.unwrap();
    s.finish().await;
}
#[tokio::test]
async fn malformed_short_and_negative_records_fail_explicitly() {
    for payload in [b"DA".as_slice(), b"DATA\xff\xff\xff\xff", b"DATA\x03\0\0\0a"] {
        let s = stub::session(move |mut s| async move {
            s.expect_cmd("sync:").await;
            s.accept().await;
            s.expect_sync(b"RECV", "/x").await;
            s.respond(payload).await;
        })
        .await;
        let d = tempfile::tempdir().unwrap();
        let request = PullFileRequest::new("/x", d.path().join("x"), Some(3));
        let completion = request.completion();
        let mut stream = s.client().execute_channel(request, None).unwrap();
        assert!(matches!(
            stream.next().await,
            Some(Err(AdbError::UnsupportedSyncProtocol(_)))
        ));
        completion.close().await.unwrap();
        s.finish().await;
    }
}

#[tokio::test]
async fn empty_push_acknowledges_once_and_dry_run_flags_are_exact() {
    let d = tempfile::tempdir().unwrap();
    let local = d.path().join("empty");
    tokio::fs::write(&local, b"").await.unwrap();
    let s = stub::session(|mut s| async move {
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(b"SND2", "/x").await;
        s.expect_bytes(b"SND2\xff\x01\0\0\0\0\0\x80").await;
        let done = s.read_exact(8).await;
        assert_eq!(&done[..4], b"DONE");
        s.accept().await;
    })
    .await;
    let request = PushFileRequest::prepare(local, "/x", "0777")
        .await
        .unwrap()
        .with_v2(vec![Feature::SendrecvV2, Feature::SendrecvV2DryRunSend], true);
    let completion = request.completion().unwrap();
    let mut stream = s.client().execute_channel(request, None).unwrap();
    assert_eq!(stream.next().await, Some(Ok(1.0)));
    assert!(stream.next().await.is_none());
    completion.close().await.unwrap();
    s.finish().await;
}
#[tokio::test]
async fn early_local_eof_is_an_error_and_failed_file_creation_is_not_success() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("file");
    tokio::fs::write(&path, b"abc").await.unwrap();
    let request = PushFileRequest::prepare(path.clone(), "/x", "0777").await.unwrap();
    tokio::fs::write(&path, b"").await.unwrap();
    let completion = request.completion().unwrap();
    let s = stub::session(|mut s| async move {
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.expect_sync(b"SEND", "/x,511").await;
        assert!(s.read_to_end().await.is_empty());
    })
    .await;
    let mut stream = s.client().execute_channel(request, None).unwrap();
    assert_eq!(
        stream.next().await,
        Some(Err(AdbError::Io("Unexpected end of local file".into())))
    );
    completion.close().await.unwrap();
    s.finish().await;
    let s = stub::session(|mut s| async move {
        s.expect_cmd("sync:").await;
        s.accept().await;
        s.discard().await;
    })
    .await;
    let request = PullFileRequest::new("/x", path.join("impossible"), Some(3));
    let completion = request.completion();
    let mut stream = s.client().execute_channel(request, None).unwrap();
    assert!(matches!(stream.next().await, Some(Err(AdbError::Io(_)))));
    completion.close().await.unwrap();
    s.finish().await;
}

#[tokio::test]
async fn empty_directory_push_sends_bare_mkdir_like_jvm() {
    let s = stub::sessions(3, |n, mut s| async move {
        let rows = include_str!("../../../../../fixtures/oracle/adb-request-probes.jsonl");
        let key = format!("directory-command:true:{n}");
        let value = rows
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v["case"] == key)
            .unwrap();
        let bytes = value["value"]
            .as_str()
            .unwrap()
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        s.expect_cmd(std::str::from_utf8(&bytes).unwrap()).await;
        s.accept().await;
        if n == 0 {
            s.expect_sync(b"STAT", "/dest").await;
            s.respond(b"STAT\0\0\0\0\0\0\0\0\0\0\0\0").await;
        } else {
            s.respond(b"x0").await;
        }
    })
    .await;
    let d = tempfile::tempdir().unwrap();
    let r = directory::PushRequest::new(d.path().to_path_buf(), "/dest".into(), vec![]);
    let completion = r.completion();
    assert!(r.execute(&s.client(), None).await.unwrap());
    completion.close().await.unwrap();
    s.finish().await;
}

// Mode 0o140755 sets both the directory and the regular-file bits; adam checks the directory
// bit first, so the folder lands in `<dest>/<name>` instead of failing as "target is a file".
#[tokio::test]
async fn directory_push_into_target_with_directory_and_file_bits() {
    let d = tempfile::tempdir().unwrap();
    let name = d.path().file_name().unwrap().to_string_lossy().into_owned();
    let mkdir = format!("shell:mkdir -p '/dest/{name}';echo x$?");
    let s = stub::sessions(3, move |n, mut s| {
        let mkdir = mkdir.clone();
        async move {
            match n {
                0 => {
                    s.expect_cmd("sync:").await;
                    s.accept().await;
                    s.expect_sync(b"STAT", "/dest").await;
                    let mut stat = b"STAT".to_vec();
                    stat.extend(0o140755u32.to_le_bytes());
                    stat.extend([0; 8]);
                    s.respond(&stat).await;
                }
                1 => {
                    s.expect_cmd(&mkdir).await;
                    s.accept().await;
                    s.respond(b"x0").await;
                }
                _ => {
                    s.expect_cmd("shell:mkdir;echo x$?").await;
                    s.accept().await;
                    s.respond(b"x0").await;
                }
            }
        }
    })
    .await;
    let r = directory::PushRequest::new(d.path().to_path_buf(), "/dest".into(), vec![]);
    let completion = r.completion();
    assert!(r.execute(&s.client(), None).await.unwrap());
    completion.close().await.unwrap();
    s.finish().await;
}

#[tokio::test]
async fn push_samples_mtime_at_done_after_preparation() {
    for v2 in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("file");
        tokio::fs::write(&local, b"abc").await.unwrap();
        let file = tokio::fs::File::open(&local).await.unwrap().into_std().await;
        tokio::task::spawn_blocking(move || {
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(0x01020304))
                .unwrap();
        })
        .await
        .unwrap();
        let mut request = PushFileRequest::prepare(local.clone(), "/x", "0777").await.unwrap();
        if v2 {
            request = request.with_v2(vec![Feature::SendrecvV2], false);
        }
        let completion = request.completion().unwrap();
        let server = stub::session(move |mut s| async move {
            s.expect_cmd("sync:").await;
            s.accept().await;
            s.expect_sync(if v2 { b"SND2" } else { b"SEND" }, if v2 { "/x" } else { "/x,511" })
                .await;
            if v2 {
                s.expect_bytes(b"SND2\xff\x01\0\0\0\0\0\0").await;
            }
            s.expect_bytes(b"DATA\x03\0\0\0abc").await;
            s.expect_bytes(b"DONE\x05\x06\x07\x08").await;
            s.accept().await;
        })
        .await;
        let mut stream = server.client().execute_channel(request, None).unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), 1.0);
        let file = tokio::fs::File::open(&local).await.unwrap().into_std().await;
        tokio::task::spawn_blocking(move || {
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(0x05060708))
                .unwrap();
        })
        .await
        .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), 1.0);
        assert!(stream.next().await.is_none());
        completion.close().await.unwrap();
        server.finish().await;
    }
}
