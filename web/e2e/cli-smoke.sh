#!/usr/bin/env bash
# CLI-surface smoke (ADR-0119/0121): pins the command line's observable
# contract — the retired spellings are gone (unrecognized commands), the
# noun-verb shapes parse, and the remote transport actually connects —
# against a live, isolated daemon.
#
# Complements apps/web/e2e/daemon-smoke.mjs (protocol-level) by exercising
# the binary itself: parsing, errors, exit codes, and the --remote path.
#
# Env: MUTA_BIN (default target/debug/muta) and MUTX_BIN (default
# target/debug/mutx). Exits non-zero on the first failed expectation; the
# instance root is a throwaway tempdir, so a host daemon is never touched
# (ADR-0121).

set -u

# Resolve the repo root through this script's location so it works from any
# cwd and through symlinks: apps/web/e2e → three levels up is the root.
SCRIPT_SOURCE=${BASH_SOURCE[0]}
REPO_ROOT=$(cd -P "$(dirname "$SCRIPT_SOURCE")/../../.." >/dev/null 2>&1 && pwd)
MUTA=${MUTA_BIN:-$REPO_ROOT/target/debug/muta}
MUTX=${MUTX_BIN:-$REPO_ROOT/target/debug/mutx}
if [ ! -x "$MUTA" ]; then
  echo "cli-smoke: no core binary at $MUTA — build it first: cargo build -p muta" >&2
  exit 127
fi
if [ ! -x "$MUTX" ]; then
  echo "cli-smoke: no terminal app at $MUTX — build it first: cargo build -p mutx" >&2
  exit 127
fi
ROOT=$(mktemp -d /tmp/mutx-smoke.XXXXXX)
PORT=${PORT:-9809}
export MUTA_HOME="$ROOT"
export MUTA_PORT="$PORT"
cleanup() { "$MUTA" stop >/dev/null 2>&1; rm -rf "$ROOT"; }
trap cleanup EXIT

pass=0
fail=0

# expect_status <want> <desc> <cmd...>  — assert an exit status
expect_status() {
  local want=$1 desc=$2
  shift 2
  if "$@" >/dev/null 2>&1; then local got=0; else local got=$?; fi
  if [ "$got" = "$want" ]; then
    pass=$((pass + 1)); echo "ok   $desc"
  else
    fail=$((fail + 1)); echo "FAIL $desc (exit $got, want $want)"
  fi
}

# expect_out <pattern> <desc> <cmd...>  — assert combined output matches
expect_out() {
  local pat=$1 desc=$2
  shift 2
  local out
  out=$("$@" 2>&1)
  if printf '%s' "$out" | grep -q "$pat"; then
    pass=$((pass + 1)); echo "ok   $desc"
  else
    fail=$((fail + 1)); echo "FAIL $desc (output: $out)"
  fi
}

echo "== retired spellings are removed =="
expect_status 2 "bare session errors" "$MUTA" session
expect_out   "session rm" "session teaches rm" "$MUTA" session
expect_status 2 "session ls is an unknown subcommand" "$MUTA" session ls
expect_status 2 "bare mcp errors" "$MUTA" mcp
expect_out   "mcp ls" "mcp teaches ls" "$MUTA" mcp
expect_status 2 "bare skill errors" "$MUTA" skill
expect_out   "skill ls" "skill teaches ls" "$MUTA" skill
expect_status 2 "serve is unrecognized" "$MUTA" serve
expect_status 2 "--home flag errors with MUTA_HOME guidance" "$MUTA" --home /tmp/dev
expect_status 2 "mutx --home flag errors with MUTA_HOME guidance" "$MUTX" --home /tmp/dev

echo "== noun-verb shapes parse =="
expect_status 0 "mcp ls runs" "$MUTA" mcp ls
expect_status 0 "skill ls runs" "$MUTA" skill ls
expect_status 2 "session rm needs an id" "$MUTA" session rm
expect_status 0 "top-level stop runs" "$MUTA" stop
expect_status 0 "top-level status runs" "$MUTA" status

echo "== core and terminal app have disjoint surfaces =="
expect_status 2 "core rejects terminal run command" "$MUTA" run ping
expect_out "muta service command" "terminal app points daemon users to muta" \
  "$MUTX" daemon status

echo "== remote transport validates its inputs =="
expect_out "not host:port" "remote rejects a bare host" \
  "$MUTX" --remote box.lan --token t run x
expect_out "needs --token" "remote demands the token" \
  "$MUTX" --remote box.lan:9800 run x
expect_out "not a port number" "remote rejects a bad port" \
  "$MUTX" --remote box.lan:notaport --token t run x

echo "== mutx auto-starts muta; remote connects to that daemon =="
# No daemon exists yet. Reaching the daemon's provider error proves mutx found
# and launched the core binary before attempting the run.
expect_out "provider" "mutx auto-starts the sibling muta core" \
  env MUTA_BIN="$MUTA" "$MUTX" run "ping"
for _ in $(seq 1 50); do
  [ -s "$ROOT/muta/instance/daemon.json" ] && break
  sleep 0.2
done
TOKEN=$("$MUTA" token)
# The handshake must complete over TCP+bearer: with no provider configured
# the daemon's own reply is the "no provider" error — proof the remote
# path reached a live daemon rather than failing to connect.
expect_out "provider" "--remote reaches the live daemon" \
  "$MUTX" --remote "127.0.0.1:$PORT" --token "$TOKEN" run "ping"

echo
echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
