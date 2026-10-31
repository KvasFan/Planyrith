//! Copies dependency attribution into each independently usable payload.
use super::{Result, copy, copy_tree, output, write};
use std::{path::Path, process::Command};

pub(super) fn collect(root: &Path, destination: &Path) -> Result<()> {
    let metadata: serde_json::Value =
        serde_json::from_slice(&output(Command::new("cargo").current_dir(root).args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
        ]))?)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?;
    let mut index = String::from(
        "# Third-party licenses\n\nApplications: MIT OR Apache-2.0. The optional, independently executed\nplanyrith-skp and openskp component is GPL-3.0-only. Removing the helper\ndisables SKP/SKB support and leaves native modeling operational.\n\n",
    );
    for package in packages {
        let name = package["name"].as_str().ok_or("package name missing")?;
        let version = package["version"]
            .as_str()
            .ok_or("package version missing")?;
        let manifest = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("manifest path missing")?,
        );
        let directory = manifest.parent().ok_or("manifest parent missing")?;
        let folder = destination.join(format!("{name}-{version}"));
        let mut found = false;
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let label = entry.file_name().to_string_lossy().to_uppercase();
            if ["LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE"]
                .iter()
                .any(|prefix| label.starts_with(prefix))
            {
                if entry.file_type()?.is_file() {
                    copy(&entry.path(), &folder.join(entry.file_name()))?;
                    found = true;
                } else if entry.file_type()?.is_dir() {
                    copy_tree(&entry.path(), &folder.join(entry.file_name()))?;
                    found = true;
                }
            }
        }
        if let Some(path) = package["license_file"].as_str() {
            let source = directory.join(path);
            if source.is_file() {
                copy(&source, &folder.join("DECLARED-LICENSE"))?;
                found = true;
            }
        }
        if !found && directory.starts_with(root) {
            copy(&root.join("LICENSE-MIT"), &folder.join("LICENSE-MIT"))?;
            copy(&root.join("LICENSE-APACHE"), &folder.join("LICENSE-APACHE"))?;
            found = true;
        }
        // Keep author/repository attribution even when an upstream crate omits
        // a standalone license file. Its full source is also distributed.
        copy(manifest, &folder.join("Cargo.toml"))?;
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .to_uppercase()
                    .starts_with("README")
            {
                copy(&entry.path(), &folder.join(entry.file_name()))?;
            }
        }
        index.push_str(&format!(
            "- {name} {version}: {}{}\n",
            package["license"]
                .as_str()
                .unwrap_or("see declared license file"),
            if found {
                ""
            } else {
                " (see package source for full attribution)"
            }
        ));
    }
    copy(
        &root.join("LICENSE-MIT"),
        &destination.join("Planyrith-MIT.txt"),
    )?;
    copy(
        &root.join("LICENSE-APACHE"),
        &destination.join("Planyrith-Apache-2.0.txt"),
    )?;
    copy(
        &root.join("crates/planyrith-skp/LICENSE"),
        &destination.join("SKP-GPL-3.0.txt"),
    )?;
    copy(
        &root.join("crates/planyrith-skp/tests/fixtures/intermediate/LICENSE"),
        &destination.join("Classic-reader-MIT.txt"),
    )?;
    copy(
        &root.join("crates/planyrith-core/assets/fonts/Inter-LICENSE.txt"),
        &destination.join("Inter-font.txt"),
    )?;
    write(&destination.join("INDEX.md"), index)
}
