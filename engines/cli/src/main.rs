//! `usie-engine`: the analysis engine process.
//!
//! The Go services run this binary as a child process and talk to it over
//! newline-delimited JSON on stdin/stdout. Keeping analysis in a separate,
//! unprivileged process means a crash or hang while handling untrusted source
//! cannot take a service down, and the process can be resource-limited.
//!
//! Commands:
//!
//! * `info`     — print engine identity and parser fingerprints.
//! * `parse`    — read parse requests, write one result per request, in order.
//! * `resolve`  — read a project (config + file IRs), write graph records.
//!
//! The engine only reads files below `--root` and never executes anything.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use usie_ir::{FileIr, Language, IR_SCHEMA_VERSION};
use usie_parser::{ParseError, ParseOptions};
use usie_semantic::{Project, ProjectConfig, SourceFile};

const LANGUAGES: [Language; 4] = [Language::Python, Language::Javascript, Language::Typescript, Language::Tsx];
const BATCH: usize = 256;
/// Extraction recurses over the syntax tree; give workers room for deep input.
const WORKER_STACK: usize = 16 * 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("info") => info(),
        Some("parse") => parse_args(&args[1..]).and_then(parse_command),
        Some("resolve") => resolve_command(),
        Some("--version") | Some("-V") => {
            println!("usie-engine {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => Err(Failure::usage(
            "usage: usie-engine <info|parse|resolve>\n  parse --root <dir> [--timeout-ms N] [--max-file-bytes N] [--threads N]",
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            // Errors go to stderr as one JSON line so the caller can log them.
            let line = serde_json::json!({ "type": "error", "code": f.code, "message": f.message });
            eprintln!("{line}");
            ExitCode::from(f.exit)
        }
    }
}

struct Failure {
    code: &'static str,
    message: String,
    exit: u8,
}

impl Failure {
    fn usage(message: impl Into<String>) -> Failure {
        Failure { code: "usage", message: message.into(), exit: 2 }
    }
    fn input(message: impl Into<String>) -> Failure {
        Failure { code: "invalid_input", message: message.into(), exit: 3 }
    }
    fn io(err: io::Error) -> Failure {
        Failure { code: "io", message: err.to_string(), exit: 4 }
    }
}

// ---- info -----------------------------------------------------------------

fn info() -> Result<(), Failure> {
    let languages: Vec<_> = LANGUAGES
        .iter()
        .map(|l| serde_json::json!({ "language": l.as_str(), "fingerprint": usie_parser::parser_fingerprint(*l) }))
        .collect();
    let doc = serde_json::json!({
        "engine": "usie-engine",
        "version": env!("CARGO_PKG_VERSION"),
        "ir_schema_version": IR_SCHEMA_VERSION,
        "extractor_version": usie_parser::EXTRACTOR_VERSION,
        "languages": languages,
    });
    println!("{doc}");
    Ok(())
}

// ---- parse ----------------------------------------------------------------

struct ParseArgs {
    root: PathBuf,
    timeout: Duration,
    max_file_bytes: u64,
    threads: usize,
}

fn parse_args(args: &[String]) -> Result<ParseArgs, Failure> {
    let mut root = None;
    let mut timeout_ms = 10_000u64;
    let mut max_file_bytes = 2 * 1024 * 1024u64;
    let mut threads = 0usize;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| Failure::usage(format!("{flag} needs a value")));
        let number = |v: &String| v.parse::<u64>().map_err(|_| Failure::usage(format!("{flag}: `{v}` is not a number")));
        match flag.as_str() {
            "--root" => root = Some(PathBuf::from(value()?)),
            "--timeout-ms" => timeout_ms = number(value()?)?,
            "--max-file-bytes" => max_file_bytes = number(value()?)?,
            "--threads" => threads = number(value()?)? as usize,
            other => return Err(Failure::usage(format!("unknown flag `{other}`"))),
        }
    }
    let root = root.ok_or_else(|| Failure::usage("parse requires --root <dir>"))?;
    // Canonicalize once: every file read is checked against this prefix.
    let root = root.canonicalize().map_err(|e| Failure::usage(format!("--root: {e}")))?;
    Ok(ParseArgs { root, timeout: Duration::from_millis(timeout_ms), max_file_bytes, threads })
}

