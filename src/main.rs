use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Default)]
enum Target {
    #[default]
    Cc,
    Codex,
    Opencode,
}

#[derive(Debug, Parser)]
#[command(name = "ccpc")]
#[command(about = "Agent Plugin eXchange for Claude Code, Codex, and opencode plugins")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Convert {
        #[arg(long)]
        from: Target,
        #[arg(long)]
        to: Target,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        bridge_hooks: bool,
    },
    Multiplex {
        #[arg(long)]
        from: Target,
        #[arg(long, required = true)]
        to: Vec<Target>,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        bridge_hooks: bool,
    },
    Lint {
        #[arg(long)]
        from: Target,
        #[arg(long)]
        root: PathBuf,
    },
}

#[derive(Debug, Default)]
struct Ir {
    source: Target,
    skills: Vec<Skill>,
    docs: Vec<Doc>,
    hooks_json: Option<Value>,
    hooks: Vec<HookSpec>,
    manifest: Manifest,
    mcp: Option<Value>,
    opencode_config: Option<Value>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
struct Skill {
    rel_path: PathBuf,
    content: Vec<u8>,
}

/// A markdown component (slash command or agent/subagent) preserved verbatim.
///
/// The full file `content` is kept byte-for-byte so same-target round trips are
/// lossless; `fields` is a best-effort frontmatter parse used only to raise
/// `NEEDS_REVIEW` diagnostics on cross-target frontmatter mismatches.
#[derive(Debug)]
struct Doc {
    kind: DocKind,
    rel_subpath: PathBuf,
    fields: BTreeMap<String, String>,
    complex_frontmatter: bool,
    content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocKind {
    Command,
    Agent,
}

#[derive(Debug, Clone)]
struct HookSpec {
    event: String,
    command: Option<String>,
    effect: HookEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookEffect {
    /// Can deny, halt, or otherwise control an action. Never bridged.
    Block,
    /// Additive context injection. Safe to bridge; injected output is not
    /// propagated back into the model context on opencode.
    Inject,
    /// After-the-fact notification with no control. Safe to bridge.
    Observe,
}

#[derive(Debug, Default)]
struct Manifest {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    opaque_by_target: BTreeMap<Target, Map<String, Value>>,
}

#[derive(Debug, Clone)]
struct Diagnostic {
    level: &'static str,
    code: &'static str,
    message: String,
}

impl Diagnostic {
    fn new(level: &'static str, code: &'static str, message: String) -> Self {
        Self {
            level,
            code,
            message,
        }
    }
}

/// Schema-backed hook event vocabulary, reconciled with the upstream sources
/// pinned in `schemas/PINNED.toml`. `effect` drives the never-fail-open
/// bridging policy; `opencode_hook` is the opencode plugin hook a bridgeable
/// (non-`Block`) event maps onto, or `None` when no safe mapping exists.
struct HookEventSpec {
    cc: &'static str,
    effect: HookEffect,
    opencode_hook: Option<&'static str>,
}

const HOOK_EVENTS: &[HookEventSpec] = &[
    // Blocking-capable events. Never bridged to opencode.
    spec("Setup", HookEffect::Block, None),
    spec("UserPromptSubmit", HookEffect::Block, None),
    spec("UserPromptExpansion", HookEffect::Block, None),
    spec("PreToolUse", HookEffect::Block, None),
    spec("PermissionRequest", HookEffect::Block, None),
    spec("PermissionDenied", HookEffect::Block, None),
    spec("SubagentStop", HookEffect::Block, None),
    spec("Stop", HookEffect::Block, None),
    spec("WorktreeCreate", HookEffect::Block, None),
    spec("PreCompact", HookEffect::Block, None),
    spec("Elicitation", HookEffect::Block, None),
    // Context-injecting events.
    spec("SessionStart", HookEffect::Inject, Some("event")),
    spec("InstructionsLoaded", HookEffect::Inject, Some("event")),
    // Observe-only events.
    spec(
        "PostToolUse",
        HookEffect::Observe,
        Some("tool.execute.after"),
    ),
    spec(
        "PostToolUseFailure",
        HookEffect::Observe,
        Some("tool.execute.after"),
    ),
    spec("PostToolBatch", HookEffect::Observe, Some("event")),
    spec("Notification", HookEffect::Observe, Some("event")),
    spec("MessageDisplay", HookEffect::Observe, Some("event")),
    spec("SubagentStart", HookEffect::Observe, Some("event")),
    spec("TaskCreated", HookEffect::Observe, Some("event")),
    spec("TaskCompleted", HookEffect::Observe, Some("event")),
    spec("StopFailure", HookEffect::Observe, Some("event")),
    spec("TeammateIdle", HookEffect::Observe, Some("event")),
    spec("ConfigChange", HookEffect::Observe, Some("event")),
    spec("CwdChanged", HookEffect::Observe, Some("event")),
    spec("FileChanged", HookEffect::Observe, Some("file.edited")),
    spec("WorktreeRemove", HookEffect::Observe, Some("event")),
    spec("PostCompact", HookEffect::Observe, Some("event")),
    spec("ElicitationResult", HookEffect::Observe, Some("event")),
    spec("SessionEnd", HookEffect::Observe, Some("event")),
];

const fn spec(
    cc: &'static str,
    effect: HookEffect,
    opencode_hook: Option<&'static str>,
) -> HookEventSpec {
    HookEventSpec {
        cc,
        effect,
        opencode_hook,
    }
}

/// Keywords that force a `Block` classification regardless of the event, so a
/// guard wired onto an otherwise observe-only event is never downgraded.
const BLOCK_KEYWORDS: &[&str] = &["block", "deny", "reject", "guard", "exit 2", "exit 1"];

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Convert {
            from,
            to,
            root,
            out,
            bridge_hooks,
        } => {
            let ir = parse(from, &root)?;
            let mut diagnostics = ir.diagnostics.clone();
            diagnostics.extend(emit(to, &ir, &out, bridge_hooks)?);
            print_diagnostics(&diagnostics);
        }
        Command::Multiplex {
            from,
            to,
            root,
            out,
            bridge_hooks,
        } => {
            let ir = parse(from, &root)?;
            fs::create_dir_all(&out)?;
            let mut diagnostics = ir.diagnostics.clone();
            for target in to {
                diagnostics.extend(emit(target, &ir, &out, bridge_hooks)?);
            }
            print_diagnostics(&diagnostics);
        }
        Command::Lint { from, root } => {
            let ir = parse(from, &root)?;
            print_diagnostics(&lint(&ir));
        }
    }

