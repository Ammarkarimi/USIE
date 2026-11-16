//! Rule tests: each rule must match its pattern and must not match the
//! closest safe variant.

use usie_ir::Language;
use usie_parser::{parse, ParseOptions};

fn findings(language: Language, src: &str) -> Vec<(String, u32)> {
    let ir = parse(language, src.as_bytes(), &ParseOptions::default()).expect("parse");
    ir.findings
        .iter()
        .map(|f| (f.rule.clone(), f.span.start_line))
        .collect()
}

fn rules(language: Language, src: &str) -> Vec<String> {
    findings(language, src)
        .into_iter()
        .map(|(r, _)| r)
        .collect()
}

fn has(language: Language, src: &str, rule: &str) -> bool {
    rules(language, src).iter().any(|r| r == rule)
}

#[test]
fn every_rule_is_described() {
    let catalog = usie_parser::checks::rules();
    let mut ids: Vec<&str> = catalog.iter().map(|r| r.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), catalog.len(), "duplicate rule ids");
    for r in catalog {
        assert!(
            matches!(r.category, "bug" | "vulnerability" | "hotspot" | "smell"),
            "{}",
            r.id
        );
        assert!(
            matches!(
                r.severity,
                "blocker" | "critical" | "major" | "minor" | "info"
            ),
            "{}",
            r.id
        );
        assert!(!r.description.is_empty() && !r.remediation.is_empty() && !r.languages.is_empty());
    }
}

#[test]
fn complexity_counts_branches_and_logical_operators() {
    let mut body = String::from("def f(a, b):\n");
    for i in 0..15 {
        body.push_str(&format!("    if a == {i} and b:\n        return {i}\n"));
    }
    body.push_str("    return 0\n");
    // 1 + 15 ifs + 15 `and` = 31
    let ir = parse(Language::Python, body.as_bytes(), &ParseOptions::default()).unwrap();
    assert_eq!(ir.metrics.functions, 1);
    assert_eq!(ir.metrics.max_complexity, 31);
    let f = ir
        .findings
        .iter()
        .find(|f| f.rule == "function-complexity")
        .expect("complexity");
    assert_eq!(f.function.as_deref(), Some("f"));
    assert_eq!(f.span.start_line, 1);

    assert!(!has(
        Language::Go,
        "package p\nfunc f(a int) int { if a > 1 { return 1 }; return 0 }",
        "function-complexity"
    ));
}

#[test]
fn anonymous_callbacks_count_towards_the_enclosing_function() {
    let src = "function outer(xs) { return xs.map(x => x ? 1 : 2); }\nconst bound = (y) => y || 3;";
    let ir = parse(
        Language::Javascript,
        src.as_bytes(),
        &ParseOptions::default(),
    )
    .unwrap();
    // outer (1 + ternary) and the bound arrow (1 + ||) are units; the callback is not.
    assert_eq!(ir.metrics.functions, 2);
    assert_eq!(ir.metrics.complexity, 4);
}

#[test]
fn length_parameters_and_nesting() {
    let long = format!("fn f() {{\n{}}}\n", "    let _x = 1;\n".repeat(101));
    assert!(has(Language::Rust, &long, "function-length"));
    assert!(!has(
        Language::Rust,
        "fn f() {\n let _x = 1;\n}\n",
        "function-length"
    ));

    assert!(has(
        Language::Java,
        "class A { void m(int a,int b,int c,int d,int e,int f,int g,int h) {} }",
        "too-many-parameters"
    ));
    assert!(!has(
        Language::Java,
        "class A { void m(int a,int b,int c,int d,int e,int f,int g) {} }",
        "too-many-parameters"
    ));
    // self is not counted; Go `a, b int` is two parameters.
    assert!(!has(
        Language::Python,
        "def m(self, a, b, c, d, e, f, g): pass",
        "too-many-parameters"
    ));
    assert!(has(
        Language::Go,
        "package p\nfunc f(a, b, c, d int, e, f, g, h string) {}",
        "too-many-parameters"
    ));

    let deep = "def f(a):\n if a:\n  for x in a:\n   while x:\n    if x:\n     try:\n      pass\n     except ValueError:\n      raise\n";
    let found = findings(Language::Python, deep);
    assert!(
        found.contains(&("deep-nesting".to_string(), 6)),
        "{found:?}"
    );
    // else-if chains do not nest.
    let chain = "function f(a){ if(a==1){} else if(a==2){} else if(a==3){} else if(a==4){} else if(a==5){} }";
    assert!(!has(Language::Javascript, chain, "deep-nesting"));
}

