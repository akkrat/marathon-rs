//! Multiplex token requests over one control exec.

use super::{Stream, protocol_error};
use crate::cmd::remote::ssh::sshj::{channel::ExecChannel, client::SshConnection};
use marathon_core::error::VendorResult;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::{oneshot, watch},
};

const MAX_FRAME: u64 = 32 * 1024;

#[derive(Default)]
struct Requests {
    next: u64,
    pending: HashMap<u64, oneshot::Sender<String>>,
    replies: u64,
    first_miss: Option<u64>,
}

pub(super) struct ControlSession {
    channel: Arc<ExecChannel>,
    requests: Mutex<Requests>,
    closed: watch::Sender<bool>,
}

impl ControlSession {
    pub(super) async fn open(connection: &SshConnection, wire: String) -> VendorResult<Arc<Self>> {
        let channel = connection.start_session().await.map_err(|error| error.into_vendor())?;
        let session = Arc::new(Self {
            channel,
            requests: Mutex::default(),
            closed: watch::channel(false).0,
        });
        session.channel.exec(wire).await.map_err(|error| error.into_vendor())?;
        let stdout = BufReader::new(session.channel.reader(Stream::Stdout));
        let weak = Arc::downgrade(&session);
        connection.runtime.spawn(Box::pin(async move {
            read_replies(stdout, weak).await;
            Ok(())
        }));
        Ok(session)
    }

    pub(super) fn is_open(&self) -> bool {
        !*self.closed.borrow() && self.channel.is_open()
    }

    pub(super) async fn wait_closed(&self) {
        let _ = self.closed.subscribe().wait_for(|closed| *closed).await;
    }

    pub(super) async fn close(&self) -> VendorResult<()> {
        self.channel.close().await.map_err(|error| error.into_vendor())
    }

    pub(super) fn request(self: &Arc<Self>, token: &str, operation: &str) -> VendorResult<Pending> {
        let (send, receive) = oneshot::channel();
        let (id, replies) = {
            let mut requests = self.requests.lock().unwrap_or_else(|error| error.into_inner());
            if !self.is_open() {
                return Err(protocol_error("Worker supervisor control stream is closed"));
            }
            let id = requests
                .next
                .checked_add(1)
                .ok_or_else(|| protocol_error("Control request IDs exhausted"))?;
            requests.next = id;
            requests.pending.insert(id, send);
            (id, requests.replies)
        };
        let request = Pending {
            session: self.clone(),
            id,
            replies,
            receive,
        };
        self.channel
            .write_data(format!("{id} {token}:{operation}\n").into_bytes())
            .map_err(|error| error.into_vendor())?;
        Ok(request)
    }

    fn close_stream(&self) {
        self.closed.send_replace(true);
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pending
            .clear();
        self.channel.request_close();
    }
}

impl Drop for ControlSession {
    fn drop(&mut self) {
        self.channel.request_close();
    }
}

pub(super) struct Pending {
    session: Arc<ControlSession>,
    id: u64,
    replies: u64,
    receive: oneshot::Receiver<String>,
}

impl Pending {
    pub(super) async fn reply(&mut self) -> VendorResult<String> {
        (&mut self.receive)
            .await
            .map_err(|_| protocol_error("Worker supervisor control stream closed before its reply"))
    }

    pub(super) fn timed_out(&self, connection: &SshConnection) {
        let mut requests = self.session.requests.lock().unwrap_or_else(|error| error.into_inner());
        if requests.replies != self.replies {
            return;
        }
        match requests.first_miss {
            // Only a request sent after the first miss can count as the
            // second. Concurrent requests can all share one slow window.
            Some(last_issued) if self.id > last_issued => connection.abandon(),
            Some(_) => {}
            None => requests.first_miss = Some(requests.next),
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.session
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pending
            .remove(&self.id);
    }
}

async fn read_replies(mut stdout: impl tokio::io::AsyncBufRead + Unpin, session: Weak<ControlSession>) {
    let mut frame = String::new();
    loop {
        frame.clear();
        let size = (&mut stdout).take(MAX_FRAME + 1).read_line(&mut frame).await;
        let (Some(active), Ok(size)) = (session.upgrade(), size) else {
            break;
        };
        if size as u64 > MAX_FRAME || !frame.ends_with('\n') {
            break;
        }
        let Some((id, response)) = frame
            .split_once(' ')
            .and_then(|(id, response)| Some((id.parse().ok()?, response)))
        else {
            break;
        };
        let sender = {
            let mut requests = active.requests.lock().unwrap_or_else(|error| error.into_inner());
            if id == 0 || id > requests.next {
                break;
            }
            // A late reply proves liveness even after its caller used the data path.
            requests.replies = requests.replies.wrapping_add(1);
            requests.first_miss = None;
            requests.pending.remove(&id)
        };
        if let Some(sender) = sender {
            let _ = sender.send(response.to_owned());
        }
    }
    if let Some(active) = session.upgrade() {
        active.close_stream();
    }
}