    Ok(())
}

fn parse(target: Target, root: &Path) -> Result<Ir> {
    let mut ir = Ir {
        source: target,
        ..Ir::default()
    };
    parse_skills(root, &mut ir)?;
    parse_docs(target, root, &mut ir)?;
    parse_mcp(target, root, &mut ir)?;
    parse_hooks(target, root, &mut ir)?;
    parse_manifest(target, root, &mut ir)?;
    parse_opencode(target, root, &mut ir)?;
    Ok(ir)
}

fn parse_skills(root: &Path, ir: &mut Ir) -> Result<()> {
    let skills = root.join("skills");
    if !skills.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(&skills)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        let rel_path = path.strip_prefix(root)?.to_path_buf();
        ir.skills.push(Skill {
            rel_path,
            content: fs::read(path)?,
        });
    }

    Ok(())
}

fn parse_docs(target: Target, root: &Path, ir: &mut Ir) -> Result<()> {
    parse_doc_kind(DocKind::Command, root.join(commands_dir(target)), ir)?;
    parse_doc_kind(DocKind::Agent, root.join(agents_dir(target)), ir)?;
    Ok(())
}

fn parse_doc_kind(kind: DocKind, dir: PathBuf, ir: &mut Ir) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(&dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let rel_subpath = path.strip_prefix(&dir)?.to_path_buf();
        let content = fs::read_to_string(path)?;
        let (frontmatter, _) = split_frontmatter(&content);
        let (fields, complex_frontmatter) = match frontmatter {
            Some(fm) => parse_frontmatter(&fm),
            None => (BTreeMap::new(), false),
        };
        ir.docs.push(Doc {
            kind,
            rel_subpath,
            fields,
            complex_frontmatter,
            content,
        });
    }

    Ok(())
}