#[test]
fn comments_and_catch_blocks() {
    assert!(has(
        Language::Go,
        "package p\n// TODO: remove\nfunc f() {}",
        "todo-comment"
    ));
    assert!(!has(
        Language::Go,
        "package p\n// todos are tracked elsewhere\nfunc f() {}",
        "todo-comment"
    ));

    assert!(has(
        Language::Java,
        "class A { void m() { try { x(); } catch (Exception e) {} } }",
        "empty-catch"
    ));
    assert!(!has(
        Language::Java,
        "class A { void m() { try { x(); } catch (Exception e) { /* expected */ } } }",
        "empty-catch"
    ));
    assert!(has(
        Language::Python,
        "try:\n    x()\nexcept ValueError:\n    pass\n",
        "empty-catch"
    ));
    assert!(!has(
        Language::Python,
        "try:\n    x()\nexcept ValueError:\n    log()\n",
        "empty-catch"
    ));
    assert!(has(
        Language::Python,
        "try:\n    x()\nexcept:\n    log()\n",
        "bare-except"
    ));
    assert!(!has(
        Language::Python,
        "try:\n    x()\nexcept Exception:\n    log()\n",
        "bare-except"
    ));
    assert!(has(
        Language::Kotlin,
        "fun f() { try { x() } catch (e: Exception) {} }",
        "empty-catch"
    ));
}

#[test]
fn javascript_rules() {
    assert!(has(
        Language::Typescript,
        "function f() { debugger; }",
        "debugger-statement"
    ));
    assert!(has(
        Language::Javascript,
        "if (a == 1) {}",
        "loose-equality"
    ));
    assert!(!has(
        Language::Javascript,
        "if (a === 1 || b == null) {}",
        "loose-equality"
    ));
}

#[test]
fn bug_patterns() {
    assert!(has(
        Language::Go,
        "package p\nfunc f(a int) bool { return a == a }",
        "self-comparison"
    ));
    assert!(has(Language::Python, "x = a.b < a.b", "self-comparison"));
    assert!(!has(
        Language::Javascript,
        "x = next() === next();",
        "self-comparison"
    ));
    assert!(!has(
        Language::Javascript,
        "x = a === b;",
        "self-comparison"
    ));

    assert!(has(
        Language::Javascript,
        "if (a) { go(); } else { go(); }",
        "identical-branches"
    ));
    assert!(has(
        Language::C,
        "int f(int a) { return a ? 1 : 1; }",
        "identical-branches"
    ));
    assert!(!has(
        Language::Javascript,
        "if (a) { go(); } else { stop(); }",
        "identical-branches"
    ));
    assert!(!has(
        Language::Javascript,
        "if (a) {} else {}",
        "identical-branches"
    ));
    assert!(!has(
        Language::Python,
        "if a:\n    go()\nelif b:\n    go()\nelse:\n    go()\n",
        "identical-branches"
    ));

    assert!(has(
        Language::C,
        "void f(int a) { if (a = 2) {} }",
        "assignment-in-condition"
    ));
    assert!(!has(
        Language::C,
        "void f(int a) { if ((a = g())) {} }",
        "assignment-in-condition"
    ));
    assert!(!has(
        Language::C,
        "void f(int a) { if (a == 2) {} }",
        "assignment-in-condition"
    ));

    assert!(has(
        Language::Python,
        "def f(a, b={}): pass",
        "mutable-default-argument"
    ));
    assert!(!has(
        Language::Python,
        "def f(a, b=None): pass",
        "mutable-default-argument"
    ));

    let found = findings(
        Language::Java,
        "class A { int m() { return 1;\n int x = 2; } }",
    );
    assert!(
        found.contains(&("unreachable-code".to_string(), 2)),
        "{found:?}"
    );
    assert!(!has(
        Language::Javascript,
        "function f() { return g(); function g() { return 1; } }",
        "unreachable-code"
    ));
    assert!(!has(
        Language::C,
        "int f() { goto out; out: return 0; }",
        "unreachable-code"
    ));
    assert!(!has(
        Language::C,
        "int f(int a) { switch (a) { case 1: return 1; case 2: return 2; } return 0; }",
        "unreachable-code"
    ));
}

