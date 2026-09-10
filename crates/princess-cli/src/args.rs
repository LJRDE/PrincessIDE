//! Hand-rolled command-line parsing.
//!
//! No `clap`: the CLI is a headless acceptance vehicle, its flag set is small
//! and fixed, and every extra dependency has to be downloaded on a host whose
//! international bandwidth is measured in KB/s.

use std::fmt;
use std::path::PathBuf;

/// Usage text shown for `--help` and for usage errors.
pub const USAGE: &str = "\
princess-cli — PrincessIDE headless engine driver

USAGE:
    princess-cli <COMMAND> [OPTIONS]

COMMANDS:
    doctor                                  detect the toolchain and print it as events
    build                                   build the project, emit build.* events
    run                                     build (unless --no-build), boot the kernel in QEMU
    symbolicate                             map a fault RIP to symbol/file/line
    events                                  validate a recorded NDJSON event stream

COMMON OPTIONS:
    --project <DIR>        project directory (default: current directory)
    --record <PATH>        also write the NDJSON event stream to PATH
    --op-id <ID>           operation id stamped on every event (default: generated)
    --quiet                suppress human-readable progress on stderr
    --strict               exit non-zero when the operation reported failure
    -h, --help             show this help

BUILD OPTIONS:
    --target <NAME>        build target (repeatable; overrides princess.toml)
    --timeout-ms <N>       build deadline

RUN OPTIONS:
    --timeout-ms <N>       run deadline (default: princess.toml run.timeout_ms)
    --cancel-after-ms <N>  cancel the run N ms after start (reason=killed)
    --no-build             boot whatever was built before
    --keep-qemu-log        keep the QEMU debug log used for triple-fault detection

SYMBOLICATE OPTIONS:
    --rip <0xADDR>         address to look up
    --from-log <PATH>      read the fault RIP from a captured serial log
    --elf <PATH>           ELF with debug info (default: the project's kernel)

EVENTS OPTIONS:
    --file <PATH>          NDJSON stream to validate (required)

EXIT CODES:
    0   the operation produced its event stream (inspect status/reason fields)
    1   the operation failed before it could produce a complete event stream,
        or --strict was given and the operation reported failure
    2   usage error
";

/// `build` flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildArgs {
    pub targets: Vec<String>,
    pub timeout_ms: Option<u64>,
}

/// `run` flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunArgs {
    pub timeout_ms: Option<u64>,
    pub cancel_after_ms: Option<u64>,
    pub no_build: bool,
    pub keep_qemu_log: bool,
}

/// `symbolicate` flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolicateArgs {
    pub rip: Option<String>,
    pub from_log: Option<PathBuf>,
    pub elf: Option<PathBuf>,
}

/// `events` flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventsArgs {
    pub file: Option<PathBuf>,
}

/// The subcommand to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Doctor,
    Build(BuildArgs),
    Run(RunArgs),
    Symbolicate(SymbolicateArgs),
    Events(EventsArgs),
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Command::Doctor => "doctor",
            Command::Build(_) => "build",
            Command::Run(_) => "run",
            Command::Symbolicate(_) => "symbolicate",
            Command::Events(_) => "events",
        };
        f.write_str(name)
    }
}

/// Fully parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// `None` when no subcommand was given (usage is printed).
    pub command: Option<Command>,
    pub project: PathBuf,
    pub record: Option<PathBuf>,
    pub op_id: Option<String>,
    pub quiet: bool,
    /// Exit non-zero when the operation reported failure (`status=failed`,
    /// `reason=timeout`, ...).  Off by default: the event stream, not the exit
    /// code, carries the outcome of a *started* operation.
    pub strict: bool,
    pub help: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            command: None,
            project: PathBuf::from("."),
            record: None,
            op_id: None,
            quiet: false,
            strict: false,
            help: false,
        }
    }
}

/// A usage error: printed with the usage text, exit code 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

