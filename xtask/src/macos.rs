//! The macOS application bundle and disk image (docs/16 §1, 1.0).
//!
//! `cargo xtask macos --bin-dir DIR` lays out `Planyrith.app` with the three
//! programs side by side in `Contents/MacOS` (the interface finds the format
//! module beside itself), writes `Info.plist` with the `.plyr` document type,
//! and the icon. On macOS it then makes `Planyrith-<version>-macos.dmg` with
//! `hdiutil` (override with `HDIUTIL`); elsewhere it stops after the bundle.
use super::{BINARIES, Options, Result, VERSION, copy, executable, licenses, run, write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The numeric version macOS expects in `CFBundleShortVersionString`.
fn short_version() -> &'static str {
    VERSION.split('-').next().unwrap_or(VERSION)
}

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>org.planyrith.Planyrith</string>
  <key>CFBundleName</key><string>Planyrith</string>
  <key>CFBundleDisplayName</key><string>Planyrith</string>
  <key>CFBundleExecutable</key><string>planyrith-gui</string>
  <key>CFBundleIconFile</key><string>Planyrith</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{short}</string>
  <key>CFBundleVersion</key><string>{VERSION}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.graphics-design</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Planyrith model</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Owner</string>
      <key>CFBundleTypeIconFile</key><string>Planyrith</string>
      <key>LSItemContentTypes</key><array><string>org.planyrith.model</string></array>
    </dict>
  </array>
  <key>UTExportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>org.planyrith.model</string>
      <key>UTTypeDescription</key><string>Planyrith model</string>
      <key>UTTypeConformsTo</key><array><string>public.data</string><string>public.zip-archive</string></array>
      <key>UTTypeIconFile</key><string>Planyrith</string>
      <key>UTTypeTagSpecification</key>
      <dict>
        <key>public.filename-extension</key><array><string>plyr</string></array>
        <key>public.mime-type</key><string>application/x-planyrith</string>
      </dict>
    </dict>
  </array>
</dict>
</plist>
"#,
        short = short_version(),
    )
}

/// An `.icns` holding one 256×256 PNG (type `ic08`): the container is a
/// header and length-prefixed entries, so no image tool is needed.
fn icns(png: &[u8]) -> Result<Vec<u8>> {
    if png.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err("the application icon is not a PNG".into());
    }
    let entry = u32::try_from(png.len() + 8)?;
    let total = entry + 8;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(b"ic08");
    out.extend_from_slice(&entry.to_be_bytes());
    out.extend_from_slice(png);
    Ok(out)
}

/// Lays out `Planyrith.app` under `dir`; returns its path.
fn bundle(options: &Options, dir: &Path) -> Result<PathBuf> {
    let app = dir.join("Planyrith.app");
    if app.exists() {
        std::fs::remove_dir_all(&app)?;
    }
    let contents = app.join("Contents");
    for binary in BINARIES {
        let source = options.bins.join(binary);
        if !source.is_file() {
            return Err(format!("missing required release binary {}", source.display()).into());
        }
        let destination = contents.join("MacOS").join(binary);
        copy(&source, &destination)?;
        executable(&destination)?;
    }
    write(&contents.join("Info.plist"), info_plist())?;
    write(&contents.join("PkgInfo"), "APPL????")?;
    let png = std::fs::read(options.root.join("packaging/icons/planyrith-256.png"))?;
    write(&contents.join("Resources/Planyrith.icns"), icns(&png)?)?;
    licenses::collect(&options.root, &contents.join("Resources/licenses"))?;
    for file in ["packaging/INSTALL.md", "CHANGELOG.md"] {
        let name = Path::new(file).file_name().ok_or("file name")?;
        copy(
            &options.root.join(file),
            &contents.join("Resources").join(name),
        )?;
    }
    Ok(app)
}

/// `cargo xtask macos`.
pub(super) fn build(options: &Options) -> Result<()> {
    let image = options.out.join("macos-image");
    if image.exists() {
        std::fs::remove_dir_all(&image)?;
    }
    let app = bundle(options, &image)?;
    if !cfg!(target_os = "macos") && std::env::var_os("HDIUTIL").is_none() {
        println!("{}: the disk image needs macOS", app.display());
        return Ok(());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("/Applications", image.join("Applications"))?;
    copy(
        &options.root.join("packaging/INSTALL.md"),
        &image.join("INSTALL.md"),
    )?;
    let hdiutil =
        std::env::var_os("HDIUTIL").map_or_else(|| PathBuf::from("hdiutil"), PathBuf::from);
    let dmg = options.out.join(format!("Planyrith-{VERSION}-macos.dmg"));
    run(Command::new(hdiutil)
        .args([
            "create",
            "-volname",
            "Planyrith",
            "-fs",
            "HFS+",
            "-format",
            "UDZO",
            "-ov",
            "-srcfolder",
        ])
        .arg(&image)
        .arg(&dmg))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_and_property_list() -> Result<()> {
        let png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        let icon = icns(&png)?;
        assert_eq!(&icon[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(icon[4..8].try_into()?) as usize,
            icon.len()
        );
        assert_eq!(&icon[8..12], b"ic08");
        assert_eq!(&icon[16..], &png[..]);
        assert!(icns(b"GIF89a").is_err());
        let plist = info_plist();
        assert!(plist.contains("<string>planyrith-gui</string>"));
        assert!(plist.contains(&format!("<string>{}</string>", short_version())));
        assert!(!short_version().contains('-'));
        Ok(())
    }

    #[test]
    fn bundle_layout() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("root")?;
        let dir = std::env::temp_dir().join(format!("planyrith-macos-{}", std::process::id()));
        for binary in BINARIES {
            write(&dir.join("bins").join(binary), "x")?;
        }
        let options = Options {
            root: root.to_path_buf(),
            bins: dir.join("bins"),
            out: dir.clone(),
            appimagetool: None,
            runtime: None,
        };
        let app = bundle(&options, &dir)?;
        for path in [
            "Contents/Info.plist",
            "Contents/MacOS/planyrith-gui",
            "Contents/MacOS/planyrith-skp",
            "Contents/Resources/Planyrith.icns",
            "Contents/Resources/licenses",
        ] {
            assert!(app.join(path).exists(), "{path}");
        }
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