fn parse_mcp(target: Target, root: &Path, ir: &mut Ir) -> Result<()> {
    match target {
        Target::Cc | Target::Codex => {
            let path = root.join(".mcp.json");
            if path.exists() {
                ir.mcp = Some(read_json(&path)?);
            }
        }
        Target::Opencode => {}
    }

    Ok(())
}

fn parse_hooks(target: Target, root: &Path, ir: &mut Ir) -> Result<()> {
    if target == Target::Opencode {
        // opencode expresses hooks as TypeScript plugins, not as a declarative
        // table, so there is nothing to lift into the IR here.
        return Ok(());
    }

    let path = root.join("hooks/hooks.json");
    if !path.exists() {
        return Ok(());
    }

    let value = read_json(&path)?;
    ir.hooks_json = Some(value.clone());

    // Accept both `{ "hooks": { Event: [...] } }` and a bare `{ Event: [...] }`.
    let events = value
        .get("hooks")
        .and_then(Value::as_object)
        .or_else(|| value.as_object());

    if let Some(events) = events {
        for (event, groups) in events {
            let Some(groups) = groups.as_array() else {
                continue;
            };
            for group in groups {
                let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
                    continue;
                };
                for hook in hooks {
                    let command = hook
                        .get("command")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let effect = classify_hook(event, command.as_deref());
                    ir.hooks.push(HookSpec {
                        event: event.clone(),
                        command,
                        effect,
                    });
                }
            }
        }
    }

    Ok(())
}

fn parse_manifest(target: Target, root: &Path, ir: &mut Ir) -> Result<()> {
    let path = match target {
        Target::Cc => root.join(".claude-plugin/plugin.json"),
        Target::Codex => root.join(".codex-plugin/plugin.json"),
        Target::Opencode => return Ok(()),
    };

    if !path.exists() {
        return Ok(());
    }

    let value = read_json(&path)?;
    let mut opaque = Map::new();

    if let Value::Object(map) = value {
        for (key, value) in map {
            match key.as_str() {
                "name" => ir.manifest.name = value.as_str().map(str::to_string),
                "version" => ir.manifest.version = value.as_str().map(str::to_string),
                "description" => ir.manifest.description = value.as_str().map(str::to_string),
                _ => {
                    opaque.insert(key, value);
                }
            }
        }
    }

    if !opaque.is_empty() {
        ir.diagnostics.push(Diagnostic::new(
            "warn",
            "OPAQUE_PARKED",
            format!("{} manifest unknown fields parked", target_name(target)),
        ));
        ir.manifest.opaque_by_target.insert(target, opaque);
    }

    Ok(())
}

fn parse_opencode(target: Target, root: &Path, ir: &mut Ir) -> Result<()> {
    if target != Target::Opencode {
        return Ok(());
    }

    let path = if root.join("opencode.json").exists() {
        Some(root.join("opencode.json"))
    } else if root.join(".opencode/opencode.json").exists() {
        Some(root.join(".opencode/opencode.json"))
    } else {
        None
    };

    if let Some(path) = path {
        let value = read_json(&path)?;
        if let Some(mcp) = value.get("mcp").cloned() {
            ir.mcp = Some(mcp);
        }
        ir.opencode_config = Some(value);
    }

    Ok(())
}

fn emit(target: Target, ir: &Ir, out: &Path, bridge_hooks: bool) -> Result<Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    emit_skills(out, ir)?;
    emit_docs(target, ir, out, &mut diagnostics)?;

    match target {
        Target::Cc => emit_manifest(out.join(".claude-plugin/plugin.json"), Target::Cc, ir)?,
        Target::Codex => emit_manifest(out.join(".codex-plugin/plugin.json"), Target::Codex, ir)?,
        Target::Opencode => emit_opencode(out, ir, bridge_hooks, &mut diagnostics)?,
    }

    if matches!(target, Target::Cc | Target::Codex) {
        if let Some(mcp) = &ir.mcp {
            write_json(out.join(".mcp.json"), mcp)?;
        }
        if let Some(hooks) = &ir.hooks_json {
            write_json(out.join("hooks/hooks.json"), hooks)?;
        }
    }

    Ok(diagnostics)
}

fn emit_skills(out: &Path, ir: &Ir) -> Result<()> {
    for skill in &ir.skills {
        let dest = out.join(&skill.rel_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, &skill.content)?;
    }

    Ok(())
}