#[test]
fn sql_injection() {
    let cases: &[(Language, &str)] = &[
        (Language::Python, "cursor.execute(\"SELECT * FROM t WHERE id = \" + uid)"),
        (Language::Python, "cursor.execute(f\"DELETE FROM t WHERE id = {uid}\")"),
        (Language::Python, "cursor.execute(\"SELECT * FROM t WHERE id = %s\" % uid)"),
        (Language::Python, "cursor.execute(\"SELECT * FROM t WHERE id = {}\".format(uid))"),
        (Language::Python, "def f(uid):\n    q = \"SELECT * FROM t WHERE id = \" + uid\n    cursor.execute(q)\n"),
        (Language::Javascript, "db.query(`SELECT * FROM t WHERE id = ${id}`)"),
        (Language::Go, "package p\nfunc f() { db.Query(fmt.Sprintf(\"SELECT * FROM t WHERE id = %s\", id)) }"),
        (Language::Java, "class A { void m() { stmt.executeQuery(\"SELECT * FROM t WHERE id = \" + id); } }"),
        (Language::Csharp, "class A { void M() { db.Database.ExecuteSqlRaw($\"DELETE FROM t WHERE id = {id}\"); } }"),
        (Language::Csharp, "class A { void M() { var c = new SqlCommand(\"SELECT * FROM t WHERE a = \" + a, conn); } }"),
        (Language::Kotlin, "fun f() { db.rawQuery(\"SELECT * FROM t WHERE id = $id\", null) }"),
        (Language::Rust, "fn f() { conn.execute(&format!(\"DELETE FROM t WHERE id = {}\", id), []); }"),
    ];
    for (lang, src) in cases {
        assert!(has(*lang, src, "sql-injection"), "{lang:?}: {src}");
    }
    let safe: &[(Language, &str)] = &[
        (Language::Python, "cursor.execute(\"SELECT * FROM t WHERE id = %s\", (uid,))"),
        (Language::Python, "COLS = \"a, b\"\ncursor.execute(\"SELECT \" + COLS + \" FROM t\")"),
        (Language::Javascript, "db.query(\"SELECT * FROM t WHERE id = $1\", [id])"),
        (Language::Javascript, "logger.get(\"Select a file from the list\" + name)"),
        (Language::Go, "package p\nconst cols = `a, b`\nfunc f() { db.Query(`SELECT `+cols+` FROM t WHERE id = $1`, id) }"),
        (Language::Java, "class A { static final String C = \"a\"; void m() { stmt.executeQuery(\"SELECT \" + C + \" FROM t\"); } }"),
        (Language::Go, "package p\nfunc f() { fmt.Println(\"SELECT * FROM t WHERE id = \" + id) }"),
    ];
    for (lang, src) in safe {
        assert!(!has(*lang, src, "sql-injection"), "{lang:?}: {src}");
    }
}

#[test]
fn command_and_code_execution() {
    assert!(has(
        Language::Python,
        "import os\nos.system(\"rm -rf \" + path)",
        "command-execution"
    ));
    assert!(has(
        Language::Python,
        "subprocess.run(cmd, shell=True)",
        "command-execution"
    ));
    assert!(!has(
        Language::Python,
        "subprocess.run([\"ls\", path])",
        "command-execution"
    ));
    assert!(!has(
        Language::Python,
        "os.system(\"ls\")",
        "command-execution"
    ));
    assert!(has(
        Language::Javascript,
        "child_process.exec(`ls ${dir}`)",
        "command-execution"
    ));
    assert!(!has(
        Language::Javascript,
        "const m = re.exec(text)",
        "command-execution"
    ));
    assert!(has(
        Language::Go,
        "package p\nfunc f() { exec.Command(\"sh\", \"-c\", cmd) }",
        "command-execution"
    ));
    assert!(!has(
        Language::Go,
        "package p\nfunc f() { exec.Command(\"git\", \"clone\", url) }",
        "command-execution"
    ));
    assert!(has(
        Language::Java,
        "class A { void m() { Runtime.getRuntime().exec(cmd); } }",
        "command-execution"
    ));
    assert!(has(
        Language::C,
        "void f(char *c) { system(c); }",
        "command-execution"
    ));
    assert!(!has(
        Language::C,
        "void f() { system(\"clear\"); }",
        "command-execution"
    ));

    assert!(has(
        Language::Python,
        "eval(expr)",
        "dynamic-code-execution"
    ));
    assert!(!has(
        Language::Python,
        "eval(\"1 + 1\")",
        "dynamic-code-execution"
    ));
    assert!(has(
        Language::Javascript,
        "const f = new Function(\"a\", body);",
        "dynamic-code-execution"
    ));
    assert!(has(
        Language::Javascript,
        "setTimeout(\"tick()\", 10)",
        "dynamic-code-execution"
    ));
    assert!(!has(
        Language::Javascript,
        "setTimeout(tick, 10)",
        "dynamic-code-execution"
    ));
}

