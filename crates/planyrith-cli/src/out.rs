//! Standard output and error (docs/08 §7, §8). A reader that goes away
//! (`planyrith … | head`) ends the program quietly: writes report it as an
//! already-reported error with the success exit code instead of panicking.

use std::fmt::Display;
use std::io::{self, Write};

use crate::CliError;

/// Writes `text` and a newline to standard output.
pub fn line(text: impl Display) -> Result<(), CliError> {
    let mut out = io::stdout().lock();
    writeln!(out, "{text}")
        .and_then(|()| out.flush())
        .map_err(write_error)
}

/// Writes raw bytes to standard output (a document saved to `-`).
pub fn bytes(data: &[u8]) -> Result<(), CliError> {
    let mut out = io::stdout().lock();
    out.write_all(data)
        .and_then(|()| out.flush())
        .map_err(write_error)
}

/// Writes `text` and a newline to standard error; failures are ignored
/// (diagnostics have nowhere else to go).
pub fn diagnostic(text: impl Display) {
    let _ = writeln!(io::stderr().lock(), "{text}");
}

/// A closed pipe ends quietly (exit 0, nothing left to print); any other
/// write failure is an I/O error (exit 4).
fn write_error(e: io::Error) -> CliError {
    if e.kind() == io::ErrorKind::BrokenPipe {
        CliError {
            exit: 0,
            code: None,
            message: String::new(),
            param: None,
            reported: true,
        }
    } else {
        CliError::io(planyrith_core::io::IoError::storage(
            "standard output",
            e.to_string(),
        ))
    }
}
