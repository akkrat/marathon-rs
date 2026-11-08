//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/progress/PoolProgressAccumulator.kt
//! and execution/queue/TestState.kt, TestEvent.kt.
use crate::{
    ci::Ci,
    error::{VendorError, VendorResult},
    executor::{bridge::call_vendor, shared::failure},
    logging::LogQueue,
    model::{DeviceInfo, DevicePoolId, Test, TestAction, TestResult, TestShard, TestStatus},
    output::OutputDriver,
    runtime::CancellationSignal,
    state_machine::{StateMachine, Transition},
    strategy::retry::RetryProgress,
    time::SchedulerClock,
    wiring::Tracker,
};
use jcompat::{formatting::format_percent, logging::Level};
use marathon_config::schema::{ExecutionMode, ExecutionStrategyConfiguration};
use std::{collections::HashMap, rc::Rc, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestState {
    Added { total: i32, running: i32 },
    Passing { total: i32, running: i32, done: i32 },
    Failing { total: i32, running: i32, done: i32 },
    Failed { total: i32, running: i32, done: i32 },
    Passed { total: i32, running: i32, done: i32 },
}
impl TestState {
    pub fn counters(self) -> (i32, i32, i32) {
        match self {
            Self::Added { total, running } => (total, 0, running),
            Self::Passing { total, running, done }
            | Self::Failing { total, running, done }
            | Self::Passed { total, running, done }
            | Self::Failed { total, running, done } => (total, done, running),
        }
    }
    fn terminal(self) -> bool {
        matches!(self, Self::Passed { .. } | Self::Failed { .. })
    }
    fn with_counters(self, total: i32, done: i32, running: i32) -> Self {
        match self {
            Self::Added { .. } => Self::Added { total, running },
            Self::Passing { .. } => Self::Passing { total, done, running },
            Self::Failing { .. } => Self::Failing { total, done, running },
            Self::Passed { .. } => Self::Passed { total, done, running },
            Self::Failed { .. } => Self::Failed { total, done, running },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestEvent {
    Started,
    Passed {
        device: DeviceInfo,
        result: TestResult,
    },
    Failed {
        device: DeviceInfo,
        result: TestResult,
    },
    Incomplete {
        device: DeviceInfo,
        result: TestResult,
        final_result: bool,
    },
    AddRetry,
    RemoveAttempts(i32),
}
fn advance(
    state: TestState,
    event: &TestEvent,
    strategy: &ExecutionStrategyConfiguration,
) -> (TestState, Option<TestAction>) {
    use TestState::*;
    let (total, done, running) = state.counters();
    let any = strategy.mode == ExecutionMode::AnySuccess;
    let (next, complete) = match event {
        TestEvent::Started => (state.with_counters(total, done, running.wrapping_add(1)), false),
        TestEvent::RemoveAttempts(count) => (state.with_counters(total.wrapping_sub(*count), done, running), false),
        TestEvent::AddRetry if state.terminal() => (state, true),
        TestEvent::AddRetry => (state.with_counters(total.wrapping_add(1), done, running), false),
        TestEvent::Incomplete { final_result, .. } => {
            let running = running.wrapping_sub(1);
            if state.terminal() || !final_result {
                (state.with_counters(total, done, running), false)
            } else if matches!(state, Passing { .. }) && any {
                (Passed { total, done, running }, true)
            } else {
                (Failed { total, done, running }, true)
            }
        }
        TestEvent::Passed { .. } | TestEvent::Failed { .. } => {
            let done = done.wrapping_add(1);
            let running = running.wrapping_sub(1);
            if state.terminal() {
                return (state.with_counters(total, done, running), None);
            }
            let passed = matches!(event, TestEvent::Passed { .. });
            let has_pass = passed || matches!(state, Passing { .. });
            let has_fail = !passed || matches!(state, Failing { .. });
            // Fast ANY_SUCCESS waits for a pass; fast ALL_SUCCESS waits for a failure.
            let complete = done >= total || (strategy.fast && if any { has_pass } else { has_fail });
            let success = if any { has_pass } else { !has_fail };
            let next = match (success, complete) {
                (true, true) => Passed { total, done, running },
                (true, false) => Passing { total, done, running },
                (false, true) => Failed { total, done, running },
                (false, false) => Failing { total, done, running },
            };
            (next, complete)
        }
    };
    (next, complete.then_some(TestAction::Complete))
}
fn machine(total: i32, strategy: ExecutionStrategyConfiguration) -> StateMachine<TestState, TestEvent, TestAction> {
    machine_at(TestState::Added { total, running: 0 }, strategy)
}
fn machine_at(
    state: TestState,
    strategy: ExecutionStrategyConfiguration,
) -> StateMachine<TestState, TestEvent, TestAction> {
    StateMachine::create(state, |machine| {
        machine.define(
            |_| true,
            |definition| {
                definition.on(|_| true, move |state, event| advance(*state, event, &strategy));
            },
        );
    })
}
struct TestExecutionData {
    original: Test,
    state: StateMachine<TestState, TestEvent, TestAction>,
}
/// Per-pool serial owner. Every async method must be awaited through completion.
/// RetryProgress remains synchronous because AddRetry has no tracker payload.
pub struct PoolProgressAccumulator {
    pool: DevicePoolId,
    // Insertion order (P3) plus a name index, so each lookup is O(1).
    tests: Vec<(String, TestExecutionData)>,
    index: HashMap<String, usize>,
    strategy: ExecutionStrategyConfiguration,
    ci: Ci,
    tracker: Arc<dyn Tracker>,
    logs: Rc<LogQueue>,
    clock: Rc<dyn SchedulerClock>,
    output: OutputDriver,
}
pub struct ProgressServices {
    pub tracker: Arc<dyn Tracker>,
    pub logs: Rc<LogQueue>,
    pub clock: Rc<dyn SchedulerClock>,
    pub output: OutputDriver,
}
impl PoolProgressAccumulator {
    pub fn new(
        pool: DevicePoolId,
        shard: &TestShard,
        strategy: ExecutionStrategyConfiguration,
        ci: Ci,
        services: ProgressServices,
    ) -> Self {
        // groupBy retains the first equal Test, then putAll overwrites name collisions.
        let mut grouped: Vec<(Test, i32)> = Vec::new();
        let mut group_index: HashMap<Test, usize> = HashMap::new();
        for test in shard.tests.iter().chain(&shard.flaky_tests) {
            if let Some(&at) = group_index.get(test) {
                grouped[at].1 = grouped[at].1.wrapping_add(1);
            } else {
                group_index.insert(test.clone(), grouped.len());
                grouped.push((test.clone(), 1));
            }
        }
        let mut tests: Vec<(String, TestExecutionData)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for (original, count) in grouped {
            let name = original.to_test_name('.', '#');
            let data = TestExecutionData {
                original,
                state: machine(count, strategy.clone()),
            };
            if let Some(&at) = index.get(&name) {
                tests[at].1 = data;
            } else {
                index.insert(name.clone(), tests.len());
                tests.push((name, data));
            }
        }
        Self {
            pool,
            tests,
            index,
            strategy,
            ci,
            tracker: services.tracker,
            logs: services.logs,
            clock: services.clock,
            output: services.output,
        }
    }
    /// Mirrors the oracle's reflection seam for all state/event cells.
    #[cfg(feature = "test-support")]
    pub fn replace_state_for_test(&mut self, test: &Test, state: TestState) {
        let machine = machine_at(state, self.strategy.clone());
        if let Some(data) = self.data_mut(&test.to_test_name('.', '#')) {
            data.state = machine;
        }
    }
    pub fn state(&self, test: &Test) -> Option<TestState> {
        self.data(&test.to_test_name('.', '#')).map(|data| *data.state.state())
    }
    fn data(&self, name: &str) -> Option<&TestExecutionData> {
        self.index.get(name).map(|&at| &self.tests[at].1)
    }
    fn data_mut(&mut self, name: &str) -> Option<&mut TestExecutionData> {
        self.index.get(name).map(|&at| &mut self.tests[at].1)
    }
    fn log(&self, level: Level, prefix: &str, test: &str, suffix: &str) {
        let mut message = String::from(prefix);
        message.push_str(test);
        message.push_str(suffix);
        self.logs.log(
            "com.malinskiy.marathon.execution.progress.PoolProgressAccumulator",
            self.clock.now_millis(),
            level,
            message,
            None,
        );
    }
    fn transition(&mut self, test: &Test, event: TestEvent) -> Option<TestAction> {
        let name = test.to_test_name('.', '#');
        let Some(data) = self.data_mut(&name) else {
            self.log(Level::Warn, "No FSM registered for test ", &name, "");
            return None;
        };
        // The construction defines every state/event pair; no invalid transition is reachable.
        match data.state.transition(event) {
            Ok(Transition::Valid { side_effect, .. }) => side_effect,
            _ => None,
        }
    }
    pub fn remove_test(&mut self, test: &Test, count: i32) -> Option<TestAction> {
        self.transition(test, TestEvent::RemoveAttempts(count))
    }
    pub fn progress(&self) -> f32 {
        let (total, done) = self.tests.iter().fold((0i32, 0i32), |(total, done), (_, data)| {
            let (t, d, _) = data.state.state().counters();
            (total.wrapping_add(t), done.wrapping_add(d))
        });
        done as f32 / total as f32
    }
    pub fn aggregate_result(&self) -> VendorResult<bool> {
        if self.tests.is_empty() {
            return Err(VendorError::Other(failure(
                "java.lang.UnsupportedOperationException",
                "Empty collection can't be reduced.",
            )));
        }
        let mut result = true;
        for (name, data) in &self.tests {
            let success = match data.state.state() {
                TestState::Added { .. } => {
                    self.log(Level::Error, "Expected to run ", name, " but no events received");
                    false
                }
                TestState::Failing { .. } | TestState::Passing { .. } => {
                    self.log(
                        Level::Error,
                        "Expected to run ",
                        name,
                        " more, but no terminal events received",
                    );
                    matches!(data.state.state(), TestState::Passing { .. })
                        && self.strategy.mode == ExecutionMode::AnySuccess
                }
                TestState::Passed { .. } => true,
                TestState::Failed { .. } => false,
            };
            result &= success;
        }
        Ok(result)
    }
    fn line(&self, device: &DeviceInfo, test: &Test, status: &str) -> VendorResult<String> {
        let percent = jcompat::jnum::kotlin_round_to_int(self.progress() * 100.0).ok_or_else(|| {
            VendorError::Other(failure("java.lang.IllegalArgumentException", "Cannot round NaN value."))
        })?;
        let mut line = format_percent(percent);
        line.push_str(" | [");
        line.push_str(&self.pool.name.clone());
        line.push_str("]-[");
        line.push_str(&device.serial_number.clone());
        line.push_str("] ");
        line.push_str(&test.to_test_name('.', '#'));
        line.push(' ');
        line.push_str(status);
        line.push('\n');
        Ok(line)
    }
    pub async fn test_started(
        &mut self,
        device: &DeviceInfo,
        test: &Test,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> VendorResult<()> {
        self.transition(test, TestEvent::Started);
        // C4: this is the fraction's toInt, not a percentage.
        if let Some(line) = self.ci.build_progress(self.progress() as i32) {
            self.logs.raw(line);
        }
        // If roundToInt throws, the preceding CI line has already been printed.
        self.output.flush(&self.logs, cancellation.clone()).await?;
        self.logs.raw(self.line(device, test, "started")?);
        self.output.flush(&self.logs, cancellation).await
    }
    pub async fn test_ended(
        &mut self,
        device: &DeviceInfo,
        result: &TestResult,
        final_result: bool,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> VendorResult<Option<TestAction>> {
        let status = match result.status {
            TestStatus::Failure => "failed",
            TestStatus::Passed => "passed",
            TestStatus::Ignored | TestStatus::AssumptionFailure => "ignored",
            TestStatus::Incomplete => "incomplete",
        };
        self.logs.raw(self.line(device, &result.test, status)?);
        self.output.flush(&self.logs, cancellation.clone()).await?;
        let event = match result.status {
            TestStatus::Failure => TestEvent::Failed {
                device: device.clone(),
                result: result.clone(),
            },
            TestStatus::Incomplete => TestEvent::Incomplete {
                device: device.clone(),
                result: result.clone(),
                final_result,
            },
            _ => TestEvent::Passed {
                device: device.clone(),
                result: result.clone(),
            },
        };
        let action = self.transition(&result.test, event);
        self.output.flush(&self.logs, cancellation.clone()).await?;
        if let Some(data) = self.data(&result.test.to_test_name('.', '#')) {
            let mut result = result.clone();
            result.test = data.original.clone();
            if final_result && result.status == TestStatus::Incomplete {
                result.status = TestStatus::Failure;
            }
            let pool = self.pool.clone();
            let device = device.clone();
            let tracker = self.tracker.clone();
            call_vendor(self.output.vendor.as_ref(), cancellation, move |_| {
                Box::pin(async move {
                    tracker
                        .test(&pool, &device, &result, action == Some(TestAction::Complete))
                        .await
                })
            })
            .await?;
        }
        Ok(action)
    }
}
impl RetryProgress for PoolProgressAccumulator {
    fn retry_test(&mut self, test: &Test) -> Option<TestAction> {
        self.transition(test, TestEvent::AddRetry)
    }
}
