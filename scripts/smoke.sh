#!/usr/bin/env bash
# Real CLI load verification: drive the built ccpc binary against the example
# plugin and assert the emitted layout exists and the generated JSON parses.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${CCPC_BIN:-$ROOT/target/debug/ccpc}"
SRC="$ROOT/examples/cc-plugin"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

if [ ! -x "$BIN" ]; then
  echo "smoke: building ccpc"
  cargo build --manifest-path "$ROOT/Cargo.toml"
  BIN="$ROOT/target/debug/ccpc"
fi

echo "smoke: lint"
"$BIN" lint --from cc --root "$SRC"

echo "smoke: multiplex cc -> cc,codex,opencode (with --bridge-hooks)"
"$BIN" multiplex --from cc --to cc --to codex --to opencode \
  --root "$SRC" --out "$OUT" --bridge-hooks

assert_file() {
  if [ ! -e "$1" ]; then
    echo "smoke: FAIL missing expected output: $1" >&2
    exit 1
  fi
  echo "smoke: ok $1"
}

assert_json() {
  if ! python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$1"; then
    echo "smoke: FAIL invalid JSON: $1" >&2
    exit 1
  fi
  echo "smoke: ok json $1"
}

assert_file "$OUT/.claude-plugin/plugin.json"
assert_file "$OUT/.codex-plugin/plugin.json"
assert_file "$OUT/.mcp.json"
assert_file "$OUT/hooks/hooks.json"
assert_file "$OUT/commands/deploy.md"
assert_file "$OUT/prompts/deploy.md"
assert_file "$OUT/agents/reviewer.md"
assert_file "$OUT/skills/greet/SKILL.md"
assert_file "$OUT/.opencode/opencode.json"
assert_file "$OUT/.opencode/plugin/ccpc-bridge.ts"

assert_json "$OUT/.claude-plugin/plugin.json"
assert_json "$OUT/.codex-plugin/plugin.json"
assert_json "$OUT/.opencode/opencode.json"

# The blocking PreToolUse guard must never leak into the opencode bridge.
if grep -q "guard.sh" "$OUT/.opencode/plugin/ccpc-bridge.ts"; then
  echo "smoke: FAIL blocking hook bridged to opencode (fail-open)" >&2
  exit 1
fi
echo "smoke: ok blocking hook not bridged"

echo "smoke: PASS"
