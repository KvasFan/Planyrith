//! Optional SKP converter process (docs/09 §5); GPL-3.0-only.
use std::io::{Read, Write};
use std::process::ExitCode;
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let operation = std::env::args().nth(1).ok_or(
        "usage: planyrith-skp decode|encode-preserve|encode-2017|encode-2026 < input > response",
    )?;
    if operation == "--version" {
        println!(
            "planyrith-skp {} (GPL-3.0-only; openskp 0.5.0)",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(planyrith_core::io::adapter::MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let result = planyrith_skp::convert(&operation, &bytes)?;
    let output = planyrith_core::io::adapter::encode(&result)?;
    std::io::stdout().lock().write_all(&output)?;
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
