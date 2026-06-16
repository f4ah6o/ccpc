use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Target {
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
    skills: Vec<Skill>,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Convert { from, to, root, out, bridge_hooks } => {
            let ir = parse(from, &root)?;
            emit(to, &ir, &out, bridge_hooks)?;
            print_diagnostics(&ir.diagnostics);
        }
        Command::Multiplex { from, to, root, out, bridge_hooks } => {
            let ir = parse(from, &root)?;
            fs::create_dir_all(&out)?;
            for target in to {
                emit(target, &ir, &out, bridge_hooks)?;
            }
            print_diagnostics(&ir.diagnostics);
        }
        Command::Lint { from, root } => {
            let ir = parse(from, &root)?;
            print_diagnostics(&lint(&ir));
        }
    }
    Ok(())
}

fn parse(target: Target, root: &Path) -> Result<Ir> {
    let mut ir = Ir::default();
    parse_skills(root, &mut ir)?;
    parse_mcp(target, root, &mut ir)?;
    parse_manifest(target, root, &mut ir)?;
    parse_opencode(target, root, &mut ir)?;
    Ok(ir)
}

fn parse_skills(root: &Path, ir: &mut Ir) -> Result<()> {
    let skills = root.join("skills");
    if !skills.exists() { return Ok(()); }
    for entry in WalkDir::new(&skills).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()) {
        let path = entry.path();
        let rel_path = path.strip_prefix(root).unwrap().to_path_buf();
        ir.skills.push(Skill { rel_path, content: fs::read(path)? });
    }
    Ok(())
}

fn parse_mcp(target: Target, root: &Path, ir: &mut Ir) -> Result<()> {
    match target {
        Target::Cc | Target::Codex => {
            let p = root.join(".mcp.json");
            if p.exists() { ir.mcp = Some(read_json(&p)?); }
        }
        Target::Opencode => {}
    }
    Ok(())
}

fn parse_manifest(target: Target, root: &Path, ir: &mut Ir) -> Result<()> {
    let path = match target {
        Target::Cc => root.join(".claude-plugin/plugin.json"),
        Target::Codex => root.join(".codex-plugin/plugin.json"),
        Target::Opencode => return Ok(()),
    };
    if !path.exists() { return Ok(()); }
    let value = read_json(&path)?;
    let mut opaque = Map::new();
    if let Value::Object(map) = value {
        for (k, v) in map {
            match k.as_str() {
                "name" => ir.manifest.name = v.as_str().map(str::to_string),
                "version" => ir.manifest.version = v.as_str().map(str::to_string),
                "description" => ir.manifest.description = v.as_str().map(str::to_string),
                _ => { opaque.insert(k, v); }
            }
        }
    }
    if !opaque.is_empty() {
        ir.diagnostics.push(Diagnostic { level: "warn", code: "OPAQUE_PARKED", message: format!("{} manifest unknown fields parked", target_name(target)) });
        ir.manifest.opaque_by_target.insert(target, opaque);
    }
    Ok(())
}

fn parse_opencode(target: Target, root: &Path, ir: &mut Ir) -> Result<()> {
    if target != Target::Opencode { return Ok(()); }
    let path = root.join("opencode.json").exists().then(|| root.join("opencode.json"))
        .or_else(|| root.join(".opencode/opencode.json").exists().then(|| root.join(".opencode/opencode.json")));
    if let Some(path) = path {
        let value = read_json(&path)?;
        if let Some(mcp) = value.get("mcp").cloned() { ir.mcp = Some(mcp); }
        ir.opencode_config = Some(value);
    }
    Ok(())
}

