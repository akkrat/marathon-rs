//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/args/CliCommands.kt and
//! cli/src/main/kotlin/com/malinskiy/marathon/cli/args/CliConfigurations.kt.
//!
//! A hand-written parser with the observable behaviour of Clikt 5.0.0 for these four commands (PLAN §3.1;
//! `parsers/ParserInternals.kt`, `internal/Finalization.kt`, `core/exceptions.kt` of Clikt): the token rules, the
//! Jaro-Winkler suggestions, which errors are reported together and under which usage line, and the help texts in
//! the non-terminal 79-column layout (D12). Clikt's `@argfile` expansion and completion script are not reproduced (D12).

use jcompat::file_manager::file_path;
use std::path::PathBuf;

/// The `CliConfiguration` a command hands to `execute`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliConfiguration {
    Run {
        marathonfile: PathBuf,
        analytics_tracking: bool,
        bugsnag_reporting: bool,
    },
    Parse {
        marathonfile: PathBuf,
        output_file_name: Option<String>,
        include_flaky_tests: bool,
    },
    Version,
}

impl CliConfiguration {
    /// Data classes retain their Kotlin rendering; objects use simple names (D58).
    pub fn to_kotlin_string(&self) -> String {
        match self {
            Self::Run {
                marathonfile,
                analytics_tracking,
                bugsnag_reporting,
            } => format!(
                "MarathonRunCommandCliConfiguration(marathonfile={}, analyticsTracking={analytics_tracking}, bugsnagReporting={bugsnag_reporting})",
                marathonfile.to_string_lossy().into_owned()
            ),
            Self::Parse {
                marathonfile,
                output_file_name,
                include_flaky_tests,
            } => format!(
                "ParseCommandCliConfiguration(marathonfile={}, outputFileName={}, includeFlakyTests={include_flaky_tests})",
                marathonfile.to_string_lossy().into_owned(),
                output_file_name.as_deref().unwrap_or("null")
            ),
            Self::Version => "VersionCommandCliConfiguration".into(),
        }
    }
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Execute(CliConfiguration),
    /// The hidden `config-dump` command (PLAN §6.3) with the arguments after its name.
    ConfigDump(Vec<String>),
    /// Help (stdout, exit 0).
    Help(String),
    /// A usage error (stderr, exit 1), already formatted.
    UsageError(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Root,
    Version,
    Parse,
    Run,
}

/// An option: its names in declaration order and whether it is the help flag (the only option without a value).
struct Opt {
    names: &'static [&'static str],
    flag: bool,
}

const MARATHONFILE: Opt = Opt {
    names: &["--marathonfile", "-m"],
    flag: false,
};
const ANALYTICS: Opt = Opt {
    names: &["--analyticsTracking"],
    flag: false,
};
const BUGSNAG: Opt = Opt {
    names: &["--bugsnag"],
    flag: false,
};
const OUTPUT: Opt = Opt {
    names: &["--output", "-o"],
    flag: false,
};
const FLAKY: Opt = Opt {
    names: &["--include-flaky-tests", "-f"],
    flag: false,
};
// Clikt registers the help option when the context is built, after the command's own options.
const HELP: Opt = Opt {
    names: &["-h", "--help"],
    flag: true,
};

const SUBCOMMANDS: [(&str, Cmd); 3] = [("version", Cmd::Version), ("parse", Cmd::Parse), ("run", Cmd::Run)];

