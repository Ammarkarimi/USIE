//! Debug helper: parse and resolve every Go, Java, C#, Kotlin, Rust, C and C++
//! file under a directory
//! and print resolution statistics plus a sample of unresolved references.
//!
//! `cargo run --release -p usie-semantic --example analyze_dir -- <dir> [go-module-path]`
use std::collections::BTreeMap;
use std::path::Path;
use usie_ir::Language;
use usie_parser::{parse, ParseOptions};
use usie_semantic::{resolve, GoModule, Outcome, Project, ProjectConfig, SourceFile};

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            walk(root, &p, out);
        } else {
            out.push(
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = Path::new(&args[1]);
    let mut paths = Vec::new();
    walk(root, root, &mut paths);
    paths.sort();
    let mut config = ProjectConfig {
        paths: paths.clone(),
        ..ProjectConfig::default()
    };
    if let Some(module) = args.get(2) {
        config.go_modules.push(GoModule {
            dir: String::new(),
            path: module.clone(),
        });
    }
    // Rust packages: `name = "..."` in the `[package]` table of Cargo.toml.
    for p in &paths {
        if p.ends_with("Cargo.toml") {
            let text = std::fs::read_to_string(root.join(p)).unwrap_or_default();
            let mut in_package = false;
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_package = line == "[package]";
                } else if in_package && line.starts_with("name") {
                    if let Some(name) = line.split('"').nth(1) {
                        let dir = p
                            .strip_suffix("Cargo.toml")
                            .unwrap_or("")
                            .trim_end_matches('/');
                        config.rust_crates.push(usie_semantic::RustCrate {
                            dir: dir.to_string(),
                            name: name.to_string(),
                        });
                    }
                }
            }
        }
    }
    // .NET project files: `<Using Include="N" />` and package references.
    for p in &paths {
        if p.ends_with(".csproj") {
            let text = std::fs::read_to_string(root.join(p)).unwrap_or_default();
            let dir = p.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            for part in text.split('<').skip(1) {
                let attr = |name: &str| {
                    part.split(&format!("{name}=\""))
                        .nth(1)
                        .and_then(|r| r.split('"').next())
                };
                if part.starts_with("Using ")
                    && !part.contains("Static=")
                    && !part.contains("Alias=")
                {
                    if let Some(ns) = attr("Include") {
                        config.dotnet_usings.push(usie_semantic::DotnetUsing {
                            dir: dir.to_string(),
                            namespace: ns.to_string(),
                        });
                    }
                } else if part.starts_with("PackageReference ") {
                    if let Some(id) = attr("Include") {
                        config.dependencies.push(usie_semantic::DeclaredDependency {
                            ecosystem: "nuget".into(),
                            name: id.to_string(),
                        });
                    }
                }
            }
        }
    }
    let mut files = Vec::new();
    for p in &paths {
        let lang = match p.rsplit('.').next() {
            Some("go") => Language::Go,
            Some("java") => Language::Java,
            Some("cs") => Language::Csharp,
            Some("kt") => Language::Kotlin,
            Some("rs") => Language::Rust,
            Some("c") => Language::C,
            Some("h") => Language::C,
            Some("cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx") => Language::Cpp,
            _ => continue,
        };
        let src = std::fs::read(root.join(p)).unwrap();
        let ir = parse(lang, &src, &ParseOptions::default()).unwrap();
        files.push(SourceFile {
            path: p.clone(),
            ir,
        });
    }
    let n = files.len();
    let r = resolve(&Project { config, files });
    println!(
        "files {n}\n{}",
        serde_json::to_string_pretty(&r.stats).unwrap()
    );
    let unresolved: Vec<_> = r
        .references
        .iter()
        .filter(|x| x.outcome == Outcome::Unresolved)
        .collect();
    let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for x in &unresolved {
        *by_rule.entry(x.rule.as_str()).or_default() += 1;
    }
    println!("unresolved by rule: {by_rule:#?}");
    if std::env::var("USIE_REASONS").is_ok() {
        let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
        for x in &unresolved {
            let r = x.reason.clone().unwrap_or_default();
            // Group by message shape: drop quoted names.
            let shape: String = r
                .split('`')
                .enumerate()
                .map(|(i, p)| if i % 2 == 1 { "_" } else { p })
                .collect();
            *reasons.entry(format!("{} | {shape}", x.rule)).or_default() += 1;
        }
        let mut v: Vec<_> = reasons.into_iter().collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.1));
        for (k, n) in v.iter().take(25) {
            println!("{n:6} {k}");
        }
    }
    let step = (unresolved.len() / 40).max(1);
    for x in unresolved.iter().step_by(step).take(40) {
        println!(
            "  {}:{} {} {} — {}",
            x.path,
            x.line,
            x.kind,
            x.text,
            x.reason.as_deref().unwrap_or("")
        );
    }
    if let Ok(pattern) = std::env::var("USIE_GREP") {
        for x in unresolved
            .iter()
            .filter(|x| x.reason.as_deref().unwrap_or("").contains(&pattern))
            .take(15)
        {
            println!(
                "  GREP {}:{} {} — {}",
                x.path,
                x.line,
                x.text,
                x.reason.as_deref().unwrap_or("")
            );
        }
    }
    if let Ok(rule) = std::env::var("USIE_SAMPLE") {
        let hits: Vec<_> = r
            .references
            .iter()
            .filter(|x| x.rule == rule && x.target.is_some())
            .collect();
        let step = (hits.len() / 12).max(1);
        for x in hits.iter().step_by(step).take(12) {
            println!(
                "  SAMPLE {}:{} {} -> {}",
                x.path,
                x.line,
                x.text,
                x.target.as_deref().unwrap_or("")
            );
        }
    }
    let mut rules: BTreeMap<String, usize> = BTreeMap::new();
    for e in &r.edges {
        *rules
            .entry(format!("{} {} {}", e.kind, e.rule, e.confidence.as_str()))
            .or_default() += 1;
    }
    println!("edges by kind, rule, confidence: {rules:#?}");
}
