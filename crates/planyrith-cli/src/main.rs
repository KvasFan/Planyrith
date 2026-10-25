//! `planyrith` command-line frontend (docs/08-cli-specification.md).
//!
//! Session verbs open (or create) a `.plyr` document, run commands through the
//! core `Engine` and save. `tool` options are generated from each command's
//! `Params` JSON schema at run time ([`args`]). Milestone 0.1 provides `new`,
//! `info`, `query`, `tool`, `export` (JSON), `validate`, `repair`, `batch` and
//! `serve` (JSON-RPC over stdio); `import`, `render`, `script`, `repl` and the
//! socket transport follow their milestones (docs/13).

#![forbid(unsafe_code)]
// Tests may unwrap (AGENTS.md §3); the binary does not.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod args;
mod out;
mod serve;
mod session;
mod shell;
mod verbs;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use planyrith_core::commands::CommandError;
use serde_json::{Value, json};

/// Global options (docs/08 §1.1); accepted before or after the verb.
#[derive(Debug, Clone, Default, Args)]
pub struct Global {
    /// Document to operate on (`.plyr`); `-` reads standard input.
    #[arg(short = 'f', long, global = true, value_name = "PATH")]
    pub file: Option<PathBuf>,
    /// Write the result to another file instead of in place (`-` = stdout).
    #[arg(short = 'o', long, global = true, value_name = "PATH")]
    pub out: Option<PathBuf>,
    /// Apply but do not write.
    #[arg(long, global = true)]
    pub no_save: bool,
    /// Validate only; nothing is applied.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Machine-readable output on stdout (one JSON document).
    #[arg(long, global = true)]
    pub json: bool,
    /// One JSON object per line (`batch`).
    #[arg(long, global = true)]
    pub jsonl: bool,
    /// Less logging.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// More logging (repeat for more).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Unit of bare numbers: mm|cm|m|in|ft (default: the document's display unit).
    #[arg(long, global = true, value_name = "UNIT")]
    pub units: Option<String>,
    /// Edit context for the session, e.g. `#Model/Kitchen`.
    #[arg(long, global = true, value_name = "PATH")]
    pub context: Option<String>,
    /// Initial selection query.
    #[arg(long, global = true, value_name = "QUERY")]
    pub select: Option<String>,
    /// Fixed timestamps for byte-identical output.
    #[arg(long, global = true)]
    pub deterministic: bool,
}

impl Global {
    /// Options given after the verb override those given before it.
    pub fn merged(&self, later: &Global) -> Global {
        Global {
            file: later.file.clone().or_else(|| self.file.clone()),
            out: later.out.clone().or_else(|| self.out.clone()),
            no_save: self.no_save || later.no_save,
            dry_run: self.dry_run || later.dry_run,
            json: self.json || later.json,
            jsonl: self.jsonl || later.jsonl,
            quiet: self.quiet || later.quiet,
            verbose: self.verbose.max(later.verbose),
            units: later.units.clone().or_else(|| self.units.clone()),
            context: later.context.clone().or_else(|| self.context.clone()),
            select: later.select.clone().or_else(|| self.select.clone()),
            deterministic: self.deterministic || later.deterministic,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "planyrith",
    version,
    about = "Planyrith 3D modeler: every tool on the command line",
    long_about = None
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Debug, Subcommand)]
enum Verb {
    /// Create a document from a template and save it.
    New {
        /// default | architectural-mm | woodworking-mm | imperial-ft-in | product-mm | blank
        #[arg(long, default_value = "default")]
        template: String,
        /// Document title.
        #[arg(long)]
        title: Option<String>,
    },
    /// Print title, units, counts, bounds and validation warnings.
    Info,
    /// List entities matching a selection query.
    Query {
        /// The query (docs/08 §5).
        query: String,
        /// Fields: pid,kind,path,tag,material,bbox,length,area,volume,name,definition,transform,attrs[dict.key],is_solid,hidden,locked
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Print only the number of matches.
        #[arg(long)]
        count: bool,
        /// Print sums of length, area and volume.
        #[arg(long)]
        stats: bool,
    },
    /// Run one registered command (`planyrith tool` lists them).
    #[command(disable_help_flag = true)]
    Tool {
        /// Command name.
        #[arg(allow_hyphen_values = true)]
        name: Option<String>,
        /// Command options (`planyrith tool <name> --help`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Export the document (0.1: `json`).
    Export {
        /// Output path (`-` = stdout).
        output: PathBuf,
        /// Format (default: from the extension).
        #[arg(long)]
        format: Option<String>,
        /// Sorted keys and fixed float formatting for diffs.
        #[arg(long)]
        canonical: bool,
        /// Indented JSON.
        #[arg(long)]
        pretty: bool,
    },
    /// Check the document (docs/04 §13); exit code 3 if problems remain.
    Validate,
    /// Fix what can be fixed: all | merge-vertices,degenerate-faces,dangling-refs
    Repair {
        /// Fixes to apply.
        #[arg(long, default_value = "all")]
        fix: String,
    },
    /// Run a command batch (one command per line) as one undo step and one save.
    Batch {
        /// Batch file (`-` or absent = standard input).
        script: Option<PathBuf>,
    },
    /// JSON-RPC 2.0 server, one JSON frame per line on standard input/output.
    Serve {
        /// Serve standard input and output (the default; the only transport in 0.1).
        #[arg(long)]
        stdio: bool,
        /// Unix socket path or localhost port (arrives with the GUI attach mode, 0.2).
        #[arg(long, value_name = "PATH|PORT", conflicts_with = "stdio")]
        socket: Option<String>,
        /// Refuse commands that change documents, undo/redo and saving.
        #[arg(long)]
        readonly: bool,
        /// Shared secret a socket client must present (socket transport only).
        #[arg(long, value_name = "SECRET")]
        token: Option<String>,
    },
}

/// A failure with its exit code (docs/08 §8) and JSON error object (§7).
#[derive(Debug)]
pub struct CliError {
    /// Process exit code.
    pub exit: u8,
    /// Stable error code (`E…`), if any.
    pub code: Option<String>,
    /// Message.
    pub message: String,
    /// Offending parameter.
    pub param: Option<String>,
    /// Already reported (the verb printed it in its own output mode).
    pub reported: bool,
}

impl CliError {
    /// Exit code 1: bad arguments.
    pub fn usage(message: impl Into<String>) -> Self {
        CliError {
            exit: 1,
            code: None,
            message: message.into(),
            param: None,
            reported: false,
        }
    }