fn emit(target: Target, ir: &Ir, out: &Path, bridge_hooks: bool) -> Result<()> {
    emit_skills(out, ir)?;
    match target {
        Target::Cc => emit_manifest(out.join(".claude-plugin/plugin.json"), Target::Cc, ir)?,
        Target::Codex => emit_manifest(out.join(".codex-plugin/plugin.json"), Target::Codex, ir)?,
        Target::Opencode => emit_opencode(out, ir, bridge_hooks)?,
    }
    if matches!(target, Target::Cc | Target::Codex) {
        if let Some(mcp) = &ir.mcp { write_json(out.join(".mcp.json"), mcp)?; }
    }
    Ok(())
}

fn emit_skills(out: &Path, ir: &Ir) -> Result<()> {
    for skill in &ir.skills {
        let dest = out.join(&skill.rel_path);
        if let Some(parent) = dest.parent() { fs::create_dir_all(parent)?; }
        fs::write(dest, &skill.content)?;
    }
    Ok(())
}

fn emit_manifest(path: PathBuf, target: Target, ir: &Ir) -> Result<()> {
    let mut map = Map::new();
    map.insert("name".into(), json!(ir.manifest.name.clone().unwrap_or_else(|| "ccpc-plugin".into())));
    if let Some(v) = &ir.manifest.version { map.insert("version".into(), json!(v)); }
    if let Some(v) = &ir.manifest.description { map.insert("description".into(), json!(v)); }
    if let Some(opaque) = ir.manifest.opaque_by_target.get(&target) {
        for (k, v) in opaque { map.insert(k.clone(), v.clone()); }
    }
    write_json(path, &Value::Object(map))
}

fn emit_opencode(out: &Path, ir: &Ir, bridge_hooks: bool) -> Result<()> {
    let dir = out.join(".opencode");
    fs::create_dir_all(&dir)?;
    let mut config = ir.opencode_config.clone().unwrap_or_else(|| json!({}));
    if let Some(mcp) = &ir.mcp {
        config.as_object_mut().context("opencode config is not an object")?.insert("mcp".into(), mcp.clone());
    }
    if !bridge_hooks {
        config.as_object_mut().unwrap().entry("plugins").or_insert(json!([]));
    }
    write_json(dir.join("opencode.json"), &config)?;
    let opencode_skills = dir.join("skills");
    if !opencode_skills.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink("../skills", &opencode_skills).ok();
    }
    if !opencode_skills.exists() {
        copy_dir_all(out.join("skills"), opencode_skills).ok();
    }
    Ok(())
}

fn lint(ir: &Ir) -> Vec<Diagnostic> {
    let mut d = ir.diagnostics.clone();
    for target in ir.manifest.opaque_by_target.keys() {
        d.push(Diagnostic { level: "info", code: "ROUNDTRIP_OPAQUE", message: format!("opaque manifest fields can be rehydrated for {}", target_name(*target)) });
    }
    d
}

fn print_diagnostics(diags: &[Diagnostic]) {
    for d in diags {
        eprintln!("{} {}: {}", d.level, d.code, d.message);
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let s = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parse json {}", path.display()))
}

fn write_json(path: PathBuf, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<()> {
    let src = src.as_ref();
    if !src.exists() { return Ok(()); }
    for entry in WalkDir::new(src).into_iter().filter_map(Result::ok) {
        let rel = entry.path().strip_prefix(src)?;
        let dest = dst.as_ref().join(rel);
        if entry.file_type().is_dir() { fs::create_dir_all(dest)?; } else { fs::copy(entry.path(), dest)?; }
    }
    Ok(())
}

fn target_name(t: Target) -> &'static str {
    match t { Target::Cc => "cc", Target::Codex => "codex", Target::Opencode => "opencode" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_words_are_conservative() {
        assert!(infer_hook_effect("pre_apply guard exit 2") == "Block");
        assert!(infer_hook_effect("observe log") == "Observe");
    }

    fn infer_hook_effect(s: &str) -> &'static str {
        let x = s.to_ascii_lowercase();
        if ["pre", "block", "deny", "reject", "exit 2", "guard"].iter().any(|k| x.contains(k)) { "Block" } else { "Observe" }
    }
}
