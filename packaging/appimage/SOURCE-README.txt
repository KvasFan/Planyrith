Optional AppImage deployment runtime
===================================

The payload applications are pure Rust. This separately replaceable deployment
launcher is AppImage/type2-runtime, commit:
fedfad51acc2860b2916a1eef32ba4a500ff31ef
Runtime x86_64 SHA-256:
617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46

Extract type2-runtime-fedfad51.tar.gz and consult BUILD.md and scripts/docker/.
The archive includes the complete runtime source, link flags, container build
instructions and the libfuse patches used by upstream. Fuse 3.15.0 and
squashfuse 0.5.2 sources are supplied here, matching upstream's verified inputs.
Other toolchain/base-system dependencies are listed in scripts/docker/Dockerfile
(Alpine 3.21). The runtime's LICENSE lists its bundled component notices;
libfuse's LGPL notices are also in the supplied fuse source.

Build the runtime using the documented container method. The resulting runtime
can replace the shipped launcher without changing the Planyrith payload:

  payload_offset=$(./Planyrith-*.AppImage --appimage-offset)
  dd if=./Planyrith-VERSION-x86_64.AppImage of=payload.squashfs bs=1 skip="$payload_offset"
  cat path/to/rebuilt/runtime payload.squashfs > Planyrith-relinked.AppImage
  chmod +x Planyrith-relinked.AppImage

Alternatively unpack with --appimage-extract and regenerate the image with
appimagetool --runtime-file path/to/rebuilt/runtime squashfs-root OUTPUT.
This deployment code is not part of the Cargo graph, core, GUI or CLI. It can
be omitted entirely by using the portable archive, Debian or Arch package.
