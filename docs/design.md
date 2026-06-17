# apx — Agent Plugin eXchange design

`ccpc` implements the `apx` design as a single Rust CLI.

## IR pivot

Converters are partial functions. `ccpc` avoids direct N×N mappings by parsing each source into a neutral IR and emitting each destination from that IR.

## Outcome policy

Lossy conversion must be represented as diagnostics or unsupported outcomes. Silent field loss is not allowed.

## Opaque retention

Unknown manifest fields are kept by source target. Same-target emission rehydrates those fields. Cross-target emission parks them and reports lint diagnostics.

## Hook policy

Blocking hooks must never be downgraded to non-blocking bridges. This prevents fail-open behavior for guard, deny, reject, and recursion protection hooks.

Hook events carry one of three effects in the IR:

- `Block` — controls an action (deny/halt). Never bridged.
- `Inject` — adds context. Bridged as observe-style; injected output is not propagated.
- `Observe` — passive notification. Bridged directly.

Classification is driven by the `HOOK_EVENTS` vocabulary table, which is
reconciled with the pinned upstream sources. Unknown events default to `Block`,
and block-intent keywords in a command override an otherwise observe-only event.
opencode bridging is opt-in via `--bridge-hooks` and emits a single generated
TypeScript plugin at `.opencode/plugin/ccpc-bridge.ts`.

## Components

Commands and agents are markdown documents. Their file bodies are preserved
verbatim across conversion (never silently rewritten); only the destination
directory changes per target. A best-effort frontmatter parse drives
`NEEDS_REVIEW` diagnostics when a document carries fields the destination target
does not recognize, or when the destination has no native concept for that
component (e.g. Codex subagents).

## Schema vendoring

`schemas/PINNED.toml` is the anchor for drift detection. Each entry records the
upstream source URL and the date its vocabulary was last reconciled. The hook
event table and frontmatter vocabularies in `src/main.rs` are the concrete
artifacts kept in sync with those pins.
