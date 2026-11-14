//! Process-level tests of the `usie-engine` NDJSON protocol.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn engine(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_usie-engine"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn engine");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn lines(out: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect()
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir =
            std::env::temp_dir().join(format!("usie-engine-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    fn write(&self, rel: &str, content: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn info_reports_fingerprints() {
    let out = engine(&["info"], "");
    assert!(out.status.success());
    let doc = &lines(&out)[0];
    assert_eq!(doc["ir_schema_version"], 1);
    assert_eq!(doc["languages"].as_array().unwrap().len(), 4);
    assert!(doc["languages"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .contains("tree-sitter-python"));
}

#[test]
fn parse_then_resolve_round_trip() {
    let repo = TempDir::new("roundtrip");
    repo.write("pkg/__init__.py", "");
    repo.write(
        "pkg/a.py",
        "from .b import helper\n\ndef main():\n    helper()\n",
    );
    repo.write("pkg/b.py", "def helper():\n    pass\n");
    repo.write(
        "web/app.ts",
        "import { helper } from './lib';\nexport const run = () => helper();\n",
    );
    repo.write("web/lib.ts", "export function helper() {}\n");

    let files = [
        ("pkg/__init__.py", "python"),
        ("pkg/a.py", "python"),
        ("pkg/b.py", "python"),
        ("web/app.ts", "typescript"),
        ("web/lib.ts", "typescript"),
    ];
    let requests: String = files
        .iter()
        .enumerate()
        .map(|(i, (path, language))| {
            format!(
                "{}\n",
                json!({ "id": i.to_string(), "path": path, "language": language })
            )
        })
        .collect();
    let parsed = engine(
        &["parse", "--root", repo.path().to_str().unwrap()],
        &requests,
    );
    assert!(
        parsed.status.success(),
        "{}",
        String::from_utf8_lossy(&parsed.stderr)
    );
    let results = lines(&parsed);
    assert_eq!(results.len(), files.len());

    let mut resolve_input = format!(
        "{}\n",
        json!({ "type": "config", "config": { "paths": files.iter().map(|f| f.0).collect::<Vec<_>>() } })
    );
    // Results come back in request order; feed them to resolve in reverse to
    // show that input order does not matter.
    for (i, result) in results.iter().enumerate().rev() {
        assert_eq!(result["id"], i.to_string());
        assert_eq!(result["status"], "ok", "{result}");
        resolve_input.push_str(&format!(
            "{}\n",
            json!({ "type": "file", "path": result["path"], "ir": result["ir"] })
        ));
    }
    let resolved = engine(&["resolve"], &resolve_input);
    assert!(
        resolved.status.success(),
        "{}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    let records = lines(&resolved);
    assert_eq!(
        records.last().unwrap()["type"],
        "stats",
        "stats terminates the stream"
    );
    let has_edge = |src: &str, kind: &str, dst: &str| {
        records.iter().any(|r| {
            r["type"] == "edge"
                && r["src"] == src
                && r["kind"] == kind
                && r["dst"] == dst
                && r["confidence"] == "confirmed"
        })
    };
    assert!(has_edge(
        "sym:pkg/a.py#main",
        "CALLS",
        "sym:pkg/b.py#helper"
    ));
    assert!(has_edge(
        "sym:web/app.ts#run",
        "CALLS",
        "sym:web/lib.ts#helper"
    ));
    assert!(has_edge("file:pkg/a.py", "IMPORTS", "file:pkg/b.py"));
    assert_eq!(records.last().unwrap()["files"], 5);
}

#[test]
fn parse_refuses_to_leave_the_root() {
    let repo = TempDir::new("escape");
    let outside = TempDir::new("escape-outside");
    outside.write("secret.py", "token = 'x'\n");
    repo.write("ok.py", "x = 1\n");
    repo.write("big.py", &"x = 1\n".repeat(100));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path().join("secret.py"),
            repo.path().join("link.py"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join("linkdir")).unwrap();
    }
    let absolute = outside.path().join("secret.py");
    let requests = [
        json!({ "id": "a", "path": "../escape-outside/secret.py", "language": "python" }),
        json!({ "id": "b", "path": absolute.to_str().unwrap(), "language": "python" }),
        json!({ "id": "c", "path": "link.py", "language": "python" }),
        json!({ "id": "d", "path": "linkdir/secret.py", "language": "python" }),
        json!({ "id": "e", "path": "ok.py", "language": "cobol" }),
        json!({ "id": "f", "path": "missing.py", "language": "python" }),
        json!({ "id": "g", "path": "big.py", "language": "python" }),
        json!({ "id": "h", "path": "logical/name.py", "language": "python", "source": "ok.py" }),
    ];
    let stdin: String = requests
        .iter()
        .map(|r| format!("{r}\n"))
        .collect::<String>()
        + "not json\n";
    let out = engine(
        &[
            "parse",
            "--root",
            repo.path().to_str().unwrap(),
            "--max-file-bytes",
            "100",
        ],
        &stdin,
    );
    assert!(out.status.success());
    let results = lines(&out);
    let code = |i: usize| results[i]["code"].as_str().unwrap_or("").to_string();
    assert_eq!(code(0), "path_escape");
    assert_eq!(code(1), "path_escape");
    #[cfg(unix)]
    {
        assert_eq!(code(2), "not_regular_file");
        assert_eq!(code(3), "path_escape");
    }
    assert_eq!(code(4), "unsupported_language");
    assert_eq!(code(5), "read_error");
    assert_eq!(code(6), "too_large");
    assert_eq!(
        (results[7]["status"].as_str(), results[7]["path"].as_str()),
        (Some("ok"), Some("logical/name.py"))
    );
    assert_eq!(code(8), "bad_request");
    assert!(
        results.iter().all(|r| !r.to_string().contains("token")),
        "no content from outside the root"
    );
}

#[test]
fn resolve_rejects_malformed_input() {
    let bad_json = engine(&["resolve"], "{\"type\":\"file\"}\n");
    assert_eq!(bad_json.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&bad_json.stderr).contains("invalid_input"));
    assert!(bad_json.stdout.is_empty(), "no partial output on failure");

    let ir = json!({
        "schema_version": 1, "language": "python", "parser": { "name": "x", "version": "y" },
        "symbols": [], "imports": [], "exports": [], "diagnostics": [],
        "stats": { "nodes": 0, "diagnostics_total": 0, "parse_micros": 0, "extract_micros": 0 },
        "references": [{ "kind": "call", "scope": 7, "path": ["f"], "text": "f", "span": { "start_byte": 0, "end_byte": 1, "start_line": 1, "start_col": 0, "end_line": 1, "end_col": 1 } }],
    });
    let out_of_range = engine(
        &["resolve"],
        &format!("{}\n", json!({ "type": "file", "path": "a.py", "ir": ir })),
    );
    assert_eq!(out_of_range.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out_of_range.stderr).contains("scope out of range"));

    let usage = engine(&["parse"], "");
    assert_eq!(usage.status.code(), Some(2));
}
