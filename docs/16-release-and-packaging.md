# 16 — Release and packaging

**Audience:** users and maintainers. Implements docs/12 §4 release distribution.

## 1. Distributions

Every desktop distribution contains the GUI, CLI (including headless rendering),
and the optional, independently executed `planyrith-skp` format module beside
the application binaries. `.plyr` remains the default working format.

| Platform | Package | Installation |
|---|---|---|
| Windows 10/11 x64 | `*-setup.exe` | Per-user installer, Start menu and optional desktop shortcut/native file association; no administrator privileges |
| Windows 10/11 x64 | `*.msi` | Windows Installer package for managed or scripted installs: per-user, Start menu shortcut, native file association, removable format-module feature; no administrator privileges |
| Windows x64 | `*-portable.zip` | Extract and run `planyrith-gui.exe` |
| macOS 11 or later (Apple silicon and Intel) | `*-macos.dmg` | Drag Planyrith to Applications; unsigned, so the first start is Open from the context menu. The CLI is `Planyrith.app/Contents/MacOS/planyrith` |
| Linux x86_64 | `*.AppImage` | Make executable, then run; `--appimage-extract-and-run` works without FUSE |
| Debian 12 / Ubuntu 22.04 or later | `*_amd64.deb` | `sudo apt install ./planyrith_*_amd64.deb` |
| Linux x86_64 with Flatpak | `*.flatpak` | `flatpak install --user Planyrith-*.flatpak` (the Freedesktop runtime comes from Flathub); sandboxed with access to the home folder |
| Arch Linux x86_64 | `*.pkg.tar.zst` | `sudo pacman -U ./planyrith-*-x86_64.pkg.tar.zst` |
| Linux x86_64 | `*-linux-x86_64.tar.gz` | Extract and run `./Planyrith`; `./Planyrith --cli` starts the CLI |

Linux binaries are built on Ubuntu 22.04 (glibc 2.35 baseline). They use system
graphics/windowing drivers; the portable packages do not bundle a Linux desktop.
A working Vulkan implementation is required for the 3D view. The Windows setup
and MSI are currently unsigned. Packages install only the native `.plyr` association.
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

