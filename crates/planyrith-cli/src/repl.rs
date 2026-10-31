//! `planyrith repl` (docs/08 §3): an interactive shell on one document. Each
//! line is a `batch` line (one or more `;`-separated commands), run at once as
//! its own undo step; `:undo`, `:redo`, `:save`, `:json on|off`, `:help` and
//! `:q` (save and quit) / `:q!` (quit without saving) control the session. The
//! document is saved on `:q` and at the end of input unless `--no-save`.

use std::io::{BufRead, IsTerminal, Write};

use planyrith_core::commands::CommandRegistry;
use serde_json::{Value, json};

use crate::session::Session;
use crate::verbs::parse_tool_args;
use crate::{CliError, Global, out, shell};

const HELP: &str = "\
Each line runs commands like a batch line: `rectangle --corner-a 0,0,0 --corner-b 1m,1m,0`.
  :undo  :redo     step through the history
  :save            save now
  :json on|off     print command output as JSON
  :q               save and quit      :q!   quit without saving
  :help            this text          `planyrith tool` lists the commands";

/// Runs the shell until `:q` or the end of input.
pub fn repl(global: &Global) -> Result<(), CliError> {
    let mut session = Session::open(global, true)?;
    let registry = crate::extensions::registry()?;
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let mut json_mode = global.json;
    if interactive {
        out::diagnostic(format!(
            "planyrith {} — {} (:help for help)",
            env!("CARGO_PKG_VERSION"),
            session
                .path
                .as_ref()
                .map_or_else(|| "new document".to_string(), |p| p.display().to_string())
        ));
    }
    let mut lines = stdin.lock().lines();
    loop {
        if interactive {
            eprint!("planyrith> ");
            let _ = std::io::stderr().flush();
        }
        let Some(line) = lines.next() else { break };
        let line = line.map_err(|e| {
            CliError::io(planyrith_core::io::IoError::storage(
                "standard input",
                e.to_string(),
            ))
        })?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line {
            ":q" | ":quit" => break,
            ":q!" => return Ok(()),
            ":help" => out::line(HELP)?,
            ":undo" => report(
                session.engine.undo().map(|()| json!({ "undone": true })),
                json_mode,
            )?,
            ":redo" => report(
                session.engine.redo().map(|()| json!({ "redone": true })),
                json_mode,
            )?,
            ":save" => match session.save(global) {
                Ok(Some(path)) => {
                    session.path = Some(std::path::PathBuf::from(&path));
                    session.native_copy_pending = false;
                    out::diagnostic(format!("saved {path}"));
                }
                Ok(None) => out::diagnostic("not saved (--no-save or --dry-run)"),
                Err(e) => print_error(&e, json_mode)?,
            },
            ":json on" => json_mode = true,
            ":json off" => json_mode = false,
            other if other.starts_with(':') => {
                print_error(
                    &CliError::usage(format!("unknown `{other}` (:help)")),
                    json_mode,
                )?;
            }
            _ => run_line(&mut session, &registry, global, line, json_mode)?,
        }
    }
    if let Some(path) = session.save(global)? {
        out::diagnostic(format!("saved {path}"));
    }
    Ok(())
}

/// Runs one batch line; errors are reported and the shell goes on.
fn run_line(
    session: &mut Session,
    registry: &CommandRegistry,
    global: &Global,
    line: &str,
    json_mode: bool,
) -> Result<(), CliError> {
    let commands = match shell::split_line(line) {
        Ok(c) => c,
        Err(e) => return print_error(&CliError::usage(e), json_mode),
    };
    for words in commands {
        let Some((name, rest)) = words.split_first() else {
            continue;
        };
        let result = (|| -> Result<Value, CliError> {
            let inv = parse_tool_args(registry, name, rest, false)?;
            let params = inv.params(session, global)?;
            let output = session
                .execute(name, params.clone())
                .map_err(CliError::from_command)?;
            session.journal(name, &params, None, global);
            Ok(output)
        })();
        match result {
            Ok(output) => print_output(name, &output, json_mode)?,
            // `--help` printed its text and asked to stop: not an error.
            Err(e) if e.exit == 0 => {}
            Err(e) => {
                print_error(&e, json_mode)?;
                // The rest of the line depends on this command.
                break;
            }
        }
    }
    Ok(())
}

fn report(
    result: Result<Value, planyrith_core::commands::CommandError>,
    json_mode: bool,
) -> Result<(), CliError> {
    match result {
        Ok(v) => print_output("history", &v, json_mode),
        Err(e) => print_error(&CliError::from_command(e), json_mode),
    }
}

fn print_output(command: &str, output: &Value, json_mode: bool) -> Result<(), CliError> {
    if json_mode {
        out::line(json!({ "ok": true, "command": command, "output": output }))
    } else {
        out::line(serde_json::to_string_pretty(output).unwrap_or_default())
    }
}

fn print_error(e: &CliError, json_mode: bool) -> Result<(), CliError> {
    if json_mode {
        out::line(e.to_json())
    } else {
        match &e.code {
            Some(code) => out::diagnostic(format!("error[{code}]: {}", e.message)),
            None => out::diagnostic(format!("error: {}", e.message)),
        }
        Ok(())
    }
}
