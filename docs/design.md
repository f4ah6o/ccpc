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

## Schema vendoring

`schemas/PINNED.toml` is the anchor for drift detection. Exact upstream pins must be filled before claiming spec-following compatibility.
