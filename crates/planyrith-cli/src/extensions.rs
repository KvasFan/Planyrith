//! Implements docs/10 §3 Extension Manager and plugin discovery for the CLI.

use crate::CliError;
use clap::Subcommand;
use planyrith_core::commands::CommandRegistry;
use planyrith_core::io::storage::NativeFs;
use planyrith_core::io::{Storage, StorageKey};
use planyrith_core::scripting::plugins::{self, Plugin};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Plugin management operations.
#[derive(Debug, Subcommand)]
pub enum ExtensionAction {
    /// Validate and install a .plyrx archive (no code executes on install).
    Install {
        /// Archive to install.
        archive: PathBuf,
    },
    /// Validate a plugin directory or .plyrx archive.
    Check {
        /// Directory or archive.
        plugin: PathBuf,
    },
    /// List bundled and installed plugins.
    List,
}
/// User plugin directory; tests and portable installations can override it.
pub fn root() -> String {
    if let Ok(root) = std::env::var("PLANYRITH_PLUGIN_DIR") {
        return root;
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".planyrith"));
    base.join("planyrith/plugins")
        .to_string_lossy()
        .replace('\\', "/")
}
/// Registry shared by CLI verbs, scripts and JSON-RPC.
pub fn registry() -> Result<CommandRegistry, CliError> {
    plugins::registry(&NativeFs::new(), &root()).map_err(CliError::from_command)
}

fn load(path: &Path) -> Result<Plugin, CliError> {
    if path.is_dir() {
        fn visit(
            base: &Path,
            path: &Path,
            files: &mut std::collections::BTreeMap<String, Vec<u8>>,
        ) -> Result<(), CliError> {
            for entry in std::fs::read_dir(path).map_err(|e| CliError::usage(e.to_string()))? {
                let entry = entry.map_err(|e| CliError::usage(e.to_string()))?;
                let kind = entry
                    .file_type()
                    .map_err(|e| CliError::usage(e.to_string()))?;
                if kind.is_symlink() {
                    return Err(CliError::usage(
                        "plugin folders cannot contain symbolic links",
                    ));
                }
                if kind.is_dir() {
                    visit(base, &entry.path(), files)?;
                } else {
                    let path = entry.path();
                    let relative = path
                        .strip_prefix(base)
                        .map_err(|e| CliError::usage(e.to_string()))?
                        .to_string_lossy()
                        .replace('\\', "/");
                    files.insert(
                        relative,
                        NativeFs::new()
                            .read(&StorageKey::new(path.to_string_lossy().to_string()))
                            .map_err(CliError::io)?,
                    );
                }
                if files.len() > 256 {
                    return Err(CliError::usage("too many plugin files"));
                }
            }
            Ok(())
        }
        let mut files = std::collections::BTreeMap::new();
        visit(path, path, &mut files)?;
        Plugin::from_files(files).map_err(CliError::from_command)
    } else {
        Plugin::from_archive(
            &NativeFs::new()
                .read(&StorageKey::new(path.to_string_lossy().to_string()))
                .map_err(CliError::io)?,
        )
        .map_err(CliError::from_command)
    }
}
/// Executes one Extension Manager operation.
pub fn manage(action: ExtensionAction) -> Result<Value, CliError> {
    match action {
        ExtensionAction::Install { archive } => {
            let bytes = NativeFs::new()
                .read(&StorageKey::new(archive.to_string_lossy().to_string()))
                .map_err(CliError::io)?;
            let info = plugins::install(&NativeFs::new(), &root(), &bytes)
                .map_err(CliError::from_command)?;
            Ok(json!({"installed":info}))
        }
        ExtensionAction::Check { plugin } => {
            let plugin = load(&plugin)?;
            Ok(
                json!({"valid":true,"plugin":plugin.manifest.plugin,"tools":plugin.manifest.tools.iter().map(|t|&t.name).collect::<Vec<_>>()}),
            )
        }
        ExtensionAction::List => {
            let mut rows = Vec::new();
            for plugin in plugins::bundled().map_err(CliError::from_command)? {
                rows.push(json!({"source":"bundled","plugin":plugin.manifest.plugin}));
            }
            for plugin in
                plugins::installed(&NativeFs::new(), &root()).map_err(CliError::from_command)?
            {
                rows.push(json!({"source":"installed","plugin":plugin.manifest.plugin}));
            }
            Ok(Value::Array(rows))
        }
    }
}