fn emit_docs(target: Target, ir: &Ir, out: &Path, diagnostics: &mut Vec<Diagnostic>) -> Result<()> {
    for doc in &ir.docs {
        let dir = match doc.kind {
            DocKind::Command => commands_dir(target),
            DocKind::Agent => agents_dir(target),
        };
        let dest = out.join(dir).join(&doc.rel_subpath);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        // Content is written verbatim: conversion never silently rewrites a
        // component body or frontmatter. Mismatches are surfaced as diagnostics.
        fs::write(&dest, &doc.content)?;

        if ir.source != target {
            review_doc(doc, target, diagnostics);
        }
    }

    Ok(())
}

fn review_doc(doc: &Doc, target: Target, diagnostics: &mut Vec<Diagnostic>) {
    let kind = match doc.kind {
        DocKind::Command => "command",
        DocKind::Agent => "agent",
    };
    let location = doc.rel_subpath.display();
    let vocab = doc_vocab(doc.kind, target);

    if vocab.is_empty() && (doc.kind == DocKind::Agent || !doc.fields.is_empty()) {
        diagnostics.push(Diagnostic::new(
            "warn",
            "NEEDS_REVIEW",
            format!(
                "{kind} {location}: {} has no native {kind} concept; review manually",
                target_name(target)
            ),
        ));
        return;
    }

    let unknown: Vec<&str> = doc
        .fields
        .keys()
        .map(String::as_str)
        .filter(|key| !vocab.contains(key))
        .collect();

    if !unknown.is_empty() {
        diagnostics.push(Diagnostic::new(
            "warn",
            "NEEDS_REVIEW",
            format!(
                "{kind} {location}: frontmatter fields not recognized by {}: {}",
                target_name(target),
                unknown.join(", ")
            ),
        ));
    }

    if doc.complex_frontmatter {
        diagnostics.push(Diagnostic::new(
            "info",
            "NEEDS_REVIEW",
            format!(
                "{kind} {location}: nested/list frontmatter retained verbatim; verify {} mapping",
                target_name(target)
            ),
        ));
    }
}

fn emit_manifest(path: PathBuf, target: Target, ir: &Ir) -> Result<()> {
    let mut map = Map::new();
    map.insert(
        "name".into(),
        json!(ir
            .manifest
            .name
            .clone()
            .unwrap_or_else(|| "ccpc-plugin".into())),
    );

    if let Some(version) = &ir.manifest.version {
        map.insert("version".into(), json!(version));
    }
    if let Some(description) = &ir.manifest.description {
        map.insert("description".into(), json!(description));
    }
    if let Some(opaque) = ir.manifest.opaque_by_target.get(&target) {
        for (key, value) in opaque {
            map.insert(key.clone(), value.clone());
        }
    }

    write_json(path, &Value::Object(map))
}

