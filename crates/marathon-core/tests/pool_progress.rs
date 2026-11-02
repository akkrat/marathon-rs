//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/progress/PoolProgressAccumulatorTest.kt
#[path = "support/progress.rs"]
mod support;
use marathon_config::schema::ExecutionMode::*;
use marathon_core::model::*;
use support::*;

#[test]
fn any_success_should_pass_on_pass_failure_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_pass_on_pass_pass_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_pass_on_pass_fail_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.end(result(&test, &device, "3", TestStatus::Failure, 2, 3), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_fail_on_no_events() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, false);
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_pass_on_retry_fail_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 1], AnySuccess, false);
            h.start(&device, &test).await;
            h.retry(&test);
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_fast_should_pass_on_pass_fail_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 1], AnySuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_fast_should_fail_on_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 1], AnySuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_fast_should_fail_on_fail_with_many_tries() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_pass_on_pass_incomplete_incomplete() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Incomplete, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Incomplete, 2, 3), true)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_pass_on_pass_retry_fail_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 2], AnySuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(h.aggregate());
            h.start(&device, &test).await;
            h.retry(&test);
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_fail_on_fail_fail_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Failure, 2, 3), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_pass_on_fail_pass_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_fast_should_pass_on_fail_pass_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn any_success_should_fail_on_add_retry_fail_add_retry_fail_remove_retry_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 2], AnySuccess, false);
            h.start(&device, &test).await;
            h.retry(&test);
            assert!(!h.aggregate());
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.retry(&test);
            assert!(!h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.remove(&test, 1);
            assert!(!h.aggregate());
            h.end(result(&test, &device, "3", TestStatus::Failure, 2, 3), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn should_fail_on_incomplete_for_a_bunch_of_test_tries_without_a_single_pass_or_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AnySuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Incomplete, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Incomplete, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Incomplete, 2, 3), true)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn should_fail_on_fail_incomplete_incomplete() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Incomplete, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Incomplete, 2, 3), true)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn should_not_change_result_after_terminating_to_failed() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 1], AnySuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Incomplete, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.retry(&test);
            assert!(!h.aggregate());
            h.remove(&test, 1);
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn should_not_change_result_after_terminating_to_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 1], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Incomplete, 1, 2), false)
                .await;
            assert!(h.aggregate());
            h.retry(&test);
            assert!(h.aggregate());
            h.remove(&test, 1);
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_passing_with_multiple_tries_passing() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_fail_on_pass_pass_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.end(result(&test, &device, "3", TestStatus::Failure, 2, 3), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_fast_should_fail_on_pass_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_fail_on_pass_fail_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_pass_on_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 1], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_fail_on_fail_with_many_test_tries() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_fail_on_first_failure_for_a_single_test() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 1], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_fast_should_fail_on_failure_for_a_many_test_tries() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_fail_on_retry_pass_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 1], AllSuccess, true);
            h.start(&device, &test).await;
            h.retry(&test);
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_pass_on_remove_retry_pass_pass_with_3_test_tries() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 4], AllSuccess, true);
            h.remove(&test, 1);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            let device2 = support::device("2");
            h.start(&device, &test).await;
            h.remove(&test, 1);
            assert!(!h.aggregate());
            h.end(result(&test, &device2, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_fail_on_pass_incomplete_incomplete() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Passed, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Incomplete, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Incomplete, 2, 3), true)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_fail_on_fail_pass_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_fast_should_fail_on_fail_pass_pass() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Passed, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Passed, 2, 3), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_should_fail_on_fail_fail_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, false);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Failure, 2, 3), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn all_success_fast_should_fail_on_fail_fail_fail() {
    run(|core| {
        Box::pin(async move {
            let test = test("method");
            let device = device("1");
            let mut h = Harness::new(&core, vec![test.clone(); 3], AllSuccess, true);
            h.start(&device, &test).await;
            h.end(result(&test, &device, "1", TestStatus::Failure, 0, 1), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "2", TestStatus::Failure, 1, 2), false)
                .await;
            assert!(!h.aggregate());
            h.start(&device, &test).await;
            h.end(result(&test, &device, "3", TestStatus::Failure, 2, 3), false)
                .await;
            assert!(!h.aggregate());
            Ok(())
        })
    });
}

#[test]
fn should_report_progress_for_one_pool_with_runtime_discovery() {
    // Kotlin P:775 contains only commented-out obsolete calls.
}

#[test]
fn any_success_should_pass_with_runtime_discovery() {
    run(|core| {
        Box::pin(async move {
            let base = test("test");
            let t0 = test("test[0]");
            let t1 = test("test[1]");
            let d = device("1");
            let mut h = Harness::new(&core, vec![base.clone()], AnySuccess, false);
            h.start(&d, &t0).await;
            h.end(result(&t0, &d, "1", TestStatus::Passed, 0, 1), false).await;
            assert!(!h.aggregate());
            h.start(&d, &t1).await;
            h.end(result(&t1, &d, "1", TestStatus::Passed, 1, 2), false).await;
            assert!(!h.aggregate());
            h.end(result(&base, &d, "1", TestStatus::Passed, 1, 2), false).await;
            assert!(h.aggregate());
            Ok(())
        })
    });
}
#[test]
fn should_report_progress() {
    run(|core| {
        Box::pin(async move {
            let a = test("method1");
            let b = test("method2");
            let c = test("method3");
            let d = device("1");
            let mut h = Harness::new(&core, vec![a.clone(), b.clone(), c.clone()], AnySuccess, false);
            assert_eq!(h.reporter.progress(), 0.0);
            h.start(&d, &a).await;
            h.end(result(&a, &d, "1", TestStatus::Passed, 0, 1), false).await;
            assert_eq!(h.reporter.progress(), 1.0 / 3.0);
            h.start(&d, &b).await;
            for _ in 0..4 {
                assert_eq!(h.retry(&b), None);
            }
            h.end(result(&b, &d, "2", TestStatus::Failure, 1, 2), false).await;
            assert_eq!(h.reporter.progress(), 2.0 / 7.0);
            for (status, batch, start, end, done) in [
                (TestStatus::Passed, "2", 2, 3, 3.0),
                (TestStatus::Failure, "3", 3, 4, 4.0),
                (TestStatus::Ignored, "4", 4, 5, 5.0),
            ] {
                h.start(&d, &b).await;
                h.end(result(&b, &d, batch, status, start, end), false).await;
                assert_eq!(h.reporter.progress(), done / 7.0);
            }
            h.remove(&b, 1);
            assert_eq!(h.reporter.progress(), 5.0 / 6.0);
            h.start(&d, &c).await;
            h.end(result(&c, &d, "5", TestStatus::Ignored, 5, 6), false).await;
            assert_eq!(h.reporter.progress(), 6.0 / 6.0);
            Ok(())
        })
    });
}
