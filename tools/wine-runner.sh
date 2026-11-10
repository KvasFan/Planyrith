#!/usr/bin/env bash
# Runs a Windows test program under wine for tools/ci-local.sh (docs/12): the
# paths cargo gives tests to the workspace's programs (CARGO_BIN_EXE_*) are
# Linux paths, which a Windows program cannot start; they are turned into
# wine's Windows paths first, and the runner setting, which test helpers
# would use to start those programs, is dropped. Wine cannot pass redirected standard handles on
# to the programs a test starts, so without a terminal the test runs in a
# pseudo-terminal.
set -euo pipefail
args=(-u CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER)
while IFS= read -r -d '' kv; do
  name=${kv%%=*}
  case $name in
    CARGO_BIN_EXE_*) args+=("$name=$(winepath -w "${kv#*=}" 2>/dev/null)") ;;
  esac
done < <(env -0)
if [ -t 1 ]; then
  exec env "${args[@]}" wine "$@"
fi
exec script -qefc "$(printf '%q ' env "${args[@]}" wine "$@")" /dev/null </dev/null