fn emit_opencode(
    out: &Path,
    ir: &Ir,
    bridge_hooks: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<()> {
    let dir = out.join(".opencode");
    fs::create_dir_all(&dir)?;

    let mut config = ir.opencode_config.clone().unwrap_or_else(|| json!({}));
    let config_object = config
        .as_object_mut()
        .context("opencode config is not an object")?;

    if let Some(mcp) = &ir.mcp {
        config_object.insert("mcp".into(), mcp.clone());
    }

    write_json(dir.join("opencode.json"), &config)?;

    let opencode_skills = dir.join("skills");
    if !opencode_skills.exists() {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink("../skills", &opencode_skills);
    }
    if !opencode_skills.exists() {
        let _ = copy_dir_all(out.join("skills"), opencode_skills);
    }

    emit_opencode_hooks(&dir, ir, bridge_hooks, diagnostics)
}

fn emit_opencode_hooks(
    dir: &Path,
    ir: &Ir,
    bridge_hooks: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<()> {
    if ir.hooks.is_empty() {
        return Ok(());
    }

    if !bridge_hooks {
        diagnostics.push(Diagnostic::new(
            "warn",
            "HOOK_NOT_BRIDGED",
            format!(
                "{} hook(s) not emitted for opencode; re-run with --bridge-hooks to generate observe/inject shims",
                ir.hooks.len()
            ),
        ));
        return Ok(());
    }

    // Group bridgeable commands by their target opencode hook.
    let mut by_hook: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();

    for hook in &ir.hooks {
        match hook.effect {
            HookEffect::Block => {
                diagnostics.push(Diagnostic::new(
                    "warn",
                    "HOOK_BLOCK_UNSUPPORTED",
                    format!(
                        "blocking hook for event {} not bridged to opencode (would fail open)",
                        hook.event
                    ),
                ));
            }
            HookEffect::Observe | HookEffect::Inject => {
                let Some(command) = &hook.command else {
                    diagnostics.push(Diagnostic::new(
                        "info",
                        "HOOK_BRIDGE_SKIPPED",
                        format!(
                            "non-command hook for event {} cannot be bridged to opencode",
                            hook.event
                        ),
                    ));
                    continue;
                };
                match opencode_hook_for(&hook.event) {
                    Some(target_hook) => {
                        if hook.effect == HookEffect::Inject {
                            diagnostics.push(Diagnostic::new(
                                "info",
                                "HOOK_INJECT_OBSERVE",
                                format!(
                                    "inject hook for event {} bridged as observe; injected output is not propagated to opencode context",
                                    hook.event
                                ),
                            ));
                        }
                        by_hook
                            .entry(target_hook)
                            .or_default()
                            .push(command.clone());
                    }
                    None => diagnostics.push(Diagnostic::new(
                        "info",
                        "HOOK_BRIDGE_SKIPPED",
                        format!(
                            "event {} has no safe opencode mapping; not bridged",
                            hook.event
                        ),
                    )),
                }
            }
        }
    }

    if by_hook.is_empty() {
        return Ok(());
    }

    let plugin_dir = dir.join("plugin");
    fs::create_dir_all(&plugin_dir)?;
    fs::write(
        plugin_dir.join("ccpc-bridge.ts"),
        render_opencode_plugin(&by_hook),
    )?;

    diagnostics.push(Diagnostic::new(
        "info",
        "HOOK_BRIDGED",
        format!(
            "generated .opencode/plugin/ccpc-bridge.ts bridging {} opencode hook(s)",
            by_hook.len()
        ),
    ));

    Ok(())
}

fn render_opencode_plugin(by_hook: &BTreeMap<&'static str, Vec<String>>) -> String {
    let mut out = String::new();
    out.push_str("// Generated by ccpc (apx). Do not edit by hand.\n");
    out.push_str("// Conservative bridge for observe/inject Claude Code hooks.\n");
    out.push_str("// Blocking hooks are intentionally omitted to avoid fail-open behavior.\n");
    out.push_str("import type { Plugin } from \"@opencode-ai/plugin\"\n\n");
    out.push_str("export const ccpcBridge: Plugin = async ({ $ }) => {\n");
    out.push_str("  const run = async (cmd: string) => {\n");
    out.push_str("    try {\n");
    out.push_str("      await $`sh -c ${cmd}`.quiet()\n");
    out.push_str("    } catch (err) {\n");
    out.push_str("      console.error(`ccpc-bridge: hook command failed: ${cmd}`, err)\n");
    out.push_str("    }\n");
    out.push_str("  }\n");
    out.push_str("  return {\n");

    for (hook, commands) in by_hook {
        out.push_str(&format!("    \"{hook}\": async () => {{\n"));
        for command in commands {
            out.push_str(&format!("      await run({})\n", json!(command)));
        }
        out.push_str("    },\n");
    }

    out.push_str("  }\n");
    out.push_str("}\n");
    out
}

fn lint(ir: &Ir) -> Vec<Diagnostic> {
    let mut diagnostics = ir.diagnostics.clone();
    for target in ir.manifest.opaque_by_target.keys() {
        diagnostics.push(Diagnostic::new(
            "info",
            "ROUNDTRIP_OPAQUE",
            format!(
                "opaque manifest fields can be rehydrated for {}",
                target_name(*target)
            ),
        ));
    }
    for hook in &ir.hooks {
        if hook.effect == HookEffect::Block {
            diagnostics.push(Diagnostic::new(
                "info",
                "HOOK_BLOCK",
                format!(
                    "event {} classified as blocking; will not be bridged to opencode",
                    hook.event
                ),
            ));
        }
    }
    diagnostics
}

fn print_diagnostics(diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        eprintln!(
            "{} {}: {}",
            diagnostic.level, diagnostic.code, diagnostic.message
        );
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("parse json {}", path.display()))
}

fn write_json(path: PathBuf, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<()> {
    let src = src.as_ref();
    if !src.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(src)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let rel = entry.path().strip_prefix(src)?;
        let dest = dst.as_ref().join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }

    Ok(())
}

/// Split a markdown document into its YAML frontmatter block (if any) and the
/// body that follows it.
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let Some(rest) = content.strip_prefix("---\n") else {
        return (None, content.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (None, content.to_string());
    };
    let frontmatter = rest[..end].to_string();
    let after = &rest[end + "\n---".len()..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    (Some(frontmatter), body.to_string())
}

/// Best-effort frontmatter parse limited to top-level `key: value` scalars.
/// Returns the parsed map and whether any nested/list structure was seen (which
/// callers surface as `NEEDS_REVIEW`).
fn parse_frontmatter(frontmatter: &str) -> (BTreeMap<String, String>, bool) {
    let mut fields = BTreeMap::new();
    let mut complex = false;

    for line in frontmatter.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) || trimmed.starts_with('-') {
            complex = true;
            continue;
        }
        match trimmed.split_once(':') {
            Some((key, value)) => {
                fields.insert(key.trim().to_string(), value.trim().to_string());
            }
            None => complex = true,
        }
    }

    (fields, complex)
}