impl Cmd {
    fn options(self) -> &'static [Opt] {
        match self {
            Self::Root | Self::Run => &[MARATHONFILE, ANALYTICS, BUGSNAG, HELP],
            Self::Parse => &[MARATHONFILE, OUTPUT, FLAKY, HELP],
            Self::Version => &[HELP],
        }
    }

    fn usage(self) -> &'static str {
        match self {
            Self::Root => "Usage: marathon [<options>] <command> [<args>]...",
            Self::Version => "Usage: marathon version [<options>]",
            Self::Parse => "Usage: marathon parse [<options>]",
            Self::Run => "Usage: marathon run [<options>]",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::Root => ROOT_HELP,
            Self::Version => VERSION_HELP,
            Self::Parse => PARSE_HELP,
            Self::Run => RUN_HELP,
        }
    }

    fn subcommand(self, token: &str) -> Option<Cmd> {
        if self != Self::Root {
            return None;
        }
        SUBCOMMANDS.iter().find(|(name, _)| *name == token).map(|(_, cmd)| *cmd)
    }

    fn option(self, name: &str) -> Option<usize> {
        self.options().iter().position(|o| o.names.contains(&name))
    }

    /// `optionsByName.keys` in insertion order (the suggestion candidates).
    fn option_names(self) -> impl Iterator<Item = &'static str> {
        self.options().iter().flat_map(|o| o.names.iter().copied())
    }

    fn is_long_name(self, name: &str) -> bool {
        name.chars().count() > 2 && self.option(name).is_some()
    }

    /// The prefixes of the option names: `-` and `--` for every command.
    fn has_prefix(self, prefix: &str) -> bool {
        !prefix.is_empty() && self.option_names().any(|n| split_option_prefix(n).0 == prefix)
    }
}

const ROOT_HELP: &str = "Usage: marathon [<options>] <command> [<args>]...

Options:
  -m, --marathonfile=<path>    Marathonfile file path
  --analyticsTracking=<value>  Enable / Disable anonymous analytics tracking.
                               Enabled by default.
  --bugsnag=<value>            Enable/Disable anonymous crash reporting.
                               Enabled by default
  -h, --help                   Show this message and exit

Commands:
  version  Print version and exit
  parse    Print the list of tests without executing them
  run      Run Marathon to execute tests
";

const RUN_HELP: &str = "Usage: marathon run [<options>]

  Run Marathon to execute tests

Options:
  -m, --marathonfile=<path>    Marathonfile file path
  --analyticsTracking=<value>  Enable / Disable anonymous analytics tracking.
                               Enabled by default.
  --bugsnag=<value>            Enable/Disable anonymous crash reporting.
                               Enabled by default
  -h, --help                   Show this message and exit
";

// C12: the help says `.yaml`, the parse command writes `.yml`.
const PARSE_HELP: &str = "Usage: marathon parse [<options>]

  Print the list of tests without executing them

Options:
  -m, --marathonfile=<path>  Marathonfile file path
  -o, --output=<text>        Output file name without extension. Will be in the
                             outputDir with .yaml extension
  -f, --include-flaky-tests=<value>
                             Include/exclude flaky tests that will have
                             preventive retries according to the current
                             flakinessStrategy
  -h, --help                 Show this message and exit
";

const VERSION_HELP: &str = "Usage: marathon version [<options>]

  Print version and exit

Options:
  -h, --help  Show this message and exit
";

