# Planyrith desktop packages

Use `planyrith-gui` for the desktop application and `planyrith` for the CLI.
Keep the independently executed `planyrith-skp` beside them for SKP/SKB support.
The native and default working format is `.plyr`.

- Windows x86_64: run the setup executable, or extract the portable ZIP and
  start `planyrith-gui.exe`. Installation is per-user; the optional GPL format
  module can be deselected. The installer is unsigned.
- AppImage x86_64: make the file executable and run it. `--cli` selects the CLI.
  `--appimage-extract-and-run` is available on systems that prohibit FUSE mounts.
- Debian 12 / Ubuntu 22.04 or newer, amd64: `sudo apt install ./planyrith_*.deb`.
- Arch Linux x86_64: `sudo pacman -U ./planyrith-*.pkg.tar.zst`.
- Portable Linux x86_64: extract the archive and run `./Planyrith`, or
  `./Planyrith --cli --help`. The build requires glibc 2.35 or newer and a working
  Vulkan driver. Wayland and X11 are supported.

Verify downloads against `SHA256SUMS`. Third-party licenses are in `licenses/`
(installed under `/usr/share/licenses/planyrith` in system Linux packages).
Complete offline Cargo sources accompany the release, including the separate
GPL format module and its dependencies. AppImage runtime sources are separate.

This is a prerelease. Historical SKP reading starts at 2013; edited SKP writers
target 2017/2026, with the documented data conversion limits. Unchanged source
files retain their original bytes through native working copies. The browser
edition currently has no SKP/SKB process backend.