#[derive(Deserialize)]
struct ParseRequest {
    /// Opaque correlation id, echoed back.
    #[serde(default)]
    id: String,
    /// Repository-relative path of the file (reported in results).
    path: String,
    language: String,
    /// File to read, relative to `--root`. Defaults to `path`. Lets the
    /// caller parse from a content-addressed store.
    #[serde(default)]
    source: Option<String>,
}

#[derive(Serialize)]
struct ParseResponse {
    id: String,
    path: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ir: Option<FileIr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    bytes: u64,
}

fn parse_command(args: ParseArgs) -> Result<(), Failure> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .stack_size(WORKER_STACK)
        .build()
        .map_err(|e| Failure { code: "internal", message: e.to_string(), exit: 5 })?;
    let options = ParseOptions { timeout: args.timeout, ..ParseOptions::default() };
    let stdin = io::stdin().lock();
    let mut out = BufWriter::new(io::stdout().lock());
    let mut batch: Vec<String> = Vec::with_capacity(BATCH);

    let flush = |batch: &mut Vec<String>, out: &mut BufWriter<io::StdoutLock>| -> Result<(), Failure> {
        let results: Vec<String> = pool.install(|| batch.par_iter().map(|line| handle_parse(line, &args, &options)).collect());
        for line in results {
            out.write_all(line.as_bytes()).map_err(Failure::io)?;
            out.write_all(b"\n").map_err(Failure::io)?;
        }
        out.flush().map_err(Failure::io)?;
        batch.clear();
        Ok(())
    };

    for line in stdin.lines() {
        let line = line.map_err(Failure::io)?;
        if line.trim().is_empty() {
            continue;
        }
        batch.push(line);
        if batch.len() >= BATCH {
            flush(&mut batch, &mut out)?;
        }
    }
    flush(&mut batch, &mut out)
}

fn handle_parse(line: &str, args: &ParseArgs, options: &ParseOptions) -> String {
    let response = match serde_json::from_str::<ParseRequest>(line) {
        Ok(req) => {
            let (id, path) = (req.id.clone(), req.path.clone());
            match parse_one(&req, args, options) {
                Ok((ir, bytes)) => ParseResponse { id, path, status: "ok", ir: Some(ir), code: None, message: None, bytes },
                Err((code, message)) => ParseResponse { id, path, status: "error", ir: None, code: Some(code), message: Some(message), bytes: 0 },
            }
        }
        Err(e) => ParseResponse {
            id: String::new(),
            path: String::new(),
            status: "error",
            ir: None,
            code: Some("bad_request"),
            message: Some(e.to_string()),
            bytes: 0,
        },
    };
    serde_json::to_string(&response).expect("response serializes")
}

fn parse_one(req: &ParseRequest, args: &ParseArgs, options: &ParseOptions) -> Result<(FileIr, u64), (&'static str, String)> {
    let language = Language::parse(&req.language)
        .ok_or_else(|| ("unsupported_language", format!("no parser for `{}`", req.language)))?;
    let rel = req.source.as_deref().unwrap_or(&req.path);
    let file = safe_path(&args.root, rel)?;
    let meta = std::fs::symlink_metadata(&file).map_err(|e| ("read_error", e.to_string()))?;
    if !meta.is_file() {
        return Err(("not_regular_file", "not a regular file (symlinks are not followed)".into()));
    }
    if meta.len() > args.max_file_bytes {
        return Err(("too_large", format!("{} bytes exceeds the limit of {}", meta.len(), args.max_file_bytes)));
    }
    let mut source = Vec::with_capacity(meta.len() as usize);
    // `take` bounds the read even if the file grows after the size check.
    std::fs::File::open(&file)
        .and_then(|f| f.take(args.max_file_bytes + 1).read_to_end(&mut source))
        .map_err(|e| ("read_error", e.to_string()))?;
    if source.len() as u64 > args.max_file_bytes {
        return Err(("too_large", format!("file exceeds the limit of {} bytes", args.max_file_bytes)));
    }
    match usie_parser::parse(language, &source, options) {
        Ok(ir) => Ok((ir, source.len() as u64)),
        Err(ParseError::Timeout) => Err(("timeout", format!("parse exceeded {} ms", options.timeout.as_millis()))),
        Err(ParseError::Grammar(m)) => Err(("grammar", m)),
    }
}