    /// Maps a command error to its exit code.
    pub fn from_command(err: CommandError) -> Self {
        let code = err.code();
        let exit = match code {
            "E1701" => 5,
            "E1901" => 70,
            c if c.starts_with("E14") || c == "E1501" => 4,
            _ => 2,
        };
        CliError {
            exit,
            code: Some(code.to_string()),
            param: err.param().map(str::to_string),
            message: err.to_string(),
            reported: false,
        }
    }

    /// Exit code 4: I/O or format error.
    pub fn io(err: planyrith_core::io::IoError) -> Self {
        CliError::from_command(CommandError::Io(err))
    }

    /// Exit code 2, `E1001`: an invalid value for parameter `param`.
    pub fn invalid(param: &str, reason: impl Into<String>) -> Self {
        CliError::from_command(CommandError::invalid_param(param, reason))
    }

    /// JSON form (docs/08 §7).
    pub fn to_json(&self) -> Value {
        json!({
            "ok": false,
            "error": { "code": self.code, "message": self.message, "param": self.param },
        })
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().collect();
    let outcome = std::panic::catch_unwind(|| run(raw));
    match outcome {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err((err, json_mode))) => {
            if err.reported {
            } else if json_mode {
                // Nothing more can be done when standard output is gone.
                let _ = out::line(err.to_json());
            } else {
                match &err.code {
                    Some(code) => out::diagnostic(format!("error[{code}]: {}", err.message)),
                    None => out::diagnostic(format!("error: {}", err.message)),
                }
            }
            ExitCode::from(err.exit)
        }
        Err(_) => {
            out::diagnostic("internal error: a bug was detected (panic); please report it");
            ExitCode::from(70)
        }
    }
}

fn run(raw: Vec<String>) -> Result<(), (CliError, bool)> {
    let cli = match Cli::try_parse_from(&raw) {
        Ok(cli) => cli,
        Err(e) => {
            let code = if e.use_stderr() { 1 } else { 0 };
            let _ = e.print();
            if code == 0 {
                return Ok(());
            }
            return Err((
                CliError {
                    exit: 1,
                    code: None,
                    message: "invalid arguments".into(),
                    param: None,
                    reported: true,
                },
                false,
            ));
        }
    };
    init_logging(&cli.global);
    let json_mode = cli.global.json || cli.global.jsonl;
    verbs::dispatch(cli.global, cli.verb).map_err(|e| (e, json_mode))
}

fn init_logging(global: &Global) {
    let level = if global.quiet {
        "error"
    } else {
        match global.verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(level))
        .with_writer(std::io::stderr)
        .try_init();
}