fn classify_hook(event: &str, command: Option<&str>) -> HookEffect {
    if let Some(command) = command {
        let lower = command.to_ascii_lowercase();
        if BLOCK_KEYWORDS.iter().any(|keyword| lower.contains(keyword)) {
            return HookEffect::Block;
        }
    }

    HOOK_EVENTS
        .iter()
        .find(|spec| spec.cc == event)
        .map_or(HookEffect::Block, |spec| spec.effect)
}

fn opencode_hook_for(event: &str) -> Option<&'static str> {
    HOOK_EVENTS
        .iter()
        .find(|spec| spec.cc == event)
        .and_then(|spec| spec.opencode_hook)
}

fn commands_dir(target: Target) -> &'static str {
    match target {
        Target::Cc => "commands",
        Target::Codex => "prompts",
        Target::Opencode => ".opencode/command",
    }
}

fn agents_dir(target: Target) -> &'static str {
    match target {
        Target::Cc => "agents",
        Target::Codex => "agents",
        Target::Opencode => ".opencode/agent",
    }
}

/// Known frontmatter field vocabulary per target and component kind. An empty
/// slice means the target has no native concept for that kind.
fn doc_vocab(kind: DocKind, target: Target) -> &'static [&'static str] {
    match (kind, target) {
        (DocKind::Command, Target::Cc) => &[
            "description",
            "argument-hint",
            "allowed-tools",
            "model",
            "disable-model-invocation",
        ],
        (DocKind::Command, Target::Opencode) => &[
            "description",
            "agent",
            "model",
            "subtask",
            "disable",
            "template",
        ],
        // Codex custom prompts are plain markdown with no frontmatter contract.
        (DocKind::Command, Target::Codex) => &[],
        (DocKind::Agent, Target::Cc) => &[
            "name",
            "description",
            "model",
            "effort",
            "maxTurns",
            "tools",
            "disallowedTools",
            "skills",
            "memory",
            "background",
            "isolation",
        ],
        (DocKind::Agent, Target::Opencode) => &[
            "description",
            "mode",
            "model",
            "temperature",
            "tools",
            "prompt",
            "disable",
            "permission",
            "reasoningEffort",
        ],
        // Codex has no subagent concept.
        (DocKind::Agent, Target::Codex) => &[],
    }
}

