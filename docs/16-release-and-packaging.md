# 16 — Release and packaging

**Audience:** users and maintainers. Implements docs/12 §4 release distribution.

## 1. Distributions

Every desktop distribution contains the GUI, CLI (including headless rendering),
and the optional, independently executed `planyrith-skp` format module beside
the application binaries. `.plyr` remains the default working format.

| Platform | Package | Installation |
|---|---|---|
| Windows 10/11 x64 | `*-setup.exe` | Per-user installer, Start menu and optional desktop shortcut/native file association; no administrator privileges |
| Windows x64 | `*-portable.zip` | Extract and run `planyrith-gui.exe` |
| Linux x86_64 | `*.AppImage` | Make executable, then run; `--appimage-extract-and-run` works without FUSE |
| Debian 12 / Ubuntu 22.04 or later | `*_amd64.deb` | `sudo apt install ./planyrith_*_amd64.deb` |
| Arch Linux x86_64 | `*.pkg.tar.zst` | `sudo pacman -U ./planyrith-*-x86_64.pkg.tar.zst` |
| Linux x86_64 | `*-linux-x86_64.tar.gz` | Extract and run `./Planyrith`; `./Planyrith --cli` starts the CLI |

Linux binaries are built on Ubuntu 22.04 (glibc 2.35 baseline). They use system
graphics/windowing drivers; the portable packages do not bundle a Linux desktop.
A working Vulkan implementation is required for the 3D view. The Windows setup
is currently unsigned. Packages install only the native `.plyr` association.
The application icon and desktop id are `org.planyrith.Planyrith`.

## 2. Rebuilding

Application and packaging orchestration code is Rust. With stable Rust, Git,
and the platform's packaging tools available:

```sh
cargo build --workspace --release --locked --features planyrith-cli/render
tools/fetch-packaging-tools.sh
cargo xtask linux --appimagetool "$PWD/target/packaging-tools/appimagetool.AppImage" --runtime "$PWD/target/packaging-tools/runtime-x86_64"
packaging/linux/verify.sh
```

Linux requires GNU tar, zstd, dpkg-deb and desktop-file-utils. Windows uses NSIS
and PowerShell: `cargo xtask windows`, optionally setting `MAKENSIS` to the
compiler path. Output goes to `dist/release/`; `--out` and `--bin-dir` override
the directories. Installation instructions, licenses and build revision travel
with each payload. Packaging from the repository requires a Git checkout.

`cargo xtask source` produces the complete source archive with locked Cargo
dependencies under `cargo-vendor/`, retaining the local `vendor/truck-shapeops`
patch. Its SOURCE-README explains offline application builds. The separate
AppImage runtime source archive includes the loader, libfuse, squashfuse,
patches and build/relink instructions. Application builds never link the format
helper or the AppImage runtime into the GUI or CLI.

## 3. Publication and checks

The manually dispatched `Release` workflow accepts a revision, matching version
tag and a publication flag. It resolves the revision once, builds on Linux and
Windows, creates the source bundles, and exercises extracted/installed programs
outside the build tree. The smoke tests open a historical model through the
adjacent format module, save a native document and validate it. Windows also
installs and uninstalls the setup; Linux checks desktop metadata and AppImage
execution without FUSE. CLI tests in the main CI include the render feature.

Publication requires successful main CI for that exact revision and every
requested package. The job creates a draft, uploads packages, corresponding
source and SHA256SUMS, then publishes it. Versions containing a prerelease
suffix are marked as prereleases. A published release is never silently
replaced. The notes come from `docs/releases/<tag>.md`.

## Decisions

- 2026-10-31 — The user explicitly requests publishing the current preview with
  Windows, AppImage, Debian and Arch downloads. This authorizes its tag and
  publication, superseding the earlier hold pending a trial. It does not mark
  the unfinished 0.11 interoperability acceptance complete or start 1.0 work.
- 2026-10-31 — Packaging originally planned for 1.0 is delivered early for this
  preview. Optional external deployment tools (NSIS, appimagetool) are build
  tools, outside the Rust application dependency graph. The optional AppImage
  loader is the documented deployment exception in docs/02; other distributions
  omit it. Both sets of corresponding sources accompany the release.
