//! Host services (`SERVICES.TXT`, "host:" part): answered by the server itself, without a device.

use std::io;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use super::protocol::{protocol_string, write_fail, write_okay};
use super::script::KillBehaviour;
use super::world::{KillRequest, Selector, Transport, World};

pub(crate) enum HostOutcome {
    /// The reply is written; the connection ends.
    Done,
    /// A transport switch succeeded; the next request on this connection goes to the device.
    Transport(Transport),
    /// `host:track-devices`: the connection stays open and streams the device list.
    Track { long: bool },
}

pub(crate) fn is_host_service(req: &str) -> bool {
    [
        "host:",
        "host-serial:",
        "host-usb:",
        "host-local:",
        "host-transport-id:",
    ]
    .iter()
    .any(|p| req.starts_with(p))
}

/// Commands that may follow `host-serial:<serial>:`. A serial such as `127.0.0.1:5555` contains colons,
/// so the serial ends at the first colon that is followed by one of these.
const DEVICE_COMMANDS: &[&str] = &["features", "get-state", "get-serialno", "get-devpath"];

/// Splits a host request into its device selector (`None` for plain `host:`) and its command.
fn parse(req: &str) -> Option<(Option<Selector>, &str)> {
    if let Some(rest) = req.strip_prefix("host:") {
        return Some((None, rest));
    }
    if let Some(rest) = req.strip_prefix("host-usb:") {
        return Some((Some(Selector::Usb), rest));
    }
    if let Some(rest) = req.strip_prefix("host-local:") {
        return Some((Some(Selector::Local), rest));
    }
    if let Some(rest) = req.strip_prefix("host-transport-id:") {
        let (id, cmd) = rest.split_once(':')?;
        return Some((Some(Selector::Id(id.parse().ok()?)), cmd));
    }
    let rest = req.strip_prefix("host-serial:")?;
    rest.match_indices(':').map(|(i, _)| i).find_map(|i| {
        let cmd = &rest[i + 1..];
        DEVICE_COMMANDS
            .contains(&cmd)
            .then(|| (Some(Selector::Serial(rest[..i].to_string())), cmd))
    })
}

async fn reply(wr: &mut OwnedWriteHalf, body: &str) -> io::Result<HostOutcome> {
    let mut out = b"OKAY".to_vec();
    out.extend(protocol_string(body.as_bytes()));
    wr.write_all(&out).await?;
    Ok(HostOutcome::Done)
}

async fn fail(wr: &mut OwnedWriteHalf, msg: &str) -> io::Result<HostOutcome> {
    write_fail(wr, msg).await?;
    Ok(HostOutcome::Done)
}

pub(crate) async fn handle(world: &World, req: &str, wr: &mut OwnedWriteHalf) -> io::Result<HostOutcome> {
    let Some((selector, cmd)) = parse(req) else {
        world.transcript.warning(format!("unknown host service {req:?}"));
        return fail(wr, "unknown host service").await;
    };
    if selector.is_none() {
        match cmd {
            "version" => return reply(wr, &format!("{:04x}", world.adb_version)).await,
            "devices" => return reply(wr, &world.device_list(false)).await,
            "devices-l" => return reply(wr, &world.device_list(true)).await,
            "track-devices" => return Ok(HostOutcome::Track { long: false }),
            "track-devices-l" => return Ok(HostOutcome::Track { long: true }),
            "host-features" => return reply(wr, &world.host_features.join(",")).await,
            "kill" => {
                write_okay(wr).await?;
                world.transcript.record(
                    json!({"type": "event", "event": "kill", "behaviour": match world.on_kill {
                        KillBehaviour::Exit => "exit",
                        KillBehaviour::Ignore => "ignore",
                    }}),
                );
                if world.on_kill == KillBehaviour::Exit {
                    // The receiver is gone only once the server is already stopping.
                    let _ = world.kill.send(KillRequest::Exit);
                }
                return Ok(HostOutcome::Done);
            }
            _ => {}
        }
        // `transport:` and `tport:` switch this connection to a device. `tport:` (newer clients) also
        // answers with the 8-byte little-endian transport id after OKAY.
        let (switch, tport) = if let Some(s) = cmd.strip_prefix("tport:") {
            let sel = match s {
                "any" => Some(Selector::Any),
                "usb" => Some(Selector::Usb),
                "local" => Some(Selector::Local),
                s => s.strip_prefix("serial:").map(|x| Selector::Serial(x.to_string())),
            };
            (sel, true)
        } else {
            let sel = match cmd {
                "transport-any" => Some(Selector::Any),
                "transport-usb" => Some(Selector::Usb),
                "transport-local" => Some(Selector::Local),
                c => c
                    .strip_prefix("transport:")
                    .map(|s| Selector::Serial(s.to_string()))
                    .or_else(|| {
                        c.strip_prefix("transport-id:")
                            .and_then(|id| id.parse().ok())
                            .map(Selector::Id)
                    }),
            };
            (sel, false)
        };
        if let Some(sel) = switch {
            return match world.resolve(&sel, true) {
                Ok(t) => {
                    write_okay(wr).await?;
                    if tport {
                        wr.write_all(&t.transport_id.to_le_bytes()).await?;
                    }
                    Ok(HostOutcome::Transport(t))
                }
                Err(m) => fail(wr, &m).await,
            };
        }
    }
    if !DEVICE_COMMANDS.contains(&cmd) {
        world.transcript.warning(format!("unknown host service {req:?}"));
        return fail(wr, "unknown host service").await;
    }
    let t = match world.resolve(selector.as_ref().unwrap_or(&Selector::Any), false) {
        Ok(t) => t,
        Err(m) => return fail(wr, &m).await,
    };
    let body = world
        .with_device(&t.serial, |d| match cmd {
            "features" => d.features.join(","),
            "get-state" => d.state.clone(),
            "get-serialno" => d.serial.clone(),
            _ => "unknown".to_string(),
        })
        .unwrap_or_default();
    reply(wr, &body).await
}

/// Streams the device list: immediately, then after every change, until the client goes away. The
/// client never sends anything, so its EOF is watched alongside the device updates.
pub(crate) async fn track(
    world: &World,
    long: bool,
    rd: &mut OwnedReadHalf,
    wr: &mut OwnedWriteHalf,
) -> io::Result<()> {
    let mut rx = world.devices_changed.subscribe();
    write_okay(wr).await?;
    let mut last: Option<String> = None;
    loop {
        rx.mark_unchanged();
        let list = world.device_list(long);
        if last.as_ref() != Some(&list) {
            // An output stream like a command's: a client that has gone is not an error.
            wr.write_all(&protocol_string(list.as_bytes()))
                .await
                .map_err(super::device::output_cancelled)?;
            last = Some(list);
        }
        let mut buf = [0u8; 256];
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
            read = rd.read(&mut buf) => {
                if matches!(read, Ok(0) | Err(_)) {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_requests() {
        assert_eq!(parse("host:version"), Some((None, "version")));
        assert_eq!(
            parse("host-serial:127.0.0.1:5555:features"),
            Some((Some(Selector::Serial("127.0.0.1:5555".into())), "features"))
        );
        assert_eq!(
            parse("host-serial:emulator-5554:get-state"),
            Some((Some(Selector::Serial("emulator-5554".into())), "get-state"))
        );
        assert_eq!(
            parse("host-transport-id:3:features"),
            Some((Some(Selector::Id(3)), "features"))
        );
        assert_eq!(
            parse("host-usb:get-serialno"),
            Some((Some(Selector::Usb), "get-serialno"))
        );
        assert_eq!(parse("host-serial:x:bogus"), None);
    }
}
