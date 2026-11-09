//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/timeline/TimelineSummaryProvider.kt.
mod report_probe_support;
use jcompat::logging::MarathonLogging;
use marathon_core::{executor::CoreExecutor, report::Reporter, reporters::timeline::*};
use report_probe_support::*;
#[test]
fn jvm_timeline_bytes_and_empty_means() {
    assert_eq!(
        json(&checked(TimelineSummaryProvider::generate(&mixed()))),
        checked(std::fs::read(fixture("timeline.json")))
    );
    assert_eq!(
        json(&checked(TimelineSummaryProvider::generate(&empty()))),
        checked(std::fs::read(fixture("timeline-empty.json")))
    );
}
#[test]
fn writes_exact_resources_and_template_even_for_empty_report() {
    for (report, oracle) in [(mixed(), "timeline.json"), (empty(), "timeline-empty.json")] {
        let temp = Temp::new();
        let root = temp.path("");
        checked(CoreExecutor::run(move |core| {
            Box::pin(async move {
                TimelineReporter::new(core.vendor(), root, &mut MarathonLogging::default())
                    .generate(&report)
                    .await
            })
        }));
        let template = checked(std::fs::read_to_string(fixture("manifest/html/timeline/index.html")));
        let original = checked(std::fs::read_to_string(fixture("timeline.json")));
        let expected = template.replace(&original, &checked(std::fs::read_to_string(fixture(oracle))));
        assert_eq!(
            checked(std::fs::read_to_string(temp.0.join("html/timeline/index.html"))),
            expected
        );
        for name in ["chart.css", "chart.js", "iframeResizer.contentWindow.min.js"] {
            assert_eq!(
                checked(std::fs::read(temp.0.join("html/timeline").join(name))),
                checked(std::fs::read(fixture(&format!("manifest/html/timeline/{name}"))))
            )
        }
    }
}
#[test]
fn overflowing_preparation_instant_is_arithmetic_exception() {
    use marathon_core::report::{DeviceProviderPreparingEvent, ExecutionReport};
    // Instant.MAX's seconds exceed Long.MAX_VALUE milliseconds.
    let huge = checked(jcompat::jtime::instant_of(31_556_889_864_403_199, 0).ok_or("instant"));
    for finish in [huge, epoch(1)] {
        let report = ExecutionReport::new(
            vec![],
            vec![],
            vec![],
            vec![DeviceProviderPreparingEvent {
                start: if finish == huge { epoch(0) } else { huge },
                finish,
                serial_number: "provider-only".into(),
            }],
            vec![],
        );
        let error = TimelineSummaryProvider::generate(&report).err().unwrap();
        assert_eq!(
            error.failure().to_string().to_owned(),
            "arithmetic failure: long overflow"
        );
    }
}
