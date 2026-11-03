//! Packages the same three independently executed programs for each platform.
use super::{
    BINARIES, Options, Result, VERSION, copy, copy_tree, executable, output, run, stage, write,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn reset(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    std::fs::create_dir_all(path)?;
    Ok(())
}
fn prefix(options: &Options, stage: &Path, root: &Path) -> Result<()> {
    reset(root)?;
    copy_tree(&stage.join("bin"), &root.join("usr/bin"))?;
    copy_tree(
        &stage.join("licenses"),
        &root.join("usr/share/licenses/planyrith"),
    )?;
    for file in ["INSTALL.md", "CHANGELOG.md", "BUILD-INFO.txt"] {
        copy(
            &stage.join(file),
            &root.join("usr/share/doc/planyrith").join(file),
        )?;
    }
    for (from, to) in [
        (
            "packaging/linux/org.planyrith.Planyrith.desktop",
            "usr/share/applications/org.planyrith.Planyrith.desktop",
        ),
        (
            "packaging/linux/org.planyrith.Planyrith.xml",
            "usr/share/mime/packages/org.planyrith.Planyrith.xml",
        ),
        (
            "assets/brand/logo-light.svg",
            "usr/share/icons/hicolor/scalable/apps/org.planyrith.Planyrith.svg",
        ),
        (
            "packaging/icons/planyrith-256.png",
            "usr/share/icons/hicolor/256x256/apps/org.planyrith.Planyrith.png",
        ),
    ] {
        copy(&options.root.join(from), &root.join(to))?;
    }
    Ok(())
}
fn epoch(options: &Options) -> Result<String> {
    Ok(String::from_utf8(output(
        Command::new("git")
            .current_dir(&options.root)
            .args(["show", "-s", "--format=%ct", "HEAD"]),
    )?)?
    .trim()
    .to_owned())
}
fn tar(root: &Path, destination: &Path, epoch: &str, compression: &str) -> Result<()> {
    run(Command::new("tar")
        .args([
            "--sort=name",
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            "--transform=s,^\\./,,",
            &format!("--mtime=@{epoch}"),
            compression,
            "-cf",
        ])
        .arg(destination)
        .arg("-C")
        .arg(root)
        .arg("."))
}
fn arch_version(version: &str) -> String {
    version
        .replace('-', ".")
        .replace(".alpha.", "alpha")
        .replace(".beta.", "beta")
        .replace(".rc.", "rc")
}

pub(super) fn linux(options: &Options) -> Result<()> {
    let stage = stage(options, false)?;
    let epoch = epoch(options)?;
    tar(
        &stage,
        &options
            .out
            .join(format!("planyrith-{VERSION}-linux-x86_64.tar.gz")),
        &epoch,
        "-z",
    )?;
    let deb = options.out.join("deb-root");
    prefix(options, &stage, &deb)?;
    let deb_version = VERSION.replace('-', "~");
    let control = format!(
        "Package: planyrith\nVersion: {deb_version}\nSection: graphics\nPriority: optional\nArchitecture: amd64\nMaintainer: KvasFan <planyrith@users.noreply.github.com>\nHomepage: https://github.com/KvasFan/Planyrith\nDepends: libc6 (>= 2.35), libgcc-s1, libx11-6, libxcursor1, libxi6, libxrandr2, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libwayland-cursor0, libwayland-egl1, libegl1, libgl1, libvulkan1, shared-mime-info, desktop-file-utils\nRecommends: mesa-vulkan-drivers\nDescription: Precise 3D modeling, documentation and automation\n Native PLYR modeling with GUI, CLI and an optional separate GPL SKP/SKB module.\n"
    );
    write(&deb.join("DEBIAN/control"), control)?;
    let deb_file = options.out.join(format!("planyrith_{VERSION}_amd64.deb"));
    run(Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&deb)
        .arg(&deb_file))?;
    run(Command::new("dpkg-deb").arg("--info").arg(&deb_file))?;
    let arch = options.out.join("arch-root");
    prefix(options, &stage, &arch)?;
    let pkgver = arch_version(VERSION);
    let pkginfo = format!(
        "pkgname = planyrith\npkgbase = planyrith\npkgver = {pkgver}-1\npkgdesc = Precise 3D modeling, documentation and automation\nurl = https://github.com/KvasFan/Planyrith\nbuilddate = {epoch}\npackager = KvasFan\nsize = {}\narch = x86_64\nlicense = MIT\nlicense = Apache-2.0\nlicense = GPL-3.0-only\nlicense = OFL-1.1\ndepend = glibc\ndepend = gcc-libs\ndepend = libx11\ndepend = libxcursor\ndepend = libxi\ndepend = libxrandr\ndepend = libxkbcommon\ndepend = libxkbcommon-x11\ndepend = wayland\ndepend = libglvnd\ndepend = vulkan-icd-loader\ndepend = shared-mime-info\ndepend = desktop-file-utils\noptdepend = vulkan-driver: hardware-accelerated viewport\n",
        directory_size(&arch)?
    );
    write(&arch.join(".PKGINFO"), pkginfo)?;
    tar(
        &arch,
        &options
            .out
            .join(format!("planyrith-{pkgver}-1-x86_64.pkg.tar.zst")),
        &epoch,
        "--zstd",
    )?;
    let appdir = options.out.join("Planyrith.AppDir");
    prefix(options, &stage, &appdir)?;
    copy(
        &options.root.join("packaging/linux/AppRun"),
        &appdir.join("AppRun"),
    )?;
    executable(&appdir.join("AppRun"))?;
    copy(
        &options
            .root
            .join("packaging/linux/org.planyrith.Planyrith.desktop"),
        &appdir.join("org.planyrith.Planyrith.desktop"),
    )?;
    copy(
        &options.root.join("assets/brand/logo-light.svg"),
        &appdir.join("org.planyrith.Planyrith.svg"),
    )?;
    copy(
        &options.root.join("packaging/icons/planyrith-256.png"),
        &appdir.join(".DirIcon"),
    )?;
    let tool = options
        .appimagetool
        .as_ref()
        .ok_or("--appimagetool is required")?;
    let runtime = options.runtime.as_ref().ok_or("--runtime is required")?;
    let runtime_license = runtime.with_file_name("RUNTIME-LICENSE.txt");
    copy(
        &runtime_license,
        &appdir.join("usr/share/licenses/planyrith/AppImage-runtime.txt"),
    )?;
    let appimage = options
        .out
        .join(format!("Planyrith-{VERSION}-x86_64.AppImage"));
    run(Command::new(tool)
        .env("ARCH", "x86_64")
        .env("SOURCE_DATE_EPOCH", &epoch)
        .args([
            "--appimage-extract-and-run",
            "--no-appstream",
            "--runtime-file",
        ])
        .arg(runtime)
        .arg(&appdir)
        .arg(&appimage))?;
    executable(&appimage)?;
    let smoke =
        output(Command::new(&appimage).args(["--appimage-extract-and-run", "--cli", "--version"]))?;
    if !String::from_utf8_lossy(&smoke).contains(VERSION) {
        return Err("AppImage version smoke failed".into());
    }
    Ok(())
}
fn directory_size(directory: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        total += if entry.file_type()?.is_dir() {
            directory_size(&entry.path())?
        } else {
            entry.metadata()?.len()
        };
    }
    Ok(total)
}
pub(super) fn windows(options: &Options) -> Result<()> {
    let stage = stage(options, true)?;
    let installer = options
        .out
        .join(format!("Planyrith-{VERSION}-windows-x86_64-setup.exe"));
    let compiler = std::env::var_os("MAKENSIS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("makensis"));
    let delimiter = if cfg!(windows) { "/" } else { "-" };
    run(Command::new(compiler)
        .arg(format!("{delimiter}DVERSION={VERSION}"))
        .arg(format!("{delimiter}DPAYLOAD={}", stage.display()))
        .arg(format!("{delimiter}DOUTPUT={}", installer.display()))
        .arg(format!(
            "{delimiter}DICON={}",
            options.root.join("packaging/icons/planyrith.ico").display()
        ))
        .arg(options.root.join("packaging/windows/installer.nsi")))?;
    let zip = options
        .out
        .join(format!("planyrith-{VERSION}-windows-x86_64-portable.zip"));
    // Use structured PowerShell parameters rather than interpolated commands.
    run(Command::new("powershell")
        .args(["-NoProfile", "-File"])
        .arg(options.root.join("packaging/windows/portable.ps1"))
        .arg(&stage)
        .arg(&zip))?;
    for name in BINARIES {
        if !stage.join(format!("{name}.exe")).is_file() {
            return Err("incomplete Windows payload".into());
        }
    }
    Ok(())
}

/// The Flatpak application id, also the desktop id (docs/16 §1).
const APP_ID: &str = "org.planyrith.Planyrith";
/// Freedesktop runtime the bundle runs on.
const FLATPAK_RUNTIME: (&str, &str, &str) =
    ("org.freedesktop.Platform", "org.freedesktop.Sdk", "26.08");

/// `cargo xtask flatpak`: the Linux payload as a single-file Flatpak bundle,
/// built with the `flatpak` command's own build steps from the release
/// binaries (no rebuild inside the SDK). The installed programs are tried in
/// the sandbox before the build is finished.
pub(super) fn flatpak(options: &Options) -> Result<()> {
    let stage = stage(options, false)?;
    let build = options.out.join("flatpak-build");
    let repo = options.out.join("flatpak-repo");
    for dir in [&build, &repo] {
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
    }
    let (runtime, sdk, branch) = FLATPAK_RUNTIME;
    run(Command::new("flatpak")
        .args(["build-init"])
        .arg(&build)
        .args([APP_ID, sdk, runtime, branch]))?;
    let usr = options.out.join("flatpak-prefix");
    prefix(options, &stage, &usr)?;
    copy_tree(&usr.join("usr"), &build.join("files"))?;
    std::fs::remove_dir_all(&usr)?;
    copy(
        &options
            .root
            .join("packaging/linux/org.planyrith.Planyrith.metainfo.xml"),
        &build.join("files/share/metainfo/org.planyrith.Planyrith.metainfo.xml"),
    )?;
    let smoke = output(
        Command::new("flatpak")
            .arg("build")
            .arg(&build)
            .args(["/app/bin/planyrith", "--version"]),
    )?;
    if !String::from_utf8_lossy(&smoke).contains(VERSION) {
        return Err("Flatpak version smoke failed".into());
    }
    // The format module beside the programs opens a historical model.
    let fixtures = options
        .root
        .join("crates/planyrith-skp/tests/fixtures/legacy");
    let info: serde_json::Value = serde_json::from_slice(&output(
        Command::new("flatpak")
            .arg("build")
            .arg(format!("--filesystem={}:ro", fixtures.display()))
            .arg(&build)
            .args(["/app/bin/planyrith", "--json", "-f"])
            .arg(fixtures.join("box-2013.skp"))
            .arg("info"),
    )?)?;
    if info["ok"] != true
        || info["output"]["problems"] != 0
        || info["output"]["counts"]["model"]["faces"] != 6
    {
        return Err(format!("Flatpak format module smoke failed: {info}").into());
    }
    run(Command::new("flatpak")
        .arg("build-finish")
        .arg(&build)
        .args([
            "--command=planyrith-gui",
            "--share=ipc",
            "--socket=wayland",
            "--socket=fallback-x11",
            "--device=dri",
            // The interface's own file dialog reads and writes where the user
            // chooses, so the home folder is reachable.
            "--filesystem=home",
        ]))?;
    run(Command::new("flatpak")
        .arg("build-export")
        .arg(&repo)
        .arg(&build))?;
    run(Command::new("flatpak")
        .arg("build-bundle")
        .arg(&repo)
        .arg(
            options
                .out
                .join(format!("Planyrith-{VERSION}-x86_64.flatpak")),
        )
        .args([
            APP_ID,
            "--runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo",
        ]))?;
    Ok(())
}
