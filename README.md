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
- `.mcp.json` parsing and emission for Claude Code / Codex
- `opencode.json#mcp` parsing and non-destructive merge
- manifest known fields plus opaque unknown field retention
- hook effect classification policy
- diagnostics for parked opaque fields and unsupported hook bridges
- `convert`, `multiplex`, and `lint` commands

Stubbed / intentionally conservative:

- commands parser/emitter
- agents/subagents parser/emitter
- native opencode TypeScript hook generation
- schema vendoring exact pins

## Layout emitted by `multiplex`

```text
dist/
  skills/<name>/SKILL.md
  .claude-plugin/plugin.json
  .codex-plugin/plugin.json
  .mcp.json
  .opencode/
    skills -> ../skills
    opencode.json
```

On platforms where symlink creation fails, `skills/` is copied into `.opencode/skills`.