/// Parse `std::env::args().skip(1)`.
pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Args, UsageError> {
    let tokens: Vec<String> = argv.into_iter().collect();
    let mut args = Args::default();
    let mut command: Option<Command> = None;
    let mut index = 0usize;

    // split_value handles both `--flag value` and `--flag=value`.
    fn split_value(token: &str) -> (String, Option<String>) {
        match token.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (token.to_string(), None),
        }
    }

    while index < tokens.len() {
        let token = tokens[index].clone();
        index += 1;

        if token == "-h" || token == "--help" {
            args.help = true;
            continue;
        }
        if !token.starts_with('-') {
            if command.is_some() {
                return Err(UsageError(format!(
                    "unexpected argument `{token}` (subcommand `{}` already given)",
                    command.unwrap()
                )));
            }
            command = Some(match token.as_str() {
                "doctor" => Command::Doctor,
                "build" => Command::Build(BuildArgs::default()),
                "run" => Command::Run(RunArgs::default()),
                "symbolicate" => Command::Symbolicate(SymbolicateArgs::default()),
                "events" => Command::Events(EventsArgs::default()),
                other => {
                    return Err(UsageError(format!(
                        "unknown subcommand `{other}` (expected doctor|build|run|symbolicate|events)"
                    )))
                }
            });
            continue;
        }

        let (flag, inline) = split_value(&token);
        // Every value-taking flag can be spelled `--flag value` or `--flag=value`.
        macro_rules! value {
            () => {{
                match inline {
                    Some(value) => value,
                    None => {
                        if index >= tokens.len() {
                            return Err(UsageError(format!("`{flag}` needs a value")));
                        }
                        let value = tokens[index].clone();
                        index += 1;
                        value
                    }
                }
            }};
        }

        match flag.as_str() {
            "--project" => args.project = PathBuf::from(value!()),
            "--record" => args.record = Some(PathBuf::from(value!())),
            "--op-id" => args.op_id = Some(value!()),
            "--quiet" => args.quiet = true,
            "--strict" => args.strict = true,
            "--target" => match command.as_mut() {
                Some(Command::Build(build)) => build.targets.push(value!()),
                _ => return Err(UsageError("`--target` is only valid for `build`".into())),
            },
            "--timeout-ms" => {
                let raw = value!();
                let parsed: u64 = raw
                    .parse()
                    .map_err(|_| UsageError(format!("`--timeout-ms {raw}` is not a number")))?;
                match command.as_mut() {
                    Some(Command::Build(build)) => build.timeout_ms = Some(parsed),
                    Some(Command::Run(run)) => run.timeout_ms = Some(parsed),
                    _ => {
                        return Err(UsageError(
                            "`--timeout-ms` is only valid for `build` and `run`".into(),
                        ))
                    }
                }
            }
            "--cancel-after-ms" => {
                let raw = value!();
                let parsed: u64 = raw
                    .parse()
                    .map_err(|_| UsageError(format!("`--cancel-after-ms {raw}` is not a number")))?;
                match command.as_mut() {
                    Some(Command::Run(run)) => run.cancel_after_ms = Some(parsed),
                    _ => return Err(UsageError("`--cancel-after-ms` is only valid for `run`".into())),
                }
            }
            "--no-build" => match command.as_mut() {
                Some(Command::Run(run)) => run.no_build = true,
                _ => return Err(UsageError("`--no-build` is only valid for `run`".into())),
            },
            "--keep-qemu-log" => match command.as_mut() {
                Some(Command::Run(run)) => run.keep_qemu_log = true,
                _ => {
                    return Err(UsageError(
                        "`--keep-qemu-log` is only valid for `run`".into(),
                    ))
                }
            },
            "--rip" => match command.as_mut() {
                Some(Command::Symbolicate(sym)) => sym.rip = Some(value!()),
                _ => return Err(UsageError("`--rip` is only valid for `symbolicate`".into())),
            },
            "--from-log" => match command.as_mut() {
                Some(Command::Symbolicate(sym)) => sym.from_log = Some(PathBuf::from(value!())),
                _ => {
                    return Err(UsageError(
                        "`--from-log` is only valid for `symbolicate`".into(),
                    ))
                }
            },
            "--elf" => match command.as_mut() {
                Some(Command::Symbolicate(sym)) => sym.elf = Some(PathBuf::from(value!())),
                _ => return Err(UsageError("`--elf` is only valid for `symbolicate`".into())),
            },
            "--file" => match command.as_mut() {
                Some(Command::Events(events)) => events.file = Some(PathBuf::from(value!())),
                _ => return Err(UsageError("`--file` is only valid for `events`".into())),
            },
            other => return Err(UsageError(format!("unknown option `{other}`"))),
        }
    }

    args.command = command;
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Args {
        parse(args.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn subcommands_and_defaults() {
        let args = parse_ok(&["doctor"]);
        assert_eq!(args.command, Some(Command::Doctor));
        assert_eq!(args.project, PathBuf::from("."));
        assert!(args.record.is_none());
        assert!(!args.quiet);
        assert!(!args.help);
        assert!(!args.strict);
    }

    #[test]
    fn strict_is_a_common_flag() {
        let args = parse_ok(&["--strict", "run"]);
        assert!(args.strict);
        assert!(matches!(args.command, Some(Command::Run(_))));
    }

    #[test]
    fn flags_accept_space_and_equals_forms_in_any_order() {
        let args = parse_ok(&["build", "--project", "fixtures/refkernel", "--record=x.ndjson"]);
        assert_eq!(args.project, PathBuf::from("fixtures/refkernel"));
        assert_eq!(args.record, Some(PathBuf::from("x.ndjson")));

        let args = parse_ok(&["--project=fixtures/refkernel", "build"]);
        assert_eq!(args.project, PathBuf::from("fixtures/refkernel"));
        assert!(matches!(args.command, Some(Command::Build(_))));

        let args = parse_ok(&["build", "--target", "iso", "--target", "all", "--timeout-ms", "9000"]);
        match args.command.unwrap() {
            Command::Build(build) => {
                assert_eq!(build.targets, vec!["iso".to_string(), "all".to_string()]);
                assert_eq!(build.timeout_ms, Some(9000));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn run_flags() {
        let args = parse_ok(&[
            "run",
            "--timeout-ms",
            "8000",
            "--cancel-after-ms=2500",
            "--no-build",
            "--keep-qemu-log",
        ]);
        match args.command.unwrap() {
            Command::Run(run) => {
                assert_eq!(run.timeout_ms, Some(8000));
                assert_eq!(run.cancel_after_ms, Some(2500));
                assert!(run.no_build);
                assert!(run.keep_qemu_log);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn symbolicate_and_events_flags() {
        let args = parse_ok(&["symbolicate", "--rip", "0x100b3d", "--elf", "k.elf"]);
        match args.command.unwrap() {
            Command::Symbolicate(sym) => {
                assert_eq!(sym.rip.as_deref(), Some("0x100b3d"));
                assert_eq!(sym.elf, Some(PathBuf::from("k.elf")));
            }
            other => panic!("unexpected {other:?}"),
        }
        let args = parse_ok(&["events", "--file", "fixtures/events/refkernel-run.ndjson"]);
        match args.command.unwrap() {
            Command::Events(events) => {
                assert_eq!(
                    events.file,
                    Some(PathBuf::from("fixtures/events/refkernel-run.ndjson"))
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn usage_errors() {
        let err = parse(vec!["nope".to_string()]).unwrap_err();
        assert!(err.0.contains("unknown subcommand"), "{err}");
        let err = parse(vec!["--project".to_string()]).unwrap_err();
        assert!(err.0.contains("needs a value"), "{err}");
        let err = parse(vec!["doctor".to_string(), "--target".to_string(), "x".to_string()])
            .unwrap_err();
        assert!(err.0.contains("only valid for `build`"), "{err}");
        let err = parse(vec!["build".to_string(), "--timeout-ms".to_string(), "soon".to_string()])
            .unwrap_err();
        assert!(err.0.contains("not a number"), "{err}");
        let err = parse(vec!["doctor".to_string(), "extra".to_string()]).unwrap_err();
        assert!(err.0.contains("already given"), "{err}");
        let err = parse(vec!["--bogus".to_string()]).unwrap_err();
        assert!(err.0.contains("unknown option"), "{err}");

        let args = parse(vec!["--help".to_string()]).unwrap();
        assert!(args.help);
        assert!(args.command.is_none());
        assert!(USAGE.contains("princess-cli"));
    }
}
