//! Port of core/src/main/kotlin/com/malinskiy/marathon/time/SystemTimer.kt
//! and WP 0.2b tools/oracle/hooks/OracleHooks.kt and Barriers.kt (replay seams).
use super::clock::Timeline;
use crate::time::*;
use crossbeam_skiplist::SkipMap;
use jcompat::jtime::{Instant, instant_of};
use std::{
    collections::HashMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;

#[derive(Clone, Copy)]
pub enum ClockMode {
    Real,
    Frozen(i64),
    Scheduler(i64),
    Step { start: i64, step: i64 },
}
#[derive(Clone)]
pub enum Draw {
    Int(i32),
    Key(String),
}
#[derive(Clone)]
pub enum Ids {
    Values(Vec<String>),
    Pattern {
        prefix: String,
        width: usize,
        hex: bool,
        suffix: String,
        start: i64,
    },
}
/// Typed tape data; JSON validation belongs to 3.10, not the runtime API.
pub struct Tape {
    pub wall: ClockMode,
    pub monotonic: ClockMode,
    pub reads: HashMap<String, Vec<i64>>,
    pub draws: HashMap<String, Vec<Draw>>,
    pub ids: HashMap<String, Ids>,
    pub barriers: Vec<String>,
}
impl Default for Tape {
    fn default() -> Self {
        Self {
            wall: ClockMode::Real,
            monotonic: ClockMode::Real,
            reads: HashMap::new(),
            draws: HashMap::new(),
            ids: HashMap::new(),
            barriers: Vec::new(),
        }
    }
}
#[derive(Default)]
struct Failures {
    first: OnceLock<ReplayError>,
    count: AtomicUsize,
}
impl Failures {
    fn fail<T>(&self, label: &str, message: &str) -> ClockResult<T> {
        let error = ReplayError {
            label: label.into(),
            message: message.into(),
        };
        self.count.fetch_add(1, Ordering::SeqCst);
        let _ = self.first.set(error.clone());
        Err(error)
    }
    fn verify(&self) -> ClockResult<()> {
        self.first.get().cloned().map_or(Ok(()), Err)
    }
}
pub struct RuntimeHooks {
    tape: Option<Tape>,
    cursors: SkipMap<String, AtomicUsize>,
    timeline: Option<Arc<Timeline>>,
    epoch: std::time::Instant,
    failures: Arc<Failures>,
    barriers: Arc<Barriers>,
}
impl RuntimeHooks {
    pub fn live() -> Arc<Self> {
        Self::build(None, None)
    }
    /// No tape: wall, monotonic and identity reads are the host's.
    pub fn is_live(&self) -> bool {
        self.tape.is_none()
    }
    pub fn taped(tape: Tape, clock: &super::RuntimeClock) -> Arc<Self> {
        Self::build(Some(tape), Some(clock.timeline.clone()))
    }
    fn build(tape: Option<Tape>, timeline: Option<Arc<Timeline>>) -> Arc<Self> {
        let failures = Arc::new(Failures::default());
        let barriers = Arc::new(Barriers::new(
            tape.as_ref().map(|t| t.barriers.clone()).unwrap_or_default(),
            failures.clone(),
        ));
        Arc::new(Self {
            tape,
            cursors: SkipMap::new(),
            timeline,
            epoch: std::time::Instant::now(),
            failures,
            barriers,
        })
    }
    pub fn barriers(&self) -> Arc<dyn ReplayBarriers> {
        Arc::new(self.barriers.clone())
    }
    fn count(&self, kind: &str, label: &str) -> usize {
        self.cursors
            .get_or_insert(format!("{kind}:{label}"), AtomicUsize::new(0))
            .value()
            .fetch_add(1, Ordering::SeqCst)
    }
    fn used(&self, kind: &str, label: &str) -> usize {
        self.cursors
            .get(&format!("{kind}:{label}"))
            .map_or(0, |v| v.value().load(Ordering::SeqCst))
    }
    fn host_millis(&self) -> i64 {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            Err(e) => -(e.duration().as_millis() as i64),
        }
    }
    fn read(&self, label: &str, wall: bool) -> ClockResult<i64> {
        let Some(tape) = &self.tape else {
            return Ok(if wall {
                self.host_millis()
            } else {
                self.epoch.elapsed().as_nanos() as i64
            });
        };
        if let Some(values) = tape.reads.get(label) {
            return values
                .get(self.count("read", label))
                .copied()
                .map_or_else(|| self.failures.fail(label, "read tape exhausted"), Ok);
        }
        let mode = if wall { tape.wall } else { tape.monotonic };
        let count = self.count("clock", label) as i64;
        match mode {
            ClockMode::Real => Ok(if wall {
                self.host_millis()
            } else {
                self.epoch.elapsed().as_nanos() as i64
            }),
            ClockMode::Frozen(start) => Ok(start),
            ClockMode::Step { start, step } => Ok(start.wrapping_add(count.wrapping_mul(step))),
            ClockMode::Scheduler(start) => match &self.timeline {
                Some(t) if t.driver.load(Ordering::SeqCst) => {
                    Ok(start.wrapping_add(t.now().wrapping_mul(if wall { 1 } else { 1_000_000 })))
                }
                _ => self.failures.fail(label, "scheduler clock requires a ReplayDriver"),
            },
        }
    }
    fn draw(&self, label: &str) -> ClockResult<&Draw> {
        let values = self.tape.as_ref().and_then(|t| t.draws.get(label));
        values
            .and_then(|v| v.get(self.count("draw", label)))
            .map_or_else(|| self.failures.fail(label, "draw tape missing or exhausted"), Ok)
    }
}
impl WallClock for RuntimeHooks {
    fn millis(&self, label: &'static str) -> ClockResult<i64> {
        self.read(label, true)
    }
    fn instant(&self, label: &'static str) -> ClockResult<Instant> {
        let millis = self.millis(label)?;
        instant_of(millis.div_euclid(1000), millis.rem_euclid(1000) * 1_000_000).ok_or_else(|| ReplayError {
            label: label.into(),
            message: "instant out of range".into(),
        })
    }
}
impl MonotonicClock for RuntimeHooks {
    fn nanos(&self, label: &'static str) -> ClockResult<i64> {
        self.read(label, false)
    }
}
impl ReplayHooks for RuntimeHooks {
    fn draw_int(&self, label: &'static str) -> ClockResult<i32> {
        if self.tape.is_none() {
            return Ok(rand::random());
        }
        match self.draw(label)? {
            Draw::Int(value) => Ok(*value),
            _ => self.failures.fail(label, "draw is not a signed i32"),
        }
    }
    fn choose_key(&self, label: &'static str, candidates: &[String]) -> ClockResult<String> {
        if self.tape.is_none() {
            return candidates
                .get(if candidates.is_empty() {
                    0
                } else {
                    rand::random_range(0..candidates.len())
                })
                .cloned()
                .map_or_else(|| self.failures.fail(label, "empty candidates"), Ok);
        }
        match self.draw(label)? {
            Draw::Key(key) if candidates.contains(key) => Ok(key.clone()),
            _ => self.failures.fail(label, "draw does not name a candidate"),
        }
    }
    fn allocate_id(&self, label: &'static str) -> ClockResult<String> {
        if self.tape.is_none() {
            let id: u128 = rand::random();
            let id = (id & !(0xf000_u128 << 64) & !(0xc000_u128 << 48)) | (0x4000_u128 << 64) | (0x8000_u128 << 48);
            let raw = format!("{id:032x}");
            return Ok(format!(
                "{}-{}-{}-{}-{}",
                &raw[..8],
                &raw[8..12],
                &raw[12..16],
                &raw[16..20],
                &raw[20..]
            ));
        }
        let index = self.count("id", label);
        match self.tape.as_ref().and_then(|t| t.ids.get(label)) {
            Some(Ids::Values(values)) => values
                .get(index)
                .cloned()
                .map_or_else(|| self.failures.fail(label, "ID tape exhausted"), Ok),
            Some(Ids::Pattern {
                prefix,
                width,
                hex,
                suffix,
                start,
            }) => {
                let value = start.wrapping_add(index as i64);
                Ok(if *hex {
                    format!("{prefix}{value:0width$x}{suffix}")
                } else {
                    format!("{prefix}{value:0width$}{suffix}")
                })
            }
            None => self.failures.fail(label, "ID tape missing"),
        }
    }
    fn verify_consumed(&self) -> ClockResult<()> {
        self.failures.verify()?;
        if let Some(tape) = &self.tape {
            for (label, values) in &tape.reads {
                if self.used("read", label) != values.len() {
                    return self.failures.fail(label, "unused read entries");
                }
            }
            for (label, values) in &tape.draws {
                if self.used("draw", label) != values.len() {
                    return self.failures.fail(label, "unused draw entries");
                }
            }
            for (label, values) in &tape.ids {
                if let Ids::Values(values) = values
                    && self.used("id", label) != values.len()
                {
                    return self.failures.fail(label, "unused ID entries");
                }
            }
        }
        self.barriers.verify()
    }
}
struct Barriers {
    script: Vec<String>,
    states: Vec<AtomicU8>,
    cursor: AtomicUsize,
    changed: Notify,
    failures: Arc<Failures>,
}
impl Barriers {
    fn new(script: Vec<String>, failures: Arc<Failures>) -> Self {
        let states = script.iter().map(|_| AtomicU8::new(0)).collect();
        Self {
            script,
            states,
            cursor: AtomicUsize::new(0),
            changed: Notify::new(),
            failures,
        }
    }
    fn verify(&self) -> ClockResult<()> {
        self.failures.verify()?;
        if self.cursor.load(Ordering::SeqCst) != self.script.len() {
            self.failures.fail("barriers", "unused barrier entries")
        } else {
            Ok(())
        }
    }
}
struct Waiting<'a> {
    barriers: &'a Barriers,
    index: usize,
}
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let _ = self.barriers.states[self.index].compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}
struct NoopPermit;
impl BarrierPermit for NoopPermit {
    fn finish(self: Box<Self>) -> ClockResult<()> {
        Ok(())
    }
}
struct Permit {
    barriers: Arc<Barriers>,
    index: usize,
}
impl BarrierPermit for Permit {
    fn finish(self: Box<Self>) -> ClockResult<()> {
        if self.barriers.states[self.index]
            .compare_exchange(2, 3, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return self.barriers.failures.fail("barriers", "permit already released");
        }
        self.barriers.cursor.fetch_add(1, Ordering::SeqCst);
        self.barriers.changed.notify_waiters();
        Ok(())
    }
}
impl ReplayBarriers for Arc<Barriers> {
    fn enter<'a>(&'a self, label: &'static str, key: &'a str) -> BarrierFuture<'a> {
        Box::pin(async move {
            if self.script.is_empty() {
                return Ok(Box::new(NoopPermit) as Box<dyn BarrierPermit>);
            }
            let token = format!("{label}:{}", key.to_owned());
            let cursor = self.cursor.load(Ordering::SeqCst);
            let Some(index) = self
                .script
                .iter()
                .enumerate()
                .skip(cursor)
                .find_map(|(i, v)| (v == &token).then_some(i))
            else {
                return self.failures.fail(label, "unexpected or exhausted barrier");
            };
            if self.states[index]
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return self.failures.fail(label, "duplicate pending barrier");
            }
            let _waiting = Waiting { barriers: self, index };
            loop {
                let changed = self.changed.notified();
                let mut changed = std::pin::pin!(changed);
                changed.as_mut().enable();
                if self.cursor.load(Ordering::SeqCst) == index {
                    self.states[index].store(2, Ordering::SeqCst);
                    return Ok(Box::new(Permit {
                        barriers: self.clone(),
                        index,
                    }) as Box<dyn BarrierPermit>);
                }
                changed.await;
            }
        })
    }
}
