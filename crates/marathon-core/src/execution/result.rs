//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/result/TestRunResultsAccumulator.kt
//! and execution/result/TemporalTestResult.kt.
use marathon_logging::Level;

use crate::{
    logging::{LogRecord, WallSource},
    model::{DeviceInfo, Test, TestBatch, TestResult, TestStatus},
    time::{ClockResult, Timer, WallClock},
};
use jcompat::jstring;
use std::sync::Arc;

#[derive(Clone, Debug, Default, Eq)]
pub struct Metrics(Vec<(String, String)>);
impl Metrics {
    pub fn entries(&self) -> &[(String, String)] {
        &self.0
    }
    pub fn insert(&mut self, key: String, value: String) {
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
impl FromIterator<(String, String)> for Metrics {
    fn from_iter<T: IntoIterator<Item = (String, String)>>(iter: T) -> Self {
        let mut map = Self::default();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}
impl From<Vec<(String, String)>> for Metrics {
    fn from(entries: Vec<(String, String)>) -> Self {
        entries.into_iter().collect()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalTestResult {
    pub status: TestStatus,
    pub start_time: i64,
    pub end_time: i64,
    pub stack_trace: Option<String>,
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
/// D28: an instrumentation of a batch that is over, and why it failed, if it did.
struct FinishedInstrumentation {
    tests: Vec<Test>,
    failure: Option<String>,
}
/// Serial vendor-side owner; callbacks are synchronous and never do I/O.
/// Adapters deliver `take_logs()` with the shared ordered output after callbacks.
pub struct TestRunResultsAccumulator {
    timer: Arc<dyn Timer>,
    wall: WallSource,
    creation_time: i64,
    name: String,
    pub temporal_test_results: TemporalResults,
    pub run_metrics: Metrics,
    pub is_run_complete: bool,
    count_dirty: bool,
    status_counts: [i32; 5],
    expected_test_count: i32,
    elapsed_time: i64,
    run_failure_message: Option<String>,
    /// D28 (Android): the instrumentations of this batch that are over. Empty unless the batch has several.
    finished_instrumentations: Vec<FinishedInstrumentation>,
    /// D28 (Android): the tests of the instrumentation running now. Empty unless the batch has several.
    current_instrumentation: Vec<Test>,
    /// D28 (Android): how many tests the batch's earlier instrumentations reported.
    tests_of_earlier_runs: usize,
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
            finished_instrumentations: Vec::new(),
            current_instrumentation: Vec::new(),
            tests_of_earlier_runs: 0,
            logs: Vec::new(),
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn elapsed_time(&self) -> i64 {
        self.elapsed_time
    }
    pub fn run_failure_message(&self) -> Option<&str> {
        self.run_failure_message.as_deref()
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
    /// D28: an instrumentation of a batch of several is about to run these tests, and the one before it is over. Its
    /// failure stays the text of its own tests that do not complete, and of no others.
    pub fn start_instrumentation(&mut self, tests: Vec<Test>) {
        let finished = std::mem::replace(&mut self.current_instrumentation, tests);
        self.finished_instrumentations.push(FinishedInstrumentation {
            tests: finished,
            failure: self.run_failure_message.take(),
        });
        self.tests_of_earlier_runs = self.temporal_test_results.0.len();
    }
    /// A test of an instrumentation that is over has that one's failure. Any other has the current failure: its
    /// instrumentation is the current one, or never ran because the current one ended the batch.
    fn failure_of(&self, test: &Test) -> Option<&str> {
        match self.finished_instrumentations.iter().find(|i| i.tests.contains(test)) {
            Some(finished) => finished.failure.as_deref(),
            None => self.run_failure_message.as_deref(),
        }
    }
    /// D25 (Android): this run's output broke off, and that is why a test in progress never finished. Other run
    /// failures leave such a test without a text, as in Kotlin.
    pub fn explain_tests_in_progress(&mut self, reason: &str) {
        for (_, result) in self.temporal_test_results.0.iter_mut().skip(self.tests_of_earlier_runs) {
            if result.status == TestStatus::Incomplete && result.stack_trace.is_none() {
                result.stack_trace = Some(reason.to_string());
            }
        }
    }
    pub fn test_run_started(&mut self, name: String, count: i32) {
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
        trace: Option<&str>,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> ClockResult<()> {
        let mut result = match self.temporal_test_results.get(test) {
            Some(result) => result.clone(),
            None => {
                let mut message = String::from("received test event without test start for ");
                message.push_str(&test.to_safe_test_name());
                // Plain records keep this Send vendor-side owner free of core-local
                // Rc state; adapters forward them without rendering.
                self.logs.push(LogRecord {
                    target: module_path!().into(),
                    logger: module_path!().into(),
                    at_millis: 0,
                    wall_millis: (self.wall)(),
                    level: Level::DEBUG,
                    message,
                    cause: None,
                });
                TemporalTestResult::new(self.timer.current_time_millis()?)
            }
        };
        result.status = status;
        if let Some(trace) = trace.filter(|s| !s.chars().all(jstring::is_whitespace)) {
            result.stack_trace = Some(trace.to_string());
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
        trace: &str,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> ClockResult<()> {
        self.update(test, TestStatus::Failure, Some(trace), start_time, end_time)
    }
    pub fn test_assumption_failure(&mut self, test: &Test, trace: &str) -> ClockResult<()> {
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
    pub fn test_run_failed(&mut self, message: String) -> ClockResult<()> {
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
        let trace = |test: &Test| {
            self.failure_of(test).map(str::to_owned).unwrap_or_else(|| {
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
                stacktrace: Some(trace(test)),
                attachments: Vec::new(),
            })
        };
        tests.iter().map(synthesize).collect()
    }
    pub fn take_logs(&mut self) -> Vec<LogRecord> {
        std::mem::take(&mut self.logs)
    }
}
