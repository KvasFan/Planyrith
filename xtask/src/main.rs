//! Release staging and platform packages (docs/12 §4 and docs/16).
mod licenses;
mod packages;
mod source;

use std::{
    error::Error,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const VERSION: &str = env!("CARGO_PKG_VERSION");
const BINARIES: [&str; 3] = ["planyrith", "planyrith-gui", "planyrith-skp"];

struct Options {
    root: PathBuf,
    bins: PathBuf,
    out: PathBuf,
    appimagetool: Option<PathBuf>,
    runtime: Option<PathBuf>,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("missing workspace")?
            .to_path_buf();
        let mut options = Self {
            bins: root.join("target/release"),
            out: root.join("dist/release"),
            root,
            appimagetool: None,
            runtime: None,
        };
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--bin-dir" => options.bins = PathBuf::from(value),
                "--out" => options.out = PathBuf::from(value),
                "--appimagetool" => options.appimagetool = Some(PathBuf::from(value)),
                "--runtime" => options.runtime = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown option {flag}").into()),
            }
        }
        std::fs::create_dir_all(&options.out)?;
        options.out = std::fs::canonicalize(&options.out)?;
        Ok(options)
    }
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("command failed ({status}): {command:?}").into());
    }
    Ok(())
}
fn output(command: &mut Command) -> Result<Vec<u8>> {
    let result = command.output()?;
    if !result.status.success() {
        return Err(format!(
            "command failed: {command:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    Ok(result.stdout)
}
fn write(path: &Path, data: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)?;
    Ok(())
}
fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to)?;
    Ok(())
}
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    let mut paths = std::fs::read_dir(from)?.collect::<std::result::Result<Vec<_>, _>>()?;
    paths.sort_by_key(|entry| entry.file_name());
    for entry in paths {
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        } else if entry.file_type()?.is_file() {
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
    }
    Ok(())
}
fn executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
fn stage(options: &Options, windows: bool) -> Result<PathBuf> {
    let name = if windows {
        "windows-x86_64"
    } else {
        "linux-x86_64"
    };
    let stage = options.out.join(format!("planyrith-{VERSION}-{name}"));
    if stage.exists() {
        std::fs::remove_dir_all(&stage)?;
    }
    for binary in BINARIES {
        let name = format!("{binary}{}", if windows { ".exe" } else { "" });
        let source = options.bins.join(&name);
        if !source.is_file() {
            return Err(format!(
                "missing required release binary {} (build --workspace --release)",
                source.display()
            )
            .into());
        }
        let destination = stage.join(if windows { "" } else { "bin" }).join(name);
        copy(&source, &destination)?;
        executable(&destination)?;
    }
    licenses::collect(&options.root, &stage.join("licenses"))?;
    copy(
        &options.root.join("packaging/INSTALL.md"),
        &stage.join("INSTALL.md"),
    )?;
    copy(
        &options.root.join("CHANGELOG.md"),
        &stage.join("CHANGELOG.md"),
    )?;
    if !windows {
        copy(
            &options.root.join("packaging/linux/Planyrith"),
            &stage.join("Planyrith"),
        )?;
        executable(&stage.join("Planyrith"))?;
    } else {
        copy(
            &options.root.join("packaging/icons/planyrith.ico"),
            &stage.join("Planyrith.ico"),
        )?;
    }
    let revision = String::from_utf8(output(
        Command::new("git")
            .current_dir(&options.root)
            .args(["rev-parse", "HEAD"]),
    )?)?;
    write(
        &stage.join("BUILD-INFO.txt"),
        format!(
            "Planyrith {VERSION}\nTarget: {name}\nCommit: {}\nApplications: MIT OR Apache-2.0\nOptional planyrith-skp process: GPL-3.0-only\n",
            revision.trim()
        ),
    )?;
    Ok(stage)
}
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = (|| -> Result<()> {
        let action = args.first().ok_or("usage: cargo xtask linux|windows|source|licenses [--bin-dir PATH] [--out PATH] [--appimagetool PATH] [--runtime PATH]")?;
        let options = Options::parse(&args[1..])?;
        match action.as_str() {
            "linux" => packages::linux(&options),
            "windows" => packages::windows(&options),
            "source" => source::bundle(&options),
            "licenses" => licenses::collect(&options.root, &options.out.join("licenses")),
            _ => Err(format!("unknown action {action}").into()),
        }
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
