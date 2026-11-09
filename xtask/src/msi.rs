//! The Windows Installer package (docs/16 §1, 1.0): a per-user MSI with the
//! same payload as the setup program, built from WiX source by `wixl`
//! (msitools), a build tool outside the application's dependencies.
//!
//! Component and product codes are derived from each file's path and the
//! version, so rebuilding a version keeps them. Windows Installer versions are
//! numeric: `0.11.0-alpha.4` installs as `0.11.0`, and a later pre-release of
//! the same numeric version still replaces it.
use super::{Options, Result, VERSION, run, stage, write};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Same for every Planyrith MSI, so a newer one replaces an older one.
const UPGRADE_CODE: &str = "6A1F5E3C-2B7D-4C8E-9F10-3D2A4B5C6E70";

/// A deterministic GUID from `seed` (FNV-1a over two lanes, formatted as a
/// version-4-shaped GUID). Not cryptographic; only stable and distinct.
fn guid(seed: &str) -> String {
    let mut lanes = [0xcbf2_9ce4_8422_2325_u64, 0x8422_2325_cbf2_9ce4_u64];
    for (i, lane) in lanes.iter_mut().enumerate() {
        for byte in seed.bytes().chain([i as u8]) {
            *lane ^= u64::from(byte);
            *lane = lane.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let hex = format!("{:016X}{:016X}", lanes[0], lanes[1]);
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

/// An identifier WiX accepts: letters, digits, `_` and `.`, from a seed.
fn ident(prefix: &str, seed: &str) -> String {
    format!("{prefix}_{}", &guid(seed).replace('-', "")[..20])
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The numeric version Windows Installer accepts.
fn msi_version() -> String {
    VERSION.split('-').next().unwrap_or(VERSION).to_owned()
}

/// Files and folders below `dir`, sorted, as paths relative to `root`.
fn walk(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, files)?;
        } else {
            files.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

/// The component ids of a payload, split by feature.
#[derive(Default)]
struct Features {
    core: Vec<String>,
    skp: Vec<String>,
}

/// The `<Directory>` content for `rel`: one component per file, keyed by a
/// per-user registry value as Windows Installer requires for per-user
/// installs, then the subfolders.
fn folder(
    out: &mut String,
    stage: &Path,
    rel: &Path,
    files: &[PathBuf],
    dirs: &BTreeSet<PathBuf>,
    features: &mut Features,
) {
    for file in files.iter().filter(|f| f.parent() == Some(rel)) {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let key = file.to_string_lossy().replace('\\', "/");
        let id = ident("C", &key);
        let remove = if rel.as_os_str().is_empty() {
            String::new()
        } else {
            format!(
                r#"<RemoveFolder Id="{}" On="uninstall"/>"#,
                ident("R", &key)
            )
        };
        let _ = writeln!(
            out,
            r#"<Component Id="{id}" Guid="{}"><RegistryValue Root="HKCU" Key="Software\Planyrith\Files" Name="{}" Type="integer" Value="1" KeyPath="yes"/><File Id="{}" Name="{}" Source="{}"/>{remove}</Component>"#,
            guid(&format!("component {key}")),
            xml(&key),
            ident("F", &key),
            xml(&name),
            xml(&stage.join(file).to_string_lossy()),
        );
        if name == "planyrith-skp.exe" {
            features.skp.push(id);
        } else {
            features.core.push(id);
        }
    }
    for sub in dirs.iter().filter(|d| d.parent() == Some(rel)) {
        let name = sub
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = writeln!(
            out,
            r#"<Directory Id="{}" Name="{}">"#,
            ident("D", &sub.to_string_lossy().replace('\\', "/")),
            xml(&name)
        );
        folder(out, stage, sub, files, dirs, features);
        out.push_str("</Directory>\n");
    }
}

/// The WiX source for the payload in `stage`.
fn source(options: &Options, stage: &Path) -> Result<String> {
    let mut files = Vec::new();
    walk(stage, stage, &mut files)?;
    let dirs: BTreeSet<PathBuf> = files
        .iter()
        .flat_map(|f| f.ancestors().skip(1))
        .filter(|d| !d.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect();
    let mut tree = String::new();
    let mut features = Features::default();
    folder(
        &mut tree,
        stage,
        Path::new(""),
        &files,
        &dirs,
        &mut features,
    );
    if features.skp.is_empty() || features.core.len() < 3 {
        return Err("incomplete Windows payload for the MSI".into());
    }
    let refs = |ids: &[String]| {
        ids.iter()
            .map(|id| format!(r#"<ComponentRef Id="{id}"/>"#))
            .collect::<String>()
    };
    let version = msi_version();
    let icon = options.root.join("packaging/icons/planyrith.ico");
    Ok(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!-- Generated by `cargo xtask msi` (docs/16); do not edit. -->
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
<Product Id="{product}" Name="Planyrith" Language="1033" Version="{version}" Manufacturer="KvasFan" UpgradeCode="{UPGRADE_CODE}">
<Package InstallerVersion="500" Compressed="yes" InstallScope="perUser" Description="Planyrith {VERSION}" Comments="3D modeling; the SKP module is separate GPL-3.0-only software"/>
<MajorUpgrade AllowSameVersionUpgrades="yes" DowngradeErrorMessage="A newer Planyrith is installed."/>
<Property Id="PLYR_DEFAULT"><RegistrySearch Id="PlyrDefault" Root="HKCU" Key="Software\Classes\.plyr" Name="" Type="raw"/></Property>
<Media Id="1" Cabinet="planyrith.cab" EmbedCab="yes"/>
<Icon Id="Planyrith.ico" SourceFile="{icon}"/>
<Property Id="ARPPRODUCTICON" Value="Planyrith.ico"/>
<Directory Id="TARGETDIR" Name="SourceDir">
<Directory Id="LocalAppDataFolder">
<Directory Id="ProgramsFolder_Local" Name="Programs">
<Directory Id="INSTALLDIR" Name="Planyrith">
{tree}<Component Id="Association" Guid="{association}">
<RegistryValue Root="HKCU" Key="Software\Classes\Planyrith.Model" Type="string" Value="Planyrith model" KeyPath="yes"/>
<RegistryValue Root="HKCU" Key="Software\Classes\Planyrith.Model\DefaultIcon" Type="string" Value="[INSTALLDIR]Planyrith.ico,0"/>
<RegistryValue Root="HKCU" Key="Software\Classes\Planyrith.Model\shell\open\command" Type="string" Value="&quot;[INSTALLDIR]planyrith-gui.exe&quot; &quot;%1&quot;"/>
<RegistryValue Root="HKCU" Key="Software\Classes\.plyr\OpenWithProgids" Name="Planyrith.Model" Type="string" Value=""/>
<RemoveFolder Id="RemoveInstallDir" On="uninstall"/>
</Component>
<Component Id="DefaultAssociation" Guid="{default_association}">
<Condition>NOT PLYR_DEFAULT OR PLYR_DEFAULT = "Planyrith.Model"</Condition>
<RegistryValue Root="HKCU" Key="Software\Classes\.plyr" Type="string" Value="Planyrith.Model" KeyPath="yes"/>
</Component>
</Directory>
</Directory>
</Directory>
<Directory Id="ProgramMenuFolder">
<Directory Id="MenuFolder" Name="Planyrith">
<Component Id="Shortcuts" Guid="{shortcuts}">
<Shortcut Id="AppShortcut" Name="Planyrith" Target="[INSTALLDIR]planyrith-gui.exe" WorkingDirectory="INSTALLDIR" Icon="Planyrith.ico"/>
<RemoveFolder Id="RemoveMenuFolder" On="uninstall"/>
<RegistryValue Root="HKCU" Key="Software\Planyrith" Name="Shortcuts" Type="integer" Value="1" KeyPath="yes"/>
</Component>
</Directory>
</Directory>
</Directory>
<Feature Id="Core" Title="Planyrith" Level="1" Absent="disallow">{core}<ComponentRef Id="Association"/><ComponentRef Id="DefaultAssociation"/><ComponentRef Id="Shortcuts"/></Feature>
<Feature Id="Skp" Title="SKP / SKB support (separate GPL-3.0-only module)" Level="1">{skp}</Feature>
</Product>
</Wix>
"#,
        product = guid(&format!("product {VERSION}")),
        icon = xml(&icon.to_string_lossy()),
        association = guid("component association"),
        default_association = guid("component default association"),
        shortcuts = guid("component shortcuts"),
        core = refs(&features.core),
        skp = refs(&features.skp),
    ))
}

/// `cargo xtask msi`: stages the Windows payload and builds the MSI.
pub(super) fn build(options: &Options) -> Result<()> {
    let stage = stage(options, true)?;
    let wxs = options
        .out
        .join(format!("Planyrith-{VERSION}-windows-x86_64.wxs"));
    write(&wxs, &source(options, &stage)?)?;
    let msi = options
        .out
        .join(format!("Planyrith-{VERSION}-windows-x86_64.msi"));
    let wixl = std::env::var_os("WIXL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wixl"));
    run(Command::new(wixl)
        .args(["--arch", "x64", "-o"])
        .arg(&msi)
        .arg(&wxs))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_are_stable_well_formed_and_distinct() {
        let a = guid("component planyrith.exe");
        assert_eq!(a, guid("component planyrith.exe"));
        assert_ne!(a, guid("component planyrith-gui.exe"));
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4');
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }

    #[test]
    fn source_splits_features_and_nests_folders() -> Result<()> {
        let stage = std::env::temp_dir().join(format!("planyrith-msi-{}", std::process::id()));
        for file in [
            "planyrith.exe",
            "planyrith-gui.exe",
            "planyrith-skp.exe",
            "licenses/a-1.0/LICENSE",
            "licenses/b & c/NOTICE",
        ] {
            write(&stage.join(file), "x")?;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("root")?;
        let options = Options {
            root: root.to_path_buf(),
            bins: stage.clone(),
            out: stage.clone(),
            appimagetool: None,
            runtime: None,
        };
        let wxs = source(&options, &stage)?;
        let again = source(&options, &stage)?;
        std::fs::remove_dir_all(&stage)?;
        assert_eq!(wxs, again);
        let skp = ident("C", "planyrith-skp.exe");
        let feature = wxs
            .split("<Feature Id=\"Skp\"")
            .nth(1)
            .ok_or("no SKP feature")?;
        assert!(feature.contains(&skp));
        assert_eq!(wxs.matches(&skp).count(), 2);
        assert_eq!(wxs.matches("<Directory Id=\"D_").count(), 3);
        assert!(wxs.contains(r#"Name="b &amp; c""#));
        assert!(wxs.contains(r#"Name="licenses/a-1.0/LICENSE""#));
        Ok(())
    }
}
