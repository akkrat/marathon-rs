//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/args/CliCommands.kt and
//! cli/src/main/kotlin/com/malinskiy/marathon/cli/args/CliConfigurations.kt.

use clap::{Args, CommandFactory, Parser, Subcommand};
use jcompat::file_manager::file_path;
use std::{convert::Infallible, path::PathBuf};

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

#[derive(Parser)]
#[command(name = "marathon", disable_help_subcommand = true, args_override_self = true)]
struct Cli {
    #[command(flatten)]
    run: RunOptions,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print version and exit
    Version,
    /// Print the list of tests without executing them
    #[command(args_override_self = true)]
    Parse(ParseOptions),
    /// Run Marathon to execute tests
    #[command(args_override_self = true)]
    Run(RunOptions),
}

#[derive(Args)]
struct MarathonfileOption {
    /// Marathonfile file path
    #[arg(
        short = 'm',
        long,
        default_value = "Marathonfile",
        allow_hyphen_values = true,
        value_name = "PATH"
    )]
    marathonfile: String,
}

#[derive(Args)]
struct RunOptions {
    #[command(flatten)]
    file: MarathonfileOption,
    /// Enable / Disable anonymous analytics tracking. Enabled by default.
    #[arg(long = "analyticsTracking", default_value = "true", action = clap::ArgAction::Set,
          value_parser = to_boolean, allow_hyphen_values = true, value_name = "VALUE")]
    analytics_tracking: bool,
    /// Enable/Disable anonymous crash reporting. Enabled by default.
    #[arg(long = "bugsnag", default_value = "true", action = clap::ArgAction::Set,
          value_parser = to_boolean, allow_hyphen_values = true, value_name = "VALUE")]
    bugsnag_reporting: bool,
}

impl RunOptions {
    fn configuration(self) -> CliConfiguration {
        CliConfiguration::Run {
            marathonfile: file_path(&self.file.marathonfile),
            analytics_tracking: self.analytics_tracking,
            bugsnag_reporting: self.bugsnag_reporting,
        }
    }
}

#[derive(Args)]
struct ParseOptions {
    #[command(flatten)]
    file: MarathonfileOption,
    /// Output file name without extension. Will be in the outputDir with .yml extension
    #[arg(short = 'o', long = "output", allow_hyphen_values = true, value_name = "NAME")]
    output_file_name: Option<String>,
    /// Include/exclude flaky tests that will have preventive retries according to the current flakinessStrategy
    #[arg(short = 'f', long = "include-flaky-tests", default_value = "false", action = clap::ArgAction::Set,
          value_parser = to_boolean, allow_hyphen_values = true, value_name = "VALUE")]
    include_flaky_tests: bool,
}

/// Kotlin's `String.toBoolean()`: `true` in any case, anything else is false.
fn to_boolean(value: &str) -> Result<bool, Infallible> {
    Ok(value.eq_ignore_ascii_case("true"))
}

/// Preserve eager help's exit/stream precedence, short values' literal `=`, and subcommands after `--`.
/// The option inventory comes from the derive API; clap still validates and formats every diagnostic.
fn compatibility_args(command: &clap::Command, args: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut i = 0;
    let mut options = true;
    let mut help = false;
    let mut invalid_help = None;
    while let Some(token) = args.get(i) {
        i += 1;
        if options && token == "--" {
            options = false;
            result.push(token.clone());
        } else if let Some(subcommand) = command.find_subcommand(token) {
            if help && invalid_help.is_none() {
                break;
            }
            if !options && result.last().is_some_and(|s| s == "--") {
                result.pop();
            }
            result.push(token.clone());
            result.extend(compatibility_args(subcommand, &args[i..]));
            break;
        } else if options && token.starts_with("--") {
            let (name, value) = token[2..]
                .split_once('=')
                .map_or((&token[2..], None), |(n, v)| (n, Some(v)));
            let option = command.get_arguments().find(|arg| arg.get_long() == Some(name));
            if let Some(option) = option {
                if matches!(option.get_action(), clap::ArgAction::Help) {
                    if value.is_some() {
                        invalid_help = Some(token.clone());
                    } else {
                        help = true;
                    }
                } else if value.is_none()
                    && let Some(value) = args.get(i)
                {
                    result.push(token.clone());
                    result.push(value.clone());
                    i += 1;
                    continue;
                }
            }
            result.push(token.clone());
        } else if options && token.starts_with('-') && token.len() > 1 {
            let mut group = Vec::new();
            let mut group_help = false;
            for (at, short) in token.char_indices().skip(1) {
                let Some(option) = command.get_arguments().find(|arg| arg.get_short() == Some(short)) else {
                    // An invalid short group contributes no help, even when `h` precedes the invalid character.
                    group = vec![format!("-{short}")];
                    group_help = false;
                    break;
                };
                group.push(format!("-{short}"));
                if matches!(option.get_action(), clap::ArgAction::Help) {
                    group_help = true;
                } else {
                    let rest = &token[at + short.len_utf8()..];
                    if !rest.is_empty() {
                        group.push(rest.to_owned());
                    } else if let Some(value) = args.get(i) {
                        group.push(value.clone());
                        i += 1;
                    }
                    break;
                }
            }
            help |= group_help;
            result.extend(group);
        } else {
            result.push(token.clone());
        }
    }
    if let Some(invalid) = invalid_help {
        result.insert(0, invalid);
    } else if help {
        result.insert(0, "--help".into());
    }
    result
}

/// Parse without exiting: the caller owns the exit status and stdout/stderr routing.
pub fn parse(args: &[String]) -> Invocation {
    if args.first().map(String::as_str) == Some("config-dump") {
        return Invocation::ConfigDump(args[1..].to_vec());
    }
    let mut command = Cli::command();
    command.build();
    let args = compatibility_args(&command, args);
    let cli = match Cli::try_parse_from(std::iter::once("marathon".to_owned()).chain(args)) {
        Ok(cli) => cli,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            return Invocation::Help(error.to_string());
        }
        // clap defaults to exit 2; the CLI contract keeps usage errors at exit 1.
        Err(error) => return Invocation::UsageError(error.to_string()),
    };
    Invocation::Execute(match cli.command {
        // Root options are accepted and validated, but subcommands use their own defaults and values.
        None => cli.run.configuration(),
        Some(Command::Run(run)) => run.configuration(),
        Some(Command::Version) => CliConfiguration::Version,
        Some(Command::Parse(parse)) => CliConfiguration::Parse {
            marathonfile: file_path(&parse.file.marathonfile),
            output_file_name: parse.output_file_name,
            include_flaky_tests: parse.include_flaky_tests,
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
    fn config_dump_is_hidden_and_first_only() {
        assert_eq!(
            parse(&args(&["config-dump", "x"])),
            Invocation::ConfigDump(args(&["x"]))
        );
        assert!(matches!(
            parse(&args(&["-m", "a", "config-dump"])),
            Invocation::UsageError(_)
        ));
        let Invocation::Help(help) = parse(&args(&["--help"])) else {
            panic!("root help");
        };
        assert!(!help.contains("config-dump"));
    }

    #[test]
    fn version_line_uses_the_simple_class_name() {
        assert_eq!(
            CliConfiguration::Version.to_kotlin_string(),
            "VersionCommandCliConfiguration"
        );
    }
}