#[test]
fn crypto_tls_and_deserialization() {
    assert!(has(Language::Python, "h = hashlib.md5(data)", "weak-hash"));
    assert!(!has(
        Language::Python,
        "h = hashlib.md5(data, usedforsecurity=False)",
        "weak-hash"
    ));
    assert!(!has(
        Language::Python,
        "h = hashlib.sha256(data)",
        "weak-hash"
    ));
    assert!(has(
        Language::Javascript,
        "crypto.createHash('sha1')",
        "weak-hash"
    ));
    assert!(!has(
        Language::Javascript,
        "crypto.createHash('sha256')",
        "weak-hash"
    ));
    assert!(has(
        Language::Go,
        "package p\nfunc f() { md5.Sum(b) }",
        "weak-hash"
    ));
    assert!(has(
        Language::Java,
        "class A { void m() { MessageDigest.getInstance(\"SHA-1\"); } }",
        "weak-hash"
    ));
    assert!(has(
        Language::Csharp,
        "class A { void M() { var h = SHA1.Create(); } }",
        "weak-hash"
    ));

    assert!(has(
        Language::Python,
        "requests.get(url, verify=False)",
        "tls-verification-disabled"
    ));
    assert!(!has(
        Language::Python,
        "requests.get(url, verify=True)",
        "tls-verification-disabled"
    ));
    assert!(has(
        Language::Javascript,
        "https.request({ rejectUnauthorized: false })",
        "tls-verification-disabled"
    ));
    assert!(has(
        Language::Javascript,
        "process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';",
        "tls-verification-disabled"
    ));
    assert!(has(
        Language::Go,
        "package p\nvar c = &tls.Config{InsecureSkipVerify: true}",
        "tls-verification-disabled"
    ));
    assert!(!has(
        Language::Go,
        "package p\nvar c = &tls.Config{InsecureSkipVerify: false}",
        "tls-verification-disabled"
    ));
    assert!(has(
        Language::Csharp,
        "class A { void M() { h.ServerCertificateCustomValidationCallback = (a, b, c, d) => true; } }",
        "tls-verification-disabled"
    ));
    assert!(has(
        Language::Rust,
        "fn f() { let c = b.danger_accept_invalid_certs(true); }",
        "tls-verification-disabled"
    ));

    assert!(has(
        Language::Python,
        "obj = pickle.loads(data)",
        "unsafe-deserialization"
    ));
    assert!(has(
        Language::Python,
        "obj = yaml.load(data)",
        "unsafe-deserialization"
    ));
    assert!(!has(
        Language::Python,
        "obj = yaml.load(data, Loader=yaml.SafeLoader)",
        "unsafe-deserialization"
    ));
    assert!(has(
        Language::Java,
        "class A { void m() { new ObjectInputStream(in).readObject(); } }",
        "unsafe-deserialization"
    ));
    assert!(has(
        Language::Csharp,
        "class A { void M() { var f = new BinaryFormatter(); } }",
        "unsafe-deserialization"
    ));
}

#[test]
fn native_and_rust_rules() {
    assert!(has(
        Language::C,
        "void f(char *d, char *s) { strcpy(d, s); }",
        "unbounded-string-copy"
    ));
    assert!(has(
        Language::Cpp,
        "void f(char *d) { std::sprintf(d, \"%d\", 1); }",
        "unbounded-string-copy"
    ));
    assert!(!has(
        Language::C,
        "void f(char *d, char *s) { snprintf(d, 4, \"%s\", s); }",
        "unbounded-string-copy"
    ));
    assert!(has(
        Language::Rust,
        "fn f() { unsafe { g(); } }",
        "unsafe-block"
    ));
}

#[test]
fn findings_carry_function_and_stable_context() {
    let a = parse(
        Language::Python,
        b"def handler(x):\n    eval(x)\n",
        &ParseOptions::default(),
    )
    .unwrap();
    let b = parse(
        Language::Python,
        b"\n\n# moved\ndef handler(x):\n    eval(x)\n",
        &ParseOptions::default(),
    )
    .unwrap();
    let (fa, fb) = (&a.findings[0], &b.findings[0]);
    assert_eq!(fa.function.as_deref(), Some("handler"));
    assert_eq!(
        fa.context, fb.context,
        "moving code must not change the context hash"
    );
    assert_ne!(fa.span.start_line, fb.span.start_line);
}
