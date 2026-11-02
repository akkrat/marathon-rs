//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/LineBuffer.kt

use marathon_core::error::VendorResult;
use marathon_core::runtime::VendorFuture;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

type OnLine = dyn Fn(String) -> VendorFuture<'static, ()> + Send + Sync;
pub struct LineBuffer {
    state: Mutex<State>,
    delivery: AsyncMutex<()>,
    on_line: Arc<OnLine>,
}
#[derive(Default)]
struct State {
    text: String,
    pending_utf8: Vec<u8>,
    previous_cr: bool,
}
impl State {
    fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let suffix = if self.previous_cr {
            text.strip_prefix('\n').unwrap_or(text)
        } else {
            text
        };
        self.previous_cr = text.ends_with('\r');
        self.text.push_str(&suffix.replace("\r\n", "\n").replace('\r', "\n"));
    }
    fn decode(&mut self, finish: bool) {
        let bytes = std::mem::take(&mut self.pending_utf8);
        let mut rest = bytes.as_slice();
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    self.append_text(text);
                    break;
                }
                Err(error) => {
                    // The valid prefix is guaranteed by Utf8Error, without unsafe conversion.
                    self.append_text(std::str::from_utf8(&rest[..error.valid_up_to()]).unwrap_or_default());
                    rest = &rest[error.valid_up_to()..];
                    if let Some(length) = error.error_len() {
                        self.append_text("\u{fffd}");
                        rest = &rest[length..];
                    } else {
                        if finish {
                            self.append_text("\u{fffd}");
                        } else {
                            self.pending_utf8.extend_from_slice(rest);
                        }
                        break;
                    }
                }
            }
        }
    }
}
impl LineBuffer {
    pub fn new(on_line: impl Fn(String) -> VendorFuture<'static, ()> + Send + Sync + 'static) -> Self {
        Self {
            state: Mutex::new(State::default()),
            delivery: AsyncMutex::new(()),
            on_line: Arc::new(on_line),
        }
    }
    pub fn append(&self, bytes: &[u8]) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending_utf8.extend_from_slice(bytes);
        state.decode(false);
    }
    pub fn append_count(&self, bytes: &[u8], count: usize) {
        self.append(&bytes[..count]);
    }
    async fn emit(&self, drain: bool) -> VendorResult<()> {
        // Serialize extraction AND callbacks, so concurrent flushes cannot overtake.
        let _delivery = self.delivery.lock().await;
        let lines = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if drain {
                state.decode(true);
                if !state.text.is_empty() && !state.text.ends_with('\n') {
                    state.text.push('\n');
                }
            }
            if let Some(last) = state.text.rfind('\n') {
                let tail = state.text.split_off(last + 1);
                let complete = std::mem::replace(&mut state.text, tail);
                complete[..complete.len() - 1]
                    .split('\n')
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };
        for line in lines {
            (self.on_line)(line).await?;
        }
        Ok(())
    }
    pub async fn flush(&self) -> VendorResult<()> {
        self.emit(false).await
    }
    pub async fn drain(&self) -> VendorResult<()> {
        self.emit(true).await
    }
    pub async fn close(&self) -> VendorResult<()> {
        self.drain().await
    }
}
