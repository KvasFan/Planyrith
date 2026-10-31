//! Produces complete offline Cargo sources for the separately shipped GPL module.
use super::{Options, Result, VERSION, copy, output, run, write};
use std::{path::Path, process::Command};

pub(super) fn bundle(options: &Options) -> Result<()> {
    let name = format!("planyrith-{VERSION}-source");
    let destination = options.out.join(&name);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::create_dir_all(&destination)?;
    let files = output(
        Command::new("git")
            .current_dir(&options.root)
            .args(["ls-files", "-z"]),
    )?;
    for path in files.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let relative = Path::new(std::str::from_utf8(path)?);
        if relative.is_absolute()
            || relative
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            return Err("unsafe source path".into());
        }
        copy(&options.root.join(relative), &destination.join(relative))?;
    }
    let configuration = output(
        Command::new("cargo")
            .current_dir(&options.root)
            .args(["vendor", "--locked"])
            .arg(destination.join("cargo-vendor")),
    )?;
    let configuration = String::from_utf8(configuration)?.replace(
        &destination
            .join("cargo-vendor")
            .to_string_lossy()
            .to_string(),
        "cargo-vendor",
    );
    let original = std::fs::read_to_string(destination.join(".cargo/config.toml"))?;
    write(
        &destination.join(".cargo/config.toml"),
        format!("{original}\n{configuration}"),
    )?;
    write(
        &destination.join("SOURCE-README.txt"),
        "Build offline: cargo build --workspace --release --locked --offline --features planyrith-cli/render\nThe applications are MIT OR Apache-2.0; the optional planyrith-skp process and openskp are GPL-3.0-only.\nCargo dependencies, including openskp, are included in cargo-vendor/. The project patch remains in vendor/truck-shapeops/.\nAppImage runtime corresponding sources are distributed separately beside this source archive.\n",
    )?;
    let archive = options.out.join(format!("{name}.tar.gz"));
    run(Command::new("tar")
        .arg("-czf")
        .arg(archive)
        .arg("-C")
        .arg(&options.out)
        .arg(name))
}
