//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/result/TestRunResultsAccumulator.kt
//! and execution/result/TemporalTestResult.kt.
use crate::{
    logging::{LogRecord, WallSource},
    model::{DeviceInfo, Test, TestBatch, TestResult, TestStatus},
    time::{ClockResult, Timer, WallClock},
};
use jcompat::{jstring, logging::Level, writers::JavaString};
use std::sync::Arc;

#[derive(Clone, Debug, Default, Eq)]
pub struct Metrics(Vec<(JavaString, JavaString)>);
impl Metrics {
    pub fn entries(&self) -> &[(JavaString, JavaString)] {
        &self.0
    }
    pub fn insert(&mut self, key: JavaString, value: JavaString) {
        if let Some((_, old)) = self.0.iter_mut().find(|(k, _)| *k == key) {
            *old = value;
        } else {
            self.0.push((key, value));
        }
    }
}
impl PartialEq for Metrics {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .all(|(k, v)| other.0.iter().any(|(ok, ov)| k == ok && v == ov))
    }
}
impl FromIterator<(JavaString, JavaString)> for Metrics {
    fn from_iter<T: IntoIterator<Item = (JavaString, JavaString)>>(iter: T) -> Self {
        let mut map = Self::default();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}
impl From<Vec<(JavaString, JavaString)>> for Metrics {
    fn from(entries: Vec<(JavaString, JavaString)>) -> Self {
        entries.into_iter().collect()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalTestResult {
    pub status: TestStatus,
    pub start_time: i64,
    pub end_time: i64,
    pub stack_trace: Option<JavaString>,
    pub metrics: Option<Metrics>,
}
impl TemporalTestResult {
    pub fn new(start_time: i64) -> Self {
        Self {
            status: TestStatus::Incomplete,
            start_time,
            end_time: 0,
            stack_trace: None,
            metrics: None,
        }
    }
    pub fn from_clock(clock: &dyn WallClock) -> ClockResult<Self> {
        Ok(Self::new(clock.millis("core.TemporalTestResult.startTime")?))
    }
    pub fn is_successful(&self) -> bool {
        matches!(
            self.status,
            TestStatus::Passed | TestStatus::Ignored | TestStatus::AssumptionFailure
        )
    }
}
/// LinkedHashMap semantics: replacements keep the original key and position.
/// The entries slice allows Kotlin's mutable temporal values but prevents duplicate keys.
#[derive(Default)]
pub struct TemporalResults(Vec<(Test, TemporalTestResult)>);
impl TemporalResults {
    pub fn entries(&self) -> &[(Test, TemporalTestResult)] {
        &self.0
    }
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut TemporalTestResult> {
        self.0.iter_mut().map(|(_, result)| result)
    }
    pub fn get(&self, test: &Test) -> Option<&TemporalTestResult> {
        self.0.iter().find(|(key, _)| key == test).map(|(_, value)| value)
    }
    pub fn get_mut(&mut self, test: &Test) -> Option<&mut TemporalTestResult> {
        self.0.iter_mut().find(|(key, _)| key == test).map(|(_, value)| value)
    }
    pub fn insert(&mut self, test: Test, result: TemporalTestResult) {
        if let Some(old) = self.get_mut(&test) {
            *old = result;
        } else {
            self.0.push((test, result));
        }
    }
}
/// Serial vendor-side owner; callbacks are synchronous and never do I/O.
/// Adapters deliver `take_logs()` with the shared ordered output after callbacks.
pub struct TestRunResultsAccumulator {
    timer: Arc<dyn Timer>,
    wall: WallSource,
    creation_time: i64,
    name: JavaString,
    pub temporal_test_results: TemporalResults,
    pub run_metrics: Metrics,
    pub is_run_complete: bool,
    count_dirty: bool,
    status_counts: [i32; 5],
    expected_test_count: i32,
    elapsed_time: i64,
    run_failure_message: Option<JavaString>,
    /// D28 (Android): a failure from an earlier run of the same batch, which `test_run_started` does not clear.
    earlier_run_failure: Option<JavaString>,
    logs: Vec<LogRecord>,
}
impl TestRunResultsAccumulator {
    /// `wall` stamps diagnostic records: `logging::host_millis` live, the replay
    /// wall clock under replay (the run's `LogQueue::set_wall_clock` source).
    pub fn new(timer: Arc<dyn Timer>, wall: WallSource) -> ClockResult<Self> {
        let creation_time = timer.current_time_millis()?;
        Ok(Self {
            timer,
            wall,
            creation_time,
            name: "not started".into(),
            temporal_test_results: TemporalResults::default(),
            run_metrics: Metrics::default(),
            is_run_complete: false,
            count_dirty: false,
            status_counts: [0; 5],
            expected_test_count: 0,
            elapsed_time: 0,
            run_failure_message: None,
            earlier_run_failure: None,
            logs: Vec::new(),
        })
    }
    pub fn name(&self) -> &JavaString {
        &self.name
    }
    pub fn elapsed_time(&self) -> i64 {
        self.elapsed_time
    }
    pub fn run_failure_message(&self) -> Option<&JavaString> {
        self.run_failure_message.as_ref()
    }
    pub fn is_run_failure(&self) -> bool {
        self.run_failure_message.is_some()
    }
    pub fn num_tests(&self) -> i32 {
        self.temporal_test_results.0.len() as i32
    }
    pub fn completed_tests(&self) -> Vec<Test> {
        self.temporal_test_results
            .0
            .iter()
            .filter(|(_, r)| r.status != TestStatus::Incomplete)
            .map(|(t, _)| t.clone())
            .collect()
    }
    pub fn num_tests_in_state(&mut self, status: TestStatus) -> i32 {
        if self.count_dirty {
            self.status_counts = [0; 5];
            for (_, result) in &self.temporal_test_results.0 {
                let count = &mut self.status_counts[result.status as usize];
                *count = count.wrapping_add(1);
            }
            // Kotlin never clears isCountDirty, including after recomputation.
        }
        self.status_counts[status as usize]
    }
    pub fn num_complete_tests(&mut self) -> i32 {
        self.num_tests()
            .wrapping_sub(self.num_tests_in_state(TestStatus::Incomplete))
    }
    pub fn num_all_failed_tests(&mut self) -> i32 {
        self.num_tests_in_state(TestStatus::Failure)
            .wrapping_add(self.num_tests_in_state(TestStatus::AssumptionFailure))
    }
    pub fn has_failed_tests(&mut self) -> bool {
        self.num_all_failed_tests() > 0
    }
    /// Another run of the same batch follows: the failure so far stays the text of tests that do not complete.
    pub fn keep_run_failure(&mut self) {
        if let Some(message) = self.run_failure_message.clone() {
            self.earlier_run_failure = Some(message);
        }
    }
    pub fn test_run_started(&mut self, name: JavaString, count: i32) {
        self.name = name;
        self.is_run_complete = false;
        self.run_failure_message = None;
        self.expected_test_count = count;
    }
    fn add(&mut self, test: Test, result: TemporalTestResult) {
        self.count_dirty = true;
        self.temporal_test_results.insert(test, result);
    }
    pub fn test_started(&mut self, test: &Test, start_time: Option<i64>) -> ClockResult<()> {
        let start = match start_time {
            Some(time) => time,
            None => self.timer.current_time_millis()?,
        };
        // P2: omit only the immediately overwritten wall-clock default read.
        self.add(test.clone(), TemporalTestResult::new(start));
        Ok(())
    }
    fn update(
        &mut self,
        test: &Test,
        status: TestStatus,
        trace: Option<&JavaString>,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> ClockResult<()> {
        let mut result = match self.temporal_test_results.get(test) {
            Some(result) => result.clone(),
            None => {
                let mut message = JavaString::from("received test event without test start for ");
                message.append(test.to_safe_test_name());
                // Plain records keep this Send vendor-side owner free of core-local
                // Rc state; adapters forward them without rendering.
                self.logs.push(LogRecord {
                    logger: "com.malinskiy.marathon.execution.result.TestRunResultsAccumulator".to_owned(),
                    at_millis: 0,
                    wall_millis: (self.wall)(),
                    level: Level::Debug,
                    message,
                    cause: None,
                });
                TemporalTestResult::new(self.timer.current_time_millis()?)
            }
        };
        result.status = status;
        if let Some(trace) = trace.filter(|s| !s.0.iter().all(|&c| jstring::is_whitespace(c))) {
            result.stack_trace = Some(trace.clone());
        }
        if let Some(time) = start_time {
            result.start_time = time;
        }
        if let Some(time) = end_time {
            result.end_time = time;
        }
        self.add(test.clone(), result);
        Ok(())
    }
    pub fn test_failed(
        &mut self,
        test: &Test,
        trace: &JavaString,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> ClockResult<()> {
        self.update(test, TestStatus::Failure, Some(trace), start_time, end_time)
    }
    pub fn test_assumption_failure(&mut self, test: &Test, trace: &JavaString) -> ClockResult<()> {
        self.update(test, TestStatus::AssumptionFailure, Some(trace), None, None)
    }
    pub fn test_ignored(&mut self, test: &Test) -> ClockResult<()> {
        self.update(test, TestStatus::Ignored, None, None, None)
    }
    pub fn test_ended(
        &mut self,
        test: &Test,
        metrics: impl Into<Metrics>,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> ClockResult<()> {
        // Default end is evaluated before the missing record's default start.
        let end = match end_time {
            Some(time) => time,
            None => self.timer.current_time_millis()?,
        };
        let mut result = match self.temporal_test_results.get(test) {
            Some(result) => result.clone(),
            None => TemporalTestResult::new(match start_time {
                Some(time) => time,
                None => self.timer.current_time_millis()?,
            }),
        };
        if result.status == TestStatus::Incomplete {
            result.status = TestStatus::Passed;
        }
        result.end_time = end;
        result.metrics = Some(metrics.into());
        self.add(test.clone(), result);
        Ok(())
    }
    fn fill_end_time(&mut self) -> ClockResult<()> {
        for (_, result) in &mut self.temporal_test_results.0 {
            if result.end_time == 0 {
                result.end_time = self.timer.current_time_millis()?;
            }
        }
        Ok(())
    }
    pub fn test_run_failed(&mut self, message: JavaString) -> ClockResult<()> {
        self.run_failure_message = Some(message);
        self.fill_end_time()
    }
    pub fn test_run_stopped(&mut self, elapsed: i64) -> ClockResult<()> {
        self.elapsed_time = self.elapsed_time.wrapping_add(elapsed);
        self.is_run_complete = true;
        self.fill_end_time()
    }
    pub fn test_run_ended(&mut self, elapsed: i64, metrics: impl Into<Metrics>) -> ClockResult<()> {
        for (key, value) in metrics.into().0 {
            self.run_metrics.insert(key, value);
        }
        self.elapsed_time = self.elapsed_time.wrapping_add(elapsed);
        self.is_run_complete = true;
        self.fill_end_time()
    }
    pub fn create_uncompleted_test_results(
        &self,
        tests: &[Test],
        batch: &TestBatch,
        device: &DeviceInfo,
    ) -> ClockResult<Vec<TestResult>> {
        let start = self
            .temporal_test_results
            .0
            .iter()
            .map(|(_, r)| r.end_time)
            .max()
            .unwrap_or(self.creation_time);
        let trace = || {
            self.run_failure_message.clone().or_else(|| self.earlier_run_failure.clone()).unwrap_or_else(|| {
                "Test didn't complete. Either the test results are missing due to timeout or testing runtime crashed"
                    .into()
            })
        };
        let synthesize = |test: &Test| -> ClockResult<TestResult> {
            Ok(TestResult {
                test: test.clone(),
                device: device.clone(),
                test_batch_id: batch.id.clone(),
                status: TestStatus::Incomplete,
                start_time: start,
                end_time: self.timer.current_time_millis()?,
                stacktrace: Some(trace()),
                attachments: Vec::new(),
            })
        };
        tests.iter().map(synthesize).collect()
    }
    pub fn take_logs(&mut self) -> Vec<LogRecord> {
        std::mem::take(&mut self.logs)
    }
}
