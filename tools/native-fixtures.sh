#!/usr/bin/env bash
# Adds the native `.plyr` written by each command's batch to the frozen
# format corpus (docs/09 §9). Existing files are never rewritten: they are
# what earlier releases wrote, and current readers must keep loading them.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
version="${1:-v1}"
out="$root/crates/planyrith-core/tests/fixtures/native/$version"
PLANYRITH_NATIVE_FIXTURES="$out" cargo test --locked -p planyrith-cli --test cli \
  every_command_matches_its_golden_file
du -sh "$out"