/// Map a request path to a file below `root`, refusing anything that could
/// escape it: absolute paths, `..` components, and symlinked directories.
fn safe_path(root: &Path, rel: &str) -> Result<PathBuf, (&'static str, String)> {
    let rel_path = Path::new(rel);
    if rel.is_empty() || rel.contains('\0') || rel_path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(("path_escape", format!("`{rel}` is not a plain relative path")));
    }
    let joined = root.join(rel_path);
    let parent = joined.parent().unwrap_or(root);
    let real_parent = parent.canonicalize().map_err(|e| ("read_error", e.to_string()))?;
    if !real_parent.starts_with(root) {
        return Err(("path_escape", format!("`{rel}` resolves outside the root")));
    }
    Ok(real_parent.join(joined.file_name().unwrap_or_default()))
}

// ---- resolve --------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResolveInput {
    Config { config: ProjectConfig },
    File { path: String, ir: Box<FileIr> },
}

fn resolve_command() -> Result<(), Failure> {
    let mut project = Project::default();
    for (n, line) in io::stdin().lock().lines().enumerate() {
        let line = line.map_err(Failure::io)?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ResolveInput>(&line) {
            Ok(ResolveInput::Config { config }) => project.config = config,
            Ok(ResolveInput::File { path, ir }) => {
                if ir.schema_version != IR_SCHEMA_VERSION {
                    return Err(Failure::input(format!(
                        "{path}: IR schema version {} is not supported (expected {IR_SCHEMA_VERSION})",
                        ir.schema_version
                    )));
                }
                validate_ir(&path, &ir)?;
                project.files.push(SourceFile { path, ir: *ir });
            }
            Err(e) => return Err(Failure::input(format!("line {}: {e}", n + 1))),
        }
    }
    // Input order must not influence output.
    project.files.sort_by(|a, b| a.path.cmp(&b.path));
    if let Some(pair) = project.files.windows(2).find(|w| w[0].path == w[1].path) {
        return Err(Failure::input(format!("duplicate file `{}`", pair[0].path)));
    }

    let resolution = std::thread::Builder::new()
        .stack_size(WORKER_STACK)
        .spawn(move || usie_semantic::resolve(&project))
        .map_err(Failure::io)?
        .join()
        .map_err(|_| Failure { code: "internal", message: "resolver panicked".into(), exit: 5 })?;

    let mut out = BufWriter::new(io::stdout().lock());
    let mut emit = |kind: &str, value: serde_json::Value| -> Result<(), Failure> {
        let mut value = value;
        value["type"] = kind.into();
        writeln!(out, "{value}").map_err(Failure::io)
    };
    for node in &resolution.nodes {
        emit("node", serde_json::to_value(node).expect("node serializes"))?;
    }
    for edge in &resolution.edges {
        emit("edge", serde_json::to_value(edge).expect("edge serializes"))?;
    }
    for reference in &resolution.references {
        emit("reference", serde_json::to_value(reference).expect("reference serializes"))?;
    }
    // The stats record is always last: its presence marks complete output.
    emit("stats", serde_json::to_value(&resolution.stats).expect("stats serialize"))?;
    out.flush().map_err(Failure::io)
}

/// The resolver indexes by symbol id; reject IR that violates the invariants
/// the parser guarantees rather than trusting the caller.
fn validate_ir(path: &str, ir: &FileIr) -> Result<(), Failure> {
    let count = ir.symbols.len() as u32;
    let bad = |what: &str| Failure::input(format!("{path}: invalid IR: {what}"));
    for (i, sym) in ir.symbols.iter().enumerate() {
        if sym.id as usize != i {
            return Err(bad("symbol ids are not dense"));
        }
        if sym.parent.is_some_and(|p| p >= sym.id) {
            return Err(bad("symbol parent does not precede child"));
        }
        if !sym.locals.windows(2).all(|w| w[0] < w[1]) {
            return Err(bad("symbol locals are not sorted"));
        }
    }
    if ir.references.iter().any(|r| r.scope.is_some_and(|s| s >= count)) {
        return Err(bad("reference scope out of range"));
    }
    if ir.imports.iter().any(|i| i.scope.is_some_and(|s| s >= count)) {
        return Err(bad("import scope out of range"));
    }
    Ok(())
}
