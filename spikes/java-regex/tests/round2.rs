use java_regex_spike::{
    backend::{self, CompileError, Compiled},
    backends::fancy::JavaRegex,
};

fn compile(pattern: &str) -> JavaRegex {
    JavaRegex::compile(pattern).unwrap_or_else(|error| panic!("{pattern}: {error:?}"))
}

#[test]
fn negative_captures_and_supplementary_lookbehind_name_the_assertion() {
    for (pattern, spelling) in [
        (r".*?(?<!(a))\1.*", r"(?<!(a))"),
        (r"(?!(a)$).*", r"(?!(a)$)"),
        (r"(?x)(? ! (a) $ ).*", r"(? ! (a) $ )"),
        (r"a.(?<=..)", r"(?<=..)"),
        (r"(?U)a.(?<=\w{2})", r"(?<=\w{2})"),
        (r"(?<=\p{IsDeseret})", r"(?<=\p{IsDeseret})"),
        (r"(?<=[\u0000-\uFFFF]{2})", r"(?<=[\u0000-\uFFFF]{2})"),
    ] {
        match JavaRegex::compile(pattern) {
            Err(CompileError::Rejected { construct, .. }) => assert_eq!(construct, spelling),
            _ => panic!("expected D6 for {pattern}"),
        }
    }
    for pattern in [r"(?<=(a))\1", r"(?<![a-z])b", r"(?<=[\p{L}&&[a-z]])b", r"(?!(?:a)$).*"] {
        compile(pattern);
    }
}

#[test]
fn java_group_study_precedes_fixed_length_policy_even_at_zero() {
    for pattern in [r"(?<=(?:a|b){2})b", r"(?<=(a|b){1})b", r"(?<=(a?){0})b", r"(?<=\1)b"] {
        assert!(
            matches!(JavaRegex::compile(pattern), Err(CompileError::Syntax(_))),
            "{pattern}"
        );
    }
    for pattern in [r"(?<=a+b*)c", r"(?<=(?:a|b){0,1})b"] {
        assert!(
            matches!(JavaRegex::compile(pattern), Err(CompileError::Rejected { .. })),
            "{pattern}"
        );
    }
    assert_eq!(
        compile(r"(?<=(a?){0}+)b").find("ab").map(|m| m.map(|m| m.start())),
        Ok(Some(1))
    );
    assert_eq!(
        compile(r"(?<=\R{0})b").find("b").map(|m| m.map(|m| m.start())),
        Ok(Some(0))
    );
}

#[test]
fn negative_dollar_rewrite_preserves_crlf_pairing() {
    for anchor in ["$", r"\Z"] {
        let pattern = format!(r"\r(?!a*{anchor})\n");
        assert_eq!(compile(&pattern).matches("\r\n"), Ok(true));
        let pattern = format!(r"(?!a*{anchor}).*");
        let found = compile(&pattern).find("\r\n").unwrap().unwrap();
        assert_eq!((found.start(), found.end()), (1, 1));
        assert_eq!(compile(&pattern).matches("a\n"), Ok(false));
        assert_eq!(compile(&pattern).matches("\ra\n"), Ok(false));
    }
}

#[test]
fn linebreak_quantifiers_keep_java_node_choices() {
    for suffix in ["?", "??", "?+", "{0,1}", "{0,1}?", "{0,1}+"] {
        let pattern = format!(r"\R{suffix}\n");
        assert_eq!(compile(&pattern).matches("\r\n"), Ok(false), "{pattern}");
        let found = compile(&pattern).find("\r\n").unwrap();
        assert_eq!(
            found.map(|m| (m.start(), m.end())),
            if suffix.ends_with('+') { None } else { Some((1, 2)) },
            "{pattern}"
        );
    }
    for suffix in ["?", "??", "{0,1}", "{0,1}?"] {
        let pattern = format!(r"(?:\R){suffix}\n");
        assert_eq!(compile(&pattern).matches("\r\n"), Ok(true), "{pattern}");
    }
    for suffix in ["{1}", "{1}?", "{1}+"] {
        let pattern = format!(r"(?:\R){suffix}\n");
        assert_eq!(compile(&pattern).matches("\r\n"), Ok(false), "{pattern}");
        assert_eq!(compile(&pattern).find("\r\n"), Ok(None), "{pattern}");
    }
}

#[test]
fn empty_repeats_preserve_observable_captures() {
    for pattern in [r"(?:){2}", r"(?:(?:)){2}", r"(?:|){2}"] {
        assert_eq!(compile(pattern).matches(""), Ok(true));
        assert_eq!(compile(pattern).find("").unwrap().unwrap().groups, vec![Some((0, 0))]);
    }
    assert_eq!(compile(r"(){2}\1").matches(""), Ok(true));
    assert_eq!(
        compile(r"(a?){0}+").find("").unwrap().unwrap().groups,
        vec![Some((0, 0)), None]
    );
}

#[test]
fn named_characters_use_jdk_literals_and_folding() {
    assert_eq!(compile(r"\N{LATIN SMALL LETTER A}").matches("a"), Ok(true));
    assert_eq!(compile(r"(?i)\N{LATIN SMALL LETTER A}").matches("A"), Ok(true));
    assert_eq!(
        compile(r"(?iu)\N{DESERET CAPITAL LETTER LONG I}").matches("𐐨"),
        Ok(true)
    );
    assert_eq!(
        compile(r"[\N{LATIN SMALL LETTER A}-\N{LATIN SMALL LETTER Z}]").matches("z"),
        Ok(true)
    );
    assert!(matches!(
        JavaRegex::compile(r"\N{HIGH SURROGATES D800}"),
        Err(CompileError::Rejected { .. })
    ));
}

#[test]
fn malformed_syntax_wins_over_every_d6_family() {
    for pattern in [
        r"\X",
        r"\b{g}",
        r"(?<=a+)",
        r"(?!(a))",
        r"(?<=.)",
        r"(?i)(a)\1",
        r"\uD800",
        r"\N{LATIN SMALL LETTER A}",
    ] {
        for suffix in ["(", "[", ")", r"\p{Bogus}"] {
            let pattern = format!("{pattern}{suffix}");
            assert!(
                matches!(JavaRegex::compile(&pattern), Err(CompileError::Syntax(_))),
                "{pattern}"
            );
        }
    }
}

#[test]
fn scalar_boundary_policy_is_consistent_through_wrappers_and_operations() {
    for pattern in [r"\B", r"(?:\B)", r"(?U)\B"] {
        let regex = compile(pattern);
        assert_eq!(regex.find("a😀").map(|m| m.map(|m| m.start())), Ok(Some(5)));
        assert_eq!(
            backend::find_all(&regex, "😀").map(|v| v.iter().map(|m| m.start()).collect::<Vec<_>>()),
            Ok(vec![0, 4])
        );
        assert_eq!(regex.split("😀", 0), Ok(vec!["".into(), "😀".into(), "".into()]));
        assert_eq!(backend::replace(&regex, "😀", "-"), Ok("-😀-".into()));
        assert_eq!(regex.find_next("😀", 4, 0).unwrap().unwrap().start(), 4);
    }
    assert_eq!(backend::replace(&compile(r"\G|."), "😀", "-"), Ok("-😀".into()));
}

#[test]
fn actual_backend_depth_boundary_has_a_syntax_error() {
    let pattern = |n| "(".repeat(n) + "a" + &")".repeat(n);
    assert_eq!(compile(&pattern(62)).matches("a"), Ok(true));
    assert!(
        matches!(JavaRegex::compile(&pattern(63)), Err(CompileError::Syntax(ref m)) if m == "Stack overflow during pattern compilation")
    );
}
