//! A scripted `Session.Command` for the suspendFor rows (the Kotlin tests mock it with Mockito).
use marathon_apple::cmd::remote::ssh::sshj::{SshFailure, client::SshFuture, executor::ExitState};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

type JoinAnswer = Box<dyn Fn(usize) -> Result<(), SshFailure> + Send + Sync>;
pub struct Command {
    pub is_open: Box<dyn Fn() -> bool + Send + Sync>,
    pub join: JoinAnswer,
    pub exit_status: Option<i32>,
    pub joins: AtomicUsize,
    pub closes: AtomicUsize,
    pub signal: Mutex<Option<String>>,
}
impl Command {
    pub fn new(is_open: impl Fn() -> bool + Send + Sync + 'static, exit_status: Option<i32>) -> Self {
        Self {
            is_open: Box::new(is_open),
            join: Box::new(|_| Ok(())),
            exit_status,
            joins: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            signal: Mutex::new(None),
        }
    }
}
impl ExitState for Command {
    fn is_open(&self) -> bool {
        (self.is_open)()
    }
    fn join_poll(&self) -> Result<(), SshFailure> {
        let call = self.joins.fetch_add(1, Ordering::SeqCst) + 1;
        (self.join)(call)
    }
    fn close(&self) -> SshFuture<'_, ()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }
    fn exit_signal(&self) -> Option<String> {
        self.signal.lock().unwrap().clone()
    }
}