/// Clikt's `splitOptionPrefix`.
fn split_option_prefix(name: &str) -> (&str, &str) {
    let mut chars = name.chars();
    let (first, second) = (chars.next(), chars.next());
    match (first, second) {
        (Some(c), Some(_)) if !"-@/+".contains(c) => ("", name),
        (None, _) | (_, None) => ("", name),
        (Some(c), Some(d)) if c == d && name.chars().count() > 2 => name.split_at(2 * c.len_utf8()),
        (Some(c), _) => name.split_at(c.len_utf8()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UsageError {
    NoSuchOption(String, Vec<String>),
    NoSuchSubcommand(String, Vec<String>),
    NoSuchArgument(Vec<String>),
    /// `IncorrectOptionValueCount`: the name as typed and the option's minimum value count.
    ValueCount(String, usize),
}

impl UsageError {
    fn message(&self) -> String {
        fn possibilities(kind: &str, names: &[String]) -> String {
            match names {
                [] => String::new(),
                [one] => format!(". Did you mean {one}?"),
                many => format!(". (Possible {kind}: {})", many.join(", ")),
            }
        }
        match self {
            Self::NoSuchOption(name, p) => format!("no such option {name}{}", possibilities("options", p)),
            Self::NoSuchSubcommand(name, p) => format!("no such subcommand {name}{}", possibilities("subcommands", p)),
            Self::NoSuchArgument(args) => {
                let mut shown = args.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
                if args.len() > 3 {
                    shown.push_str(" ...");
                }
                if args.len() == 1 {
                    format!("got unexpected extra argument ({shown})")
                } else {
                    format!("got unexpected extra arguments ({shown})")
                }
            }
            Self::ValueCount(name, 0) => format!("option {name} does not take a value"),
            Self::ValueCount(name, _) => format!("option {name} requires a value"),
        }
    }
}

/// One `OptInvocation`: the option's index, the name as typed and its values.
struct OptInvocation {
    option: usize,
    name: String,
    values: Vec<String>,
}

struct CommandParse {
    cmd: Cmd,
    invocations: Vec<OptInvocation>,
    errors: Vec<UsageError>,
    subcommand: Option<Cmd>,
    i: usize,
}

/// Clikt's `CommandParser` for one command, from token `start`.
fn parse_command(cmd: Cmd, tokens: &[String], start: usize) -> CommandParse {
    let mut p = CommandParse {
        cmd,
        invocations: Vec::new(),
        errors: Vec::new(),
        subcommand: None,
        i: start,
    };
    let mut arguments: Vec<String> = Vec::new();
    let mut can_parse_options = true;
    while p.i < tokens.len() {
        let tok = tokens[p.i].as_str();
        let prefix = split_option_prefix(tok).0;
        if can_parse_options && tok == "--" {
            p.i += 1;
            can_parse_options = false;
        } else if can_parse_options
            && ((prefix.chars().count() > 1 && cmd.has_prefix(prefix))
                || cmd.is_long_name(tok)
                || is_long_option_with_equals(cmd, prefix, tok))
        {
            let consumed = parse_long_opt(&mut p, tokens, tok);
            p.i += consumed;
        } else if can_parse_options && tok.chars().count() >= 2 && cmd.has_prefix(prefix) {
            let consumed = parse_short_opt(&mut p, tokens, tok);
            p.i += consumed;
        } else if let Some(sub) = cmd.subcommand(tok) {
            p.subcommand = Some(sub);
            p.i += 1;
            break;
        } else {
            arguments.push(tok.to_owned());
            p.i += 1;
        }
    }
    // No command has positional arguments: every argument token is excess.
    match arguments.len() {
        0 => {}
        1 if cmd == Cmd::Root => {
            let actual = arguments.remove(0);
            let names = SUBCOMMANDS.iter().map(|(n, _)| *n);
            let suggestions = suggest(&actual, names);
            p.errors.push(UsageError::NoSuchSubcommand(actual, suggestions));
        }
        _ => p.errors.push(UsageError::NoSuchArgument(arguments)),
    }
    p
}

fn is_long_option_with_equals(cmd: Cmd, prefix: &str, tok: &str) -> bool {
    let Some((before, _)) = tok.split_once('=') else {
        return false;
    };
    if prefix.is_empty() {
        return false;
    }
    if prefix.chars().count() > 1 || cmd.is_long_name(before) {
        return true;
    }
    let first_two: String = tok.chars().take(2).collect();
    cmd.option(&first_two).is_none()
}

fn parse_long_opt(p: &mut CommandParse, tokens: &[String], tok: &str) -> usize {
    let (name, attached) = match tok.split_once('=') {
        Some((name, value)) => (name, Some(value.to_owned())),
        None => (tok, None),
    };
    match p.cmd.option(name) {
        Some(option) => parse_opt_values(p, tokens, option, name, attached),
        None => {
            let suggestions = suggest(name, p.cmd.option_names());
            p.errors.push(UsageError::NoSuchOption(name.to_owned(), suggestions));
            1
        }
    }
}

fn parse_short_opt(p: &mut CommandParse, tokens: &[String], tok: &str) -> usize {
    let mut chars = tok.char_indices();
    let Some((_, prefix)) = chars.next() else {
        return 1;
    };
    let mut flags = Vec::new();
    for (index, c) in chars {
        let name = format!("{prefix}{c}");
        let Some(option) = p.cmd.option(&name) else {
            // The whole token is dropped, including flags already seen in it.
            let long = format!("-{tok}");
            let suggestions = if prefix == '-' && p.cmd.option(&long).is_some() {
                vec![long]
            } else {
                Vec::new()
            };
            p.errors.push(UsageError::NoSuchOption(name, suggestions));
            return 1;
        };
        if p.cmd.options()[option].flag {
            flags.push(OptInvocation {
                option,
                name,
                values: Vec::new(),
            });
        } else {
            let rest = &tok[index + c.len_utf8()..];
            let attached = (!rest.is_empty()).then(|| rest.to_owned());
            p.invocations.append(&mut flags);
            return parse_opt_values(p, tokens, option, &name, attached);
        }
    }
    p.invocations.append(&mut flags);
    1
}

/// Every option takes exactly one value except the help flag, which takes none.
fn parse_opt_values(
    p: &mut CommandParse,
    tokens: &[String],
    option: usize,
    name: &str,
    attached: Option<String>,
) -> usize {
    let flag = p.cmd.options()[option].flag;
    let mut values = Vec::new();
    let consumed = match attached {
        Some(value) => {
            values.push(value);
            1
        }
        None => {
            // An option's first value is taken whatever it looks like (`-m --bogus`, `-m run`).
            if !flag && let Some(value) = tokens.get(p.i + 1) {
                values.push(value.clone());
            }
            1 + values.len()
        }
    };
    p.invocations.push(OptInvocation {
        option,
        name: name.to_owned(),
        values,
    });
    consumed
}

/// Clikt's default `suggestTypoCorrection`: candidates with a Jaro-Winkler similarity above 0.8, most similar first.
fn suggest<'a>(entered: &str, candidates: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut scored: Vec<(String, f64)> = candidates
        .map(|c| (c.to_owned(), jaro_winkler(entered, c)))
        .filter(|(_, s)| *s > 0.8)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.into_iter().map(|(c, _)| c).collect()
}

/// Clikt's `JaroWinkerSimilarity.kt`, over UTF-16 units as Kotlin strings index.
fn jaro_winkler(s1: &str, s2: &str) -> f64 {
    let a: Vec<u16> = s1.encode_utf16().collect();
    let b: Vec<u16> = s2.encode_utf16().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let jaro = jaro(&a, &b);
    (jaro + 0.1 * prefix as f64 * (1.0 - jaro)).min(1.0)
}

fn jaro(s1: &[u16], s2: &[u16]) -> f64 {
    if s1.is_empty() && s2.is_empty() {
        return 1.0;
    } else if s1.is_empty() || s2.is_empty() {
        return 0.0;
    } else if s1.len() == 1 && s2.len() == 1 {
        return if s1[0] == s2[0] { 1.0 } else { 0.0 };
    }
    let search_range = (s1.len().max(s2.len()) / 2) as i64 - 1;
    let mut consumed = vec![false; s2.len()];
    let mut matches = 0.0;
    let mut transpositions = 0;
    let mut match_index = 0;
    for (i, c1) in s1.iter().enumerate() {
        let start = (i as i64 - search_range).max(0);
        let end = (i as i64 + search_range).min(s2.len() as i64 - 1);
        let mut j = start;
        while j <= end {
            let ju = j as usize;
            if *c1 == s2[ju] && !consumed[ju] {
                consumed[ju] = true;
                matches += 1.0;
                if ju < match_index {
                    transpositions += 1;
                }
                match_index = ju;
                break;
            }
            j += 1;
        }
    }
    if matches == 0.0 {
        0.0
    } else {
        (matches / s1.len() as f64 + matches / s2.len() as f64 + (matches - f64::from(transpositions)) / matches) / 3.0
    }
}

/// `MultiUsageError.buildOrNull` and the formatter: one error keeps its command's usage line; several are reported
/// under the root's (the combined error has no context), without argument errors after an unknown option.
fn usage_error(cmd: Cmd, errors: Vec<UsageError>) -> Invocation {
    let (cmd, errors) = if errors.len() == 1 {
        (cmd, errors)
    } else {
        let mut unknown_option = false;
        let kept = errors
            .into_iter()
            .filter(|e| {
                if matches!(e, UsageError::NoSuchOption(..)) {
                    unknown_option = true;
                }
                !unknown_option || !matches!(e, UsageError::NoSuchArgument(_))
            })
            .collect();
        (Cmd::Root, kept)
    };
    let lines: Vec<String> = errors.iter().map(|e| format!("Error: {}", e.message())).collect();
    Invocation::UsageError(format!("{}\n\n{}\n", cmd.usage(), lines.join("\n")))
}

/// The last value of an option, if it was given (Clikt's default `transformAll`).
fn last_value(p: &CommandParse, names: &[&str]) -> Option<String> {
    p.invocations
        .iter()
        .rev()
        .find(|inv| p.cmd.options()[inv.option].names == names)
        .and_then(|inv| inv.values.first().cloned())
}

/// Kotlin's `String.toBoolean()`: `true` in any case, anything else is false.
fn to_boolean(value: &str) -> bool {
    value.eq_ignore_ascii_case("true")
}

/// Clikt's run of one parsed command: eager options (help), then parse errors, then value counts.
fn finalize(p: &CommandParse) -> Result<(), Invocation> {
    let help: Vec<_> = p
        .invocations
        .iter()
        .filter(|inv| p.cmd.options()[inv.option].flag)
        .collect();
    if let Some(inv) = help.iter().find(|inv| !inv.values.is_empty()) {
        return Err(usage_error(p.cmd, vec![UsageError::ValueCount(inv.name.clone(), 0)]));
    }
    if !help.is_empty() {
        return Err(Invocation::Help(p.cmd.help().to_owned()));
    }
    if !p.errors.is_empty() {
        return Err(usage_error(p.cmd, p.errors.clone()));
    }
    // An option lacks its value only at the end of the command line, so there is at most one.
    if let Some(inv) = p.invocations.iter().find(|inv| inv.values.is_empty()) {
        return Err(usage_error(p.cmd, vec![UsageError::ValueCount(inv.name.clone(), 1)]));
    }
    Ok(())
}

fn marathonfile(p: &CommandParse) -> PathBuf {
    file_path(&last_value(p, MARATHONFILE.names).unwrap_or_else(|| "Marathonfile".into()))
}

fn run_configuration(p: &CommandParse) -> CliConfiguration {
    CliConfiguration::Run {
        marathonfile: marathonfile(p),
        analytics_tracking: last_value(p, ANALYTICS.names).is_none_or(|v| to_boolean(&v)),
        bugsnag_reporting: last_value(p, BUGSNAG.names).is_none_or(|v| to_boolean(&v)),
    }
}

/// `MarathonCli(::execute).subcommands(Version, Parse, RunMarathon).main(args)` up to the `execute` call.
pub fn parse(args: &[String]) -> Invocation {
    if args.first().map(String::as_str) == Some("config-dump") {
        return Invocation::ConfigDump(args[1..].to_vec());
    }
    let root = parse_command(Cmd::Root, args, 0);
    if let Err(done) = finalize(&root) {
        return done;
    }
    let Some(sub) = root.subcommand else {
        // invokeWithoutSubcommand: the pre-subcommand CLI runs with the root's options.
        return Invocation::Execute(run_configuration(&root));
    };
    // The root's options are parsed and validated but never reach the subcommand.
    let p = parse_command(sub, args, root.i);
    if let Err(done) = finalize(&p) {
        return done;
    }
    Invocation::Execute(match sub {
        Cmd::Run | Cmd::Root => run_configuration(&p),
        Cmd::Version => CliConfiguration::Version,
        Cmd::Parse => CliConfiguration::Parse {
            marathonfile: marathonfile(&p),
            output_file_name: last_value(&p, OUTPUT.names),
            include_flaky_tests: last_value(&p, FLAKY.names).is_some_and(|v| to_boolean(&v)),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn jaro_winkler_matches_clikt_suggestions() {
        assert_eq!(suggest("--bogus", Cmd::Root.option_names()), ["--bugsnag"]);
        assert_eq!(suggest("--hel", Cmd::Root.option_names()), ["--help", "-h"]);
        assert!(suggest("rnu", SUBCOMMANDS.iter().map(|(n, _)| *n)).is_empty());
        assert_eq!(suggest("parsee", SUBCOMMANDS.iter().map(|(n, _)| *n)), ["parse"]);
    }

    #[test]
    fn config_dump_is_hidden_and_first_only() {
        assert_eq!(
            parse(&args(&["config-dump", "x"])),
            Invocation::ConfigDump(args(&["x"]))
        );
        assert!(matches!(
            parse(&args(&["-m", "a", "config-dump"])),
            Invocation::UsageError(_)
        ));
        assert!(!ROOT_HELP.contains("config-dump"));
    }

    #[test]
    fn version_line_uses_the_simple_class_name() {
        assert_eq!(
            CliConfiguration::Version.to_kotlin_string(),
            "VersionCommandCliConfiguration"
        );
    }
}
