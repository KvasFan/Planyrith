#!/usr/bin/env bash
# docs/13 0.5 acceptance, "external validators pass": builds the textured
# kitchen of the 0.4 acceptance batches, exports it in every format and runs
# the validators of other tools on the files:
#   glTF / GLB  Khronos glTF-Validator (npm `gltf-validator`, via node)
#   DXF         ezdxf's recover + audit (Python)
#   SVG         xmllint (well-formed XML)
#   PDF         pdfinfo (poppler)
# The glTF validator and ezdxf are downloaded into the scratch folder on
# first use (npm and pip, network); a missing tool is reported and skipped.
# Exits non-zero when a validator finds an error.
# Usage: tools/validate-exports.sh [scratch-dir]
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="${1:-${TMPDIR:-/tmp}/planyrith-validate-exports}"
mkdir -p "$work/model/textures" "$work/tools"
cargo build --quiet --manifest-path "$root/Cargo.toml" -p planyrith-cli
cli="$root/target/debug/planyrith"
failed=0

cd "$work/model"
python3 - <<'PY'
import struct, zlib
def png(path, w, h, rgb):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    data += chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
    open(path, "wb").write(data)
png("textures/oak.png", 64, 32, (160, 82, 45))
png("textures/tile.png", 16, 16, (200, 200, 210))
PY
rm -f cabinet.plyr kitchen.plyr
"$cli" batch "$root/crates/planyrith-cli/tests/batches/cabinet.plyrb" -f cabinet.plyr --deterministic >/dev/null
"$cli" batch "$root/crates/planyrith-cli/tests/batches/kitchen.plyrb" -f kitchen.plyr --deterministic >/dev/null
for out in kitchen.glb kitchen.gltf kitchen.obj kitchen.stl kitchen.dxf kitchen.svg kitchen.pdf; do
  "$cli" export -f kitchen.plyr "$out" >/dev/null
done
"$cli" export -f kitchen.plyr kitchen-2d.dxf --2d >/dev/null

# glTF / GLB
if command -v node >/dev/null && command -v npm >/dev/null; then
  if [ ! -d "$work/tools/node_modules/gltf-validator" ]; then
    (cd "$work/tools" && npm init -y >/dev/null && npm install --silent gltf-validator >/dev/null)
  fi
  cat > "$work/tools/validate-gltf.mjs" <<'JS'
import validator from 'gltf-validator';
import fs from 'node:fs';
import path from 'node:path';
const file = process.argv[2];
const report = await validator.validateBytes(new Uint8Array(fs.readFileSync(file)), {
  uri: path.basename(file),
  externalResourceFunction: (uri) => Promise.resolve(
    new Uint8Array(fs.readFileSync(path.join(path.dirname(file), decodeURIComponent(uri))))),
});
const i = report.issues;
console.log(`${path.basename(file)}: glTF-Validator errors=${i.numErrors} warnings=${i.numWarnings}`);
for (const m of i.messages.filter((m) => m.severity <= 1)) console.log(`  ${m.code}: ${m.message}`);
process.exit(i.numErrors > 0 ? 1 : 0);
JS
  for f in kitchen.glb kitchen.gltf; do
    (cd "$work/tools" && node validate-gltf.mjs "$work/model/$f") || failed=1
  done
else
  echo "skipped glTF: node/npm not found"
fi

# DXF
if [ ! -x "$work/tools/venv/bin/python" ]; then
  python3 -m venv "$work/tools/venv" && "$work/tools/venv/bin/pip" install --quiet ezdxf || true
fi
if "$work/tools/venv/bin/python" -c "import ezdxf" 2>/dev/null; then
  for f in kitchen.dxf kitchen-2d.dxf; do
    "$work/tools/venv/bin/python" - "$f" <<'PY' || failed=1
import sys
from ezdxf import recover
doc, auditor = recover.readfile(sys.argv[1])
count = sum(1 for _ in doc.modelspace())
print(f"{sys.argv[1]}: ezdxf audit errors={len(auditor.errors)} fixes={len(auditor.fixes)} entities={count}")
sys.exit(1 if auditor.errors or count == 0 else 0)
PY
  done
else
  echo "skipped DXF: ezdxf not available"
fi

# SVG and PDF
if command -v xmllint >/dev/null; then
  if xmllint --noout kitchen.svg; then echo "kitchen.svg: well-formed"; else failed=1; fi
else
  echo "skipped SVG: xmllint not found"
fi
if command -v pdfinfo >/dev/null; then
  if pdfinfo kitchen.pdf >/dev/null; then echo "kitchen.pdf: pdfinfo reads it"; else failed=1; fi
else
  echo "skipped PDF: pdfinfo not found"
fi

exit "$failed"
