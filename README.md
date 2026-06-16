# ccpc

`ccpc` is a Rust CLI for converting, linting, and multiplexing Claude Code, Codex, and opencode plugin layouts.

Working name: **apx — Agent Plugin eXchange**.

## Goals

- Use a neutral IR pivot instead of N×N direct converters.
- Preserve unknown fields as opaque data for same-target round trips.
- Never silently succeed on lossy conversion.
- Default to fat plugin output through `multiplex`.
- Treat blocking hooks conservatively and never fail open.

## CLI

```bash
ccpc convert   --from cc --to codex --root ./p --out ./dist-codex [--bridge-hooks]
ccpc multiplex --from cc --to cc --to codex --to opencode --root ./p --out ./dist [--bridge-hooks]
ccpc lint      --from cc --root ./p
```

## Current implementation scope

Implemented core:

- `skills/` passthrough
- commands parser/emitter (`commands/` for cc, `prompts/` for codex, `.opencode/command/` for opencode)
- agents/subagents parser/emitter with `NEEDS_REVIEW` diagnostics for frontmatter mismatches
- `.mcp.json` parsing and emission for Claude Code / Codex
- `opencode.json#mcp` parsing and non-destructive merge
- manifest known fields plus opaque unknown field retention
- `hooks/hooks.json` parsing with a schema-backed event vocabulary table
- hook effect classification (`Block` / `Inject` / `Observe`) policy
- opencode TypeScript shim generation for `Observe` / `Inject` hooks behind `--bridge-hooks`
- `Block` hooks left unbridged for opencode to avoid fail-open behavior
- diagnostics for parked opaque fields, frontmatter review, and unsupported hook bridges
- golden tests for `convert` / `multiplex` and a CLI smoke test in CI
- `convert`, `multiplex`, and `lint` commands

Intentionally conservative / pending:

- exact schema pins (see `schemas/PINNED.toml`); the Codex schema URL is still unpinned
- MCP shape translation between `.mcp.json#mcpServers` and `opencode.json#mcp`

## Hook bridging policy

Hook events are classified using the vocabulary table in `src/main.rs`
(`HOOK_EVENTS`), reconciled with the upstream sources pinned in
`schemas/PINNED.toml`:

- `Block` — can deny/halt an action (e.g. `PreToolUse`, `UserPromptSubmit`). Never
  bridged to opencode, since opencode has no equivalent veto and bridging would
  fail open. A `Block`-keyword in the command (`deny`, `guard`, `exit 2`, …)
  forces this classification regardless of event.
- `Inject` — adds context (e.g. `SessionStart`). Bridged as observe-style; the
  injected output is not propagated back into opencode's model context.
- `Observe` — after-the-fact notification (e.g. `PostToolUse`). Bridged directly.

Bridged hooks are emitted to `.opencode/plugin/ccpc-bridge.ts` only when
`--bridge-hooks` is set. Unknown events default to `Block`.

## Layout emitted by `multiplex`

```text
dist/
  skills/<name>/SKILL.md
  commands/<name>.md
  prompts/<name>.md
  agents/<name>.md
  .claude-plugin/plugin.json
  .codex-plugin/plugin.json
  .mcp.json
  hooks/hooks.json
  .opencode/
    skills -> ../skills
    command/<name>.md
    agent/<name>.md
    opencode.json
    plugin/ccpc-bridge.ts   (only with --bridge-hooks)
```

On platforms where symlink creation fails, `skills/` is copied into `.opencode/skills`.

## Example and verification

`examples/cc-plugin/` is a complete Claude Code plugin fixture. `scripts/smoke.sh`
runs `lint` and `multiplex` against it and asserts the emitted layout, JSON
validity, and the never-fail-open hook policy. CI runs `fmt`, `clippy`, `test`,
and the smoke test.
