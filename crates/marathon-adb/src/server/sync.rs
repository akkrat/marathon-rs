//! The `sync:` service (AOSP `SYNC.TXT` and `file_sync_protocol.h`), v1 and v2, on the device's
//! in-memory filesystem.
//!
//! Every request is `<id:4><length:u32 LE><payload>`. The structs adbd writes are packed, so a v1 stat is
//! 16 bytes, a v1 dent 20 plus the name, a v2 stat 72 and a v2 dent 76 plus the name.

use std::io;
use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use super::fs::Node;
use super::protocol::write_okay;
use super::world::World;

/// SYNC_DATA_MAX: the largest DATA chunk adbd sends and accepts.
const DATA_MAX: usize = 64 * 1024;
/// adbd rejects longer paths.
const PATH_MAX: usize = 1024;
const ENOENT: u32 = 2;

/// SND2/RCV2 flags. The compressed variants aren't implemented, so the default features omit them.
const FLAG_BROTLI: u32 = 1;
const FLAG_LZ4: u32 = 2;
const FLAG_ZSTD: u32 = 4;
const FLAG_DRY_RUN: u32 = 0x8000_0000;

fn sync_fail(msg: &str) -> Vec<u8> {
    let mut out = b"FAIL".to_vec();
    out.extend((msg.len() as u32).to_le_bytes());
    out.extend(msg.as_bytes());
    out
}