fn target_name(target: Target) -> &'static str {
    match target {
        Target::Cc => "cc",
        Target::Codex => "codex",
        Target::Opencode => "opencode",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn read(path: PathBuf) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn classify_is_conservative() {
        // Documented blocking event.
        assert_eq!(
            classify_hook("PreToolUse", Some("./guard.sh")),
            HookEffect::Block
        );
        // Documented observe event.
        assert_eq!(
            classify_hook("PostToolUse", Some("./format.sh")),
            HookEffect::Observe
        );
        // Block keyword overrides an observe event.
        assert_eq!(
            classify_hook("PostToolUse", Some("./check.sh && exit 2")),
            HookEffect::Block
        );
        // Unknown events default to Block.
        assert_eq!(classify_hook("MysteryEvent", None), HookEffect::Block);
    }

    #[test]
    fn frontmatter_round_trips_verbatim() {
        let content = "---\nname: reviewer\ndescription: reviews code\n---\nSystem prompt.\n";
        let (fm, body) = split_frontmatter(content);
        assert_eq!(
            fm.as_deref(),
            Some("name: reviewer\ndescription: reviews code")
        );
        assert_eq!(body, "System prompt.\n");
        let (fields, complex) = parse_frontmatter(&fm.unwrap());
        assert!(!complex);
        assert_eq!(fields.get("name").unwrap(), "reviewer");
    }

    #[test]
    fn frontmatter_flags_nested_structure() {
        let (_, complex) = parse_frontmatter("tools:\n  - Read\n  - Write");
        assert!(complex);
    }

    #[test]
    fn convert_cc_to_codex_golden() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("src");
        let out = tmp.path().join("out");
        write(&root, "skills/greet/SKILL.md", "# greet\n");
        write(
            &root,
            "commands/deploy.md",
            "---\ndescription: deploy\n---\nRun deploy.\n",
        );
        write(
            &root,
            ".claude-plugin/plugin.json",
            "{\n  \"name\": \"demo\",\n  \"version\": \"1.0.0\",\n  \"keywords\": [\"x\"]\n}\n",
        );

        let ir = parse(Target::Cc, &root).unwrap();
        let diagnostics = emit(Target::Codex, &ir, &out, false).unwrap();

        // Manifest known fields preserved; opaque keywords parked (cross-target).
        assert_eq!(
            read(out.join(".codex-plugin/plugin.json")),
            "{\n  \"name\": \"demo\",\n  \"version\": \"1.0.0\"\n}\n"
        );
        // Command body preserved verbatim under the codex prompts dir.
        assert_eq!(
            read(out.join("prompts/deploy.md")),
            "---\ndescription: deploy\n---\nRun deploy.\n"
        );
        // Skills pass through.
        assert_eq!(read(out.join("skills/greet/SKILL.md")), "# greet\n");
        // Cross-target command frontmatter is flagged for review (codex has no
        // command frontmatter contract).
        assert!(diagnostics
            .iter()
            .any(|d| d.code == "NEEDS_REVIEW" && d.message.contains("deploy.md")));
    }

    #[test]
    fn multiplex_opencode_blocks_fail_open_hooks() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("src");
        let out = tmp.path().join("out");
        write(
            &root,
            "hooks/hooks.json",
            "{\n  \"hooks\": {\n    \"PreToolUse\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"./guard.sh\"}]}],\n    \"PostToolUse\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"./format.sh\"}]}]\n  }\n}\n",
        );

        let ir = parse(Target::Cc, &root).unwrap();
        let diagnostics = emit(Target::Opencode, &ir, &out, true).unwrap();

        // Observe hook bridged.
        let plugin = read(out.join(".opencode/plugin/ccpc-bridge.ts"));
        assert!(plugin.contains("tool.execute.after"));
        assert!(plugin.contains("./format.sh"));
        // Blocking hook command is never written into the bridge.
        assert!(!plugin.contains("./guard.sh"));
        assert!(diagnostics
            .iter()
            .any(|d| d.code == "HOOK_BLOCK_UNSUPPORTED"));
    }

    #[test]
    fn opencode_hooks_not_bridged_without_flag() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("src");
        let out = tmp.path().join("out");
        write(
            &root,
            "hooks/hooks.json",
            "{\n  \"hooks\": {\n    \"PostToolUse\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"./format.sh\"}]}]\n  }\n}\n",
        );

        let ir = parse(Target::Cc, &root).unwrap();
        let diagnostics = emit(Target::Opencode, &ir, &out, false).unwrap();

        assert!(!out.join(".opencode/plugin/ccpc-bridge.ts").exists());
        assert!(diagnostics.iter().any(|d| d.code == "HOOK_NOT_BRIDGED"));
    }
}
