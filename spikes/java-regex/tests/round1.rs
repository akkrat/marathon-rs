use java_regex_spike::{
    backend::{self, CompileError, RunError},
    backends::fancy::JavaRegex,
};
use std::{error::Error, process::Command, time::Instant};

#[test]
fn original_pattern_is_the_serialized_value() -> Result<(), Box<dyn Error>> {
    let source = r"(?iu)\Q(a)\E\p{IsDeseret}";
    let regex = JavaRegex::compile(source).map_err(|e| format!("{e:?}"))?;
    assert_eq!(regex.pattern(), source);
    assert_eq!(regex.to_string(), source);
    assert_eq!(serde_json::to_string(&regex)?, serde_json::to_string(source)?);
    Ok(())
}

#[test]
fn resource_errors_are_aborted() -> Result<(), String> {
    let input = "a".repeat(1_100_000);
    for pattern in [r"(?:(a)|b)*(?=)", r"(a)*(?=)"] {
        let regex = JavaRegex::compile(pattern).map_err(|e| format!("{e:?}"))?;
        assert_eq!(regex.matches(&input), Err(RunError::Aborted));
        if let Err(error) = regex.find(&input) {
            assert_eq!(error, RunError::Aborted);
        }
        if let Err(error) = regex.split(&input, 0) {
            assert_eq!(error, RunError::Aborted);
        }
    }
    Ok(())
}

#[test]
fn java_fast_cases_do_not_exhaust_the_budget() -> Result<(), String> {
    for (pattern, input) in [
        (r"(?:(?=)(a?){22}a{22}|a{22})", "a".repeat(22)),
        (r"(?!(?:a|aa)+$).*", "a".repeat(40) + "!"),
        (r"(?=)(a)*", "a".repeat(1_100_000)),
    ] {
        let regex = JavaRegex::compile(pattern).map_err(|e| format!("{e:?}"))?;
        let start = Instant::now();
        assert_eq!(regex.matches(&input), Ok(true));
        eprintln!("{pattern} matches: {} ms", start.elapsed().as_millis());
        let start = Instant::now();
        let found = regex
            .find(&input)
            .map_err(|e| format!("{e:?}"))?
            .ok_or("missing match")?;
        assert_eq!((found.start(), found.end()), (0, input.len()));
        eprintln!("{pattern} find: {} ms", start.elapsed().as_millis());
    }
    Ok(())
}

#[test]
fn matcher_continuation_differs_from_find_all() -> Result<(), String> {
    let regex = JavaRegex::compile(r"\G").map_err(|e| format!("{e:?}"))?;
    assert_eq!(regex.split("abc", 0), Ok(vec!["".into(), "abc".into()]));
    assert_eq!(regex.split("abc", 3), Ok(vec!["".into(), "abc".into()]));
    assert_eq!(backend::replace(&regex, "abc", "-"), Ok("-abc".into()));
    assert_eq!(backend::find_all(&regex, "abc").map(|v| v.len()), Ok(4));
    Ok(())
}

#[test]
fn d6_uses_the_source_spelling() {
    for (pattern, spelling) in [
        (r"\x{d800}", r"\x{d800}"),
        ("(?ix)(a)\\1 # ignored\nb", r"\1"),
        (r"\uDC00", r"\uDC00"),
        (r"(?i)(?<word>a)\k<word>", r"\k<word>"),
        (r"\Q(\E(?<=\Qa\E+)", r"(?<=\Qa\E+)"),
    ] {
        match JavaRegex::compile(pattern) {
            Err(CompileError::Rejected { construct, .. }) => assert_eq!(construct, spelling),
            _ => panic!("expected D6 for {pattern}"),
        }
    }
}

#[test]
fn hostile_nesting_is_a_compile_error_in_a_subprocess() -> Result<(), Box<dyn Error>> {
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", "hostile_depth_child"])
        .env("MRS_REGEX_DEPTH_CHILD", "1")
        .status()?;
    assert!(status.success(), "child terminated: {status}");
    Ok(())
}

#[test]
fn hostile_depth_child() {
    if std::env::var_os("MRS_REGEX_DEPTH_CHILD").is_none() {
        return;
    }
    for pattern in [
        "(".repeat(100_000) + "a" + &")".repeat(100_000),
        "[".repeat(100_000) + "a" + &"]".repeat(100_000),
    ] {
        assert!(matches!(JavaRegex::compile(&pattern), Err(CompileError::Syntax(_))));
    }
    // TreeInfo must not recurse through the continuation of a wide sequence of optional group branches.
    assert!(matches!(
        JavaRegex::compile(&("(?<=".to_string() + &"(?:a|b)?".repeat(100_000) + ")b")),
        Err(CompileError::Rejected { .. })
    ));
    // Exercise successful parsing, emission and drop too; round2.rs tests the exact backend boundary.
    let regex = JavaRegex::compile(&("(".repeat(40) + "a" + &")".repeat(40)));
    assert!(regex.is_ok());
}