/// A stable inode number for a path (FNV-1a), so that repeated stats agree.
fn ino(path: &str) -> u64 {
    path.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The v2 stat body after the id: error, dev, ino, mode, nlink, uid, gid, size, atime, mtime, ctime.
fn stat_v2(path: &str, node: Option<&Node>) -> Vec<u8> {
    let mut out = Vec::with_capacity(68);
    match node {
        None => {
            out.extend(ENOENT.to_le_bytes());
            out.extend([0u8; 64]);
        }
        Some(n) => {
            out.extend(0u32.to_le_bytes());
            out.extend(0u64.to_le_bytes());
            out.extend(ino(path).to_le_bytes());
            out.extend(n.st_mode().to_le_bytes());
            out.extend(1u32.to_le_bytes());
            out.extend(0u32.to_le_bytes());
            out.extend(0u32.to_le_bytes());
            out.extend(n.size().to_le_bytes());
            for _ in 0..3 {
                out.extend((n.mtime as i64).to_le_bytes());
            }
        }
    }
    out
}

fn compression(flags: u32) -> bool {
    flags & (FLAG_BROTLI | FLAG_LZ4 | FLAG_ZSTD) != 0
}

pub(crate) async fn serve(
    world: &Arc<World>,
    serial: &str,
    conn: u64,
    mut rd: OwnedReadHalf,
    mut wr: OwnedWriteHalf,
) -> io::Result<()> {
    write_okay(&mut wr).await?;
    let record = |op: &str, extra: serde_json::Value| {
        let mut v = json!({"type": "sync", "conn": conn, "serial": serial, "op": op});
        if let (Some(m), serde_json::Value::Object(e)) = (v.as_object_mut(), extra) {
            m.extend(e);
        }
        world.transcript.record(v);
    };
    loop {
        let mut hdr = [0u8; 8];
        match rd.read_exact(&mut hdr).await {
            Ok(_) => {}
            // At a command boundary the client may close without QUIT. Unread DONE/OKAY
            // padding can make that close a reset; an interrupted payload still fails below.
            Err(e) if matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset) => {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let id: [u8; 4] = [hdr[0], hdr[1], hdr[2], hdr[3]];
        let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        if &id == b"QUIT" {
            record("QUIT", json!({}));
            return super::device::close(rd, wr).await;
        }
        if !matches!(
            &id,
            b"STAT" | b"STA2" | b"LST2" | b"LIST" | b"LIS2" | b"SEND" | b"SND2" | b"RECV" | b"RCV2"
        ) {
            let name = String::from_utf8_lossy(&id).into_owned();
            world
                .transcript
                .warning(format!("{serial}: unknown sync request {name:?}"));
            wr.write_all(&sync_fail("unknown command")).await?;
            return super::device::close(rd, wr).await;
        }
        if len > PATH_MAX {
            wr.write_all(&sync_fail("path too long")).await?;
            return super::device::close(rd, wr).await;
        }
        let mut path = vec![0u8; len];
        rd.read_exact(&mut path).await?;
        let path = String::from_utf8_lossy(&path).into_owned();
        let op = String::from_utf8_lossy(&id).into_owned();
        match &id {
            b"STAT" => {
                let reply = world
                    .with_device(serial, |d| {
                        let mut out = b"STAT".to_vec();
                        match d.fs.get(&path) {
                            Some(n) => {
                                out.extend(n.st_mode().to_le_bytes());
                                out.extend((n.size() as u32).to_le_bytes());
                                out.extend(n.mtime.to_le_bytes());
                            }
                            // v1 has no error field: a missing file is all zeroes.
                            None => out.extend([0u8; 12]),
                        }
                        out
                    })
                    .unwrap_or_default();
                record(
                    &op,
                    json!({"path": path, "exists": reply.get(4..8) != Some(&[0u8; 4][..])}),
                );
                wr.write_all(&reply).await?;
            }
            b"STA2" | b"LST2" => {
                let (exists, body) = world
                    .with_device(serial, |d| {
                        let n = d.fs.get(&path);
                        (n.is_some(), stat_v2(&super::fs::normalize(&path), n))
                    })
                    .unwrap_or((false, stat_v2(&path, None)));
                record(&op, json!({"path": path, "exists": exists}));
                let mut out = id.to_vec();
                out.extend(body);
                wr.write_all(&out).await?;
            }
            b"LIST" | b"LIS2" => {
                let v2 = &id == b"LIS2";
                let (count, out) = world
                    .with_device(serial, |d| {
                        let dir = super::fs::normalize(&path);
                        let mut entries: Vec<(String, String, Node)> = Vec::new();
                        if let Some(me) = d.fs.get(&dir).filter(|n| n.is_dir()) {
                            // readdir order on a real device is arbitrary; "." and ".." come first here.
                            entries.push((".".into(), dir.clone(), me.clone()));
                            let parent = super::fs::normalize(&format!("{dir}/.."));
                            if let Some(p) = d.fs.get(&parent) {
                                entries.push(("..".into(), parent, p.clone()));
                            }
                            for (name, n) in d.fs.list(&dir) {
                                let full = super::fs::normalize(&format!("{dir}/{name}"));
                                entries.push((name, full, n.clone()));
                            }
                        }
                        let mut out = Vec::new();
                        for (name, full, n) in &entries {
                            if v2 {
                                out.extend(b"DNT2");
                                out.extend(stat_v2(full, Some(n)));
                            } else {
                                out.extend(b"DENT");
                                out.extend(n.st_mode().to_le_bytes());
                                out.extend((n.size() as u32).to_le_bytes());
                                out.extend(n.mtime.to_le_bytes());
                            }
                            out.extend((name.len() as u32).to_le_bytes());
                            out.extend(name.as_bytes());
                        }
                        out.extend(b"DONE");
                        out.extend(vec![0u8; if v2 { 72 } else { 16 }]);
                        (entries.len(), out)
                    })
                    .unwrap_or_default();
                record(&op, json!({"path": path, "entries": count}));
                wr.write_all(&out).await?;
            }
            b"SEND" | b"SND2" => {
                let (target, mode, flags) = if &id == b"SEND" {
                    // "<path>,<mode>" with the mode in decimal.
                    match path.rsplit_once(',') {
                        Some((p, m)) => (p.to_string(), m.parse::<u32>().unwrap_or(0o644), 0),
                        None => (path.clone(), 0o644, 0),
                    }
                } else {
                    let mut setup = [0u8; 12];
                    rd.read_exact(&mut setup).await?;
                    if &setup[..4] != b"SND2" {
                        wr.write_all(&sync_fail("invalid SND2 setup")).await?;
                        return super::device::close(rd, wr).await;
                    }
                    let mode = u32::from_le_bytes([setup[4], setup[5], setup[6], setup[7]]);
                    let flags = u32::from_le_bytes([setup[8], setup[9], setup[10], setup[11]]);
                    (path.clone(), mode, flags)
                };
                if compression(flags) {
                    record(
                        &op,
                        json!({"path": target, "mode": mode, "flags": flags, "error": "compression"}),
                    );
                    wr.write_all(&sync_fail("fake adb: compressed sync is not supported"))
                        .await?;
                    return super::device::close(rd, wr).await;
                }
                let mut data = Vec::new();
                let mtime = loop {
                    let mut h = [0u8; 8];
                    rd.read_exact(&mut h).await?;
                    let n = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
                    match &h[..4] {
                        b"DATA" if n as usize <= DATA_MAX => {
                            let start = data.len();
                            data.resize(start + n as usize, 0);
                            rd.read_exact(&mut data[start..]).await?;
                        }
                        b"DONE" => break n,
                        _ => {
                            wr.write_all(&sync_fail("invalid data message")).await?;
                            return super::device::close(rd, wr).await;
                        }
                    }
                };
                let size = data.len();
                let payload_sha256 = format!("{:x}", Sha256::digest(&data));
                let dry_run = flags & FLAG_DRY_RUN != 0;
                let result = if dry_run {
                    Ok(())
                } else {
                    world
                        .with_device(serial, |d| d.fs.write(&target, data, mode, mtime))
                        .unwrap_or_else(|| Err("no device".into()))
                };
                record(
                    &op,
                    json!({"path": target, "mode": format!("{:o}", mode & 0o7777), "flags": flags, "size": size, "payload_sha256": payload_sha256, "mtime": mtime, "error": result.as_ref().err()}),
                );
                match result {
                    Ok(()) => {
                        let mut out = b"OKAY".to_vec();
                        out.extend([0u8; 4]);
                        wr.write_all(&out).await?;
                    }
                    Err(e) => {
                        wr.write_all(&sync_fail(&e)).await?;
                        return super::device::close(rd, wr).await;
                    }
                }
            }
            _ => {
                // RECV or RCV2.
                if &id == b"RCV2" {
                    let mut setup = [0u8; 8];
                    rd.read_exact(&mut setup).await?;
                    let flags = u32::from_le_bytes([setup[4], setup[5], setup[6], setup[7]]);
                    if &setup[..4] != b"RCV2" || compression(flags) {
                        record(&op, json!({"path": path, "flags": flags, "error": "compression"}));
                        wr.write_all(&sync_fail("fake adb: compressed sync is not supported"))
                            .await?;
                        return super::device::close(rd, wr).await;
                    }
                }
                let content = world
                    .with_device(serial, |d| match d.fs.get(&path) {
                        None => Err("open failed: No such file or directory".to_string()),
                        Some(n) => n
                            .content
                            .clone()
                            .ok_or_else(|| "read failed: Is a directory".to_string()),
                    })
                    .unwrap_or_else(|| Err("no device".into()));
                record(
                    &op,
                    json!({"path": path, "size": content.as_ref().ok().map(Vec::len), "error": content.as_ref().err()}),
                );
                match content {
                    Ok(c) => {
                        let mut out = Vec::with_capacity(c.len() + c.len() / DATA_MAX * 8 + 16);
                        for chunk in c.chunks(DATA_MAX) {
                            out.extend(b"DATA");
                            out.extend((chunk.len() as u32).to_le_bytes());
                            out.extend(chunk);
                        }
                        out.extend(b"DONE");
                        out.extend([0u8; 4]);
                        wr.write_all(&out).await?;
                    }
                    Err(e) => {
                        // adbd ends the sync session after a failed RECV.
                        wr.write_all(&sync_fail(&e)).await?;
                        return super::device::close(rd, wr).await;
                    }
                }
            }
        }
    }
}