Linux requires GNU tar, zstd, dpkg-deb and desktop-file-utils.
On macOS, `cargo xtask macos --bin-dir DIR` makes the application bundle and
disk image from universal binaries (joined with `lipo`); on other systems it
stops after the bundle.
`cargo xtask flatpak` builds the Flatpak bundle from the same binaries with
`flatpak` and the Freedesktop 26.08 Platform and SDK installed; before finishing
it runs the CLI and opens a historical model through the format module inside
the sandbox. Windows uses NSIS
and PowerShell. Official Windows binaries use the MSVC target with the platform
CRT linked statically, avoiding a separate runtime installer:

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --workspace --release --locked --target x86_64-pc-windows-msvc --features planyrith-cli/render
Remove-Item Env:RUSTFLAGS
cargo xtask windows --bin-dir target/x86_64-pc-windows-msvc/release
```

Optionally set `MAKENSIS` to the NSIS compiler path. Output goes to
`dist/release/`; `--out` and `--bin-dir` override
the directories. Installation instructions, licenses and build revision travel
with each payload. Packaging from the repository requires a Git checkout.

The MSI is built from
the same Windows binaries by `wixl` (msitools), on Linux or wherever msitools
runs; set `WIXL` to override the program:

```sh
cargo xtask msi --bin-dir path/to/windows/binaries
```

It installs into `%LOCALAPPDATA%\Programs\Planyrith`, like the setup, and
`msiexec /i Planyrith-*.msi /qn` installs it silently. A later version, including
a later prerelease of the same numeric version, replaces an earlier one; an
older one refuses to install over a newer one. The `.plyr` default is set only
when no other program owns it, and stays if it belongs to another program.

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
installs and uninstalls the setup and the MSI (built on Linux from the same
binaries) and checks executable imports for unwanted
runtime DLL dependencies; Linux checks desktop metadata and AppImage
execution without FUSE. CLI tests in the main CI include the render feature.

Publication requires successful main CI for that exact revision and every
requested package. The job creates a draft, uploads packages, corresponding
source and SHA256SUMS, then publishes it. Versions containing a prerelease
suffix are marked as prereleases. A published release is never silently
replaced. The notes come from `docs/releases/<tag>.md`.

## 4. Documentation site and web edition

`cargo xtask site` renders the README, installation notes, the room-modeling
guide, the changelog and this specification as static pages in
`dist/release/site/` (`--out` overrides the parent directory). Links between
documents become links between pages; links to other repository files lead to
the repository. A test builds the site from the repository and fails on any
broken internal link.

The manually dispatched `Pages` workflow builds the site, adds the web
application under `app/` with `tools/build-web.sh` (size budget included), and
publishes both on GitHub Pages. The repository's Pages source must be set once
to GitHub Actions (Settings › Pages). Like a release, publishing is a decision
taken by running the workflow, never a side effect of a push.

## Decisions

- 2026-11-09 — **Releasing without the hosted runners.** GitHub Actions jobs do not start for
  this account (billing lock since 2026-11-08), so `v0.11.0-alpha.4` is built on the maintainer's
  Linux machine. Linux packages are built in an Ubuntu 22.04 container (the glibc 2.35 baseline
  of §1). Windows programs are cross-built for `x86_64-pc-windows-gnu` with mingw-w64 and the C
  runtime linked statically (they import only system DLLs, the Universal CRT among them);
  `cargo xtask windows` makes the portable zip with `zip` where PowerShell is missing and runs
  NSIS from a container through `MAKENSIS`, and `cargo xtask msi` uses `wixl` as before. The
  setup and the MSI were installed, run and removed under Wine, the CLI opened an SKP model
  through the format module and rendered, and the GUI started with the OpenGL backend (Wine's
  Direct3D shader compiler lacks what the default backend needs). The macOS disk image needs
  Apple's SDK, which may only be used on Apple hardware, so it is missing from this release;
  `cargo xtask macos` on a Mac makes it. The MSVC build of §2 stays the official recipe.

- 2026-11-03 — The 1.0 macOS package is a disk image with one universal
  application bundle (both architectures, `lipo`), macOS 11 or later, holding the
  three programs side by side in `Contents/MacOS` as on other platforms. The
  bundle, property list (`.plyr` document type `org.planyrith.model`) and icon
  are written by `xtask` (an `.icns` is a container of PNGs, so no image tool);
  only `hdiutil` needs macOS. It is unsigned and not notarized: that needs a paid
  developer account, a decision for the user. The Release workflow mounts it and
  exercises both architectures' binaries, the format module and a native save.
- 2026-11-03 — The 1.0 Flatpak is a single-file bundle of the release binaries
  (glibc 2.35 baseline, so they run on the newer Freedesktop runtime), built with
  `flatpak build-init`/`build-finish`/`build-export`/`build-bundle` instead of
  `flatpak-builder`: no second compilation inside the SDK and no extra tool. It
  ships the AppStream metainfo software centers read. The home folder is shared
  because the interface's own file dialog, not a portal, picks files. A Flathub
  submission, which requires building from source there, is left for later.
- 2026-11-03 — The 1.0 documentation site is generated by `xtask` with
  `pulldown-cmark` (MIT, build tool only) instead of an external site generator:
  one Rust command, no extra toolchain, output fully determined by the sources.
  It publishes the specification as written (the repository is public), styled
  with the application's palette, dark or light following the system. The web
  edition is deployed beside it so one address serves both.
- 2026-10-31 — The user explicitly requests publishing the current preview with
  Windows, AppImage, Debian and Arch downloads. This authorizes its tag and
  publication, superseding the earlier hold pending a trial. It does not mark
  the unfinished 0.11 interoperability acceptance complete or start 1.0 work.
- 2026-10-31 — Packaging originally planned for 1.0 is delivered early for this
  preview. Optional external deployment tools (NSIS, appimagetool) are build
  tools, outside the Rust application dependency graph. The optional AppImage
  loader is the documented deployment exception in docs/02; other distributions
  omit it. Both sets of corresponding sources accompany the release.
- 2026-11-03 — The 1.0 MSI is generated by `cargo xtask msi` as WiX source and
  built by `wixl` (msitools, LGPL build tool, not linked or shipped), because it
  runs on Linux, where it is checked locally (also under Wine), and needs no
  Windows-only toolset. Windows Installer versions are numeric, so
  `X.Y.Z-pre` installs as `X.Y.Z`; same-version upgrades are allowed so
  prereleases replace each other. Component codes derive from each file's path,
  product codes from the full version, and the upgrade code is fixed. The MSI
  offers no desktop shortcut option: it is meant for scripted installs, where
  the setup program's choices have no user to ask. The Release workflow
  verifies it on Windows by installing, running, and uninstalling it. It builds
  it in a Fedora container: wixl before 0.106 (Ubuntu 24.04 has 0.103) rejects
  the component condition that leaves another program's `.plyr` default alone.
