//! Behavioural tests for the extraction adapters.

use usie_ir::{ExportKind, FileIr, ImportKind, Language, ReferenceKind, Symbol, SymbolKind};
use usie_parser::{parse, Limits, ParseOptions};

fn ir(language: Language, src: &str) -> FileIr {
    parse(language, src.as_bytes(), &ParseOptions::default()).expect("parse")
}

fn sym<'a>(ir: &'a FileIr, qualified: &str) -> &'a Symbol {
    ir.symbols
        .iter()
        .find(|s| s.qualified_name == qualified)
        .unwrap_or_else(|| panic!("symbol {qualified} not found in {:?}", names(ir)))
}

fn names(ir: &FileIr) -> Vec<&str> {
    ir.symbols
        .iter()
        .map(|s| s.qualified_name.as_str())
        .collect()
}

fn has_ref(ir: &FileIr, kind: ReferenceKind, scope: Option<&str>, path: &[&str]) -> bool {
    let scope_id = scope.map(|q| sym(ir, q).id);
    ir.references.iter().any(|r| {
        r.kind == kind
            && r.scope == scope_id
            && r.path.iter().map(String::as_str).eq(path.iter().copied())
    })
}

const PY: &str = r#""""Module doc."""
import os.path, sys as system
from . import sibling
from ..pkg.mod import thing as t, other
from x import *
__all__ = ["Service", "run"]
CONST = make()

@dataclass
class Service(Base, metaclass=Meta):
    """A service."""
    limit = 3

    def __init__(self, a, *args, b=1, **kw):
        self.a = a
        super().__init__()

    @staticmethod
    async def run(x: int) -> str:
        import json
        y = [z for z in x]
        self.helper(); Service.run(1); os.path.join("a"); t(); obj.method(); json.dumps(y)
        def inner(): pass
        return inner()

def run():
    global CONST
    CONST = 2
    with open("f") as fh:
        fh.read()
"#;

#[test]
fn python_symbols() {
    let ir = ir(Language::Python, PY);
    assert!(ir.diagnostics.is_empty());
    let service = sym(&ir, "Service");
    assert_eq!(service.kind, SymbolKind::Class);
    assert_eq!(service.bases.len(), 1);
    assert_eq!(service.bases[0].segments, ["Base"]);
    assert_eq!(service.doc.as_deref(), Some("A service."));
    assert_eq!(service.attrs["metaclass"], "Meta");
    assert_eq!(service.attrs["decorators"][0], "dataclass");

    let init = sym(&ir, "Service.__init__");
    assert_eq!(init.kind, SymbolKind::Method);
    assert_eq!(init.parent, Some(service.id));
    assert_eq!(init.locals, ["a", "args", "b", "kw", "self"]);
    assert_eq!(
        init.signature.as_deref(),
        Some("(self, a, *args, b=1, **kw)")
    );

    let run = sym(&ir, "Service.run");
    assert_eq!(run.attrs["async"], true);
    assert_eq!(run.attrs["static"], true);
    assert_eq!(run.signature.as_deref(), Some("(x: int) -> str"));
    assert!(run.locals.contains(&"json".to_string()));
    assert!(run.locals.contains(&"z".to_string()));

    assert_eq!(sym(&ir, "Service.run.inner").kind, SymbolKind::Function);
    assert_eq!(sym(&ir, "Service.limit").attrs["member"], true);
    assert_eq!(sym(&ir, "CONST").kind, SymbolKind::Variable);

    // `global CONST` means the assignment does not create a local.
    assert_eq!(sym(&ir, "run").locals, ["fh"]);
}

#[test]
fn python_imports_and_exports() {
    let ir = ir(Language::Python, PY);
    let get = |module: &str, local: Option<&str>| {
        ir.imports
            .iter()
            .find(|i| i.module == module && i.local.as_deref() == local)
            .unwrap_or_else(|| panic!("import {module} {local:?}"))
    };
    assert_eq!(get("os.path", Some("os.path")).kind, ImportKind::Namespace);
    assert_eq!(get("sys", Some("system")).kind, ImportKind::Namespace);
    let sibling = get("", Some("sibling"));
    assert_eq!((sibling.level, sibling.kind), (1, ImportKind::Named));
    let thing = get("pkg.mod", Some("t"));
    assert_eq!((thing.level, thing.imported.as_deref()), (2, Some("thing")));
    assert_eq!(get("x", None).kind, ImportKind::Wildcard);
    assert_eq!(
        get("json", Some("json")).scope,
        Some(sym(&ir, "Service.run").id)
    );

    let exported: Vec<&str> = ir.exports.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(exported, ["Service", "run"]);
    assert_eq!(ir.attrs["dunder_all"], true);
}

#[test]
fn python_references() {
    let ir = ir(Language::Python, PY);
    assert!(has_ref(&ir, ReferenceKind::Call, None, &["make"]));
    assert!(has_ref(&ir, ReferenceKind::Decorator, None, &["dataclass"]));
    assert!(has_ref(
        &ir,
        ReferenceKind::Decorator,
        Some("Service"),
        &["staticmethod"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("Service.__init__"),
        &["super", "__init__"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("Service.run"),
        &["self", "helper"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("Service.run"),
        &["os", "path", "join"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("Service.run"),
        &["inner"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("run"),
        &["fh", "read"]
    ));
}

#[test]
fn python_dynamic_all_is_flagged() {
    let ir = ir(Language::Python, "__all__ = build()\n");
    assert_eq!(ir.attrs["dunder_all_dynamic"], true);
    assert!(ir.exports.is_empty());
}

const TS: &str = r#"import React, { useState as useS, type FC } from 'react';
import * as path from 'node:path';
import './side.css';
import type { Foo } from './types';
import cfg = require('./cfg');
export * from './all';
export * as ns from './ns';
export { a as b, default as c } from './re';

/** Greets. */
export function greet(name: string): string;
export function greet(name: string, n?: number): string { return helper(name) + path.join('a'); }
function helper(x: string) { const y = x; return y.trim(); }
export const arrow = async (a: number, { b, c: d }: any = {}) => { helper('x'); new Widget(); };
export default class Widget extends Base<number> implements Foo, ns.Bar {
  static count = compute();
  private handler = () => { this.run(); };
  @memo() run(): void { super.run(); this.handler(); Widget.make(); obj.thing.do(); getIt()(); }
  static make() { return import('./lazy'); }
}
export interface Shape extends Foo { area(): number; }
export type Id = string;
export enum Color { Red, Green }
namespace NS { export function inner() { helper('z'); } }
const local = 1, other = make(() => { const q = 1; q; });
export { local, other as renamed };
app.get('/', (req, res) => { function nested() {} nested(); res.send(); });
const View: FC = () => <Layout.Main><Button onClick={() => greet('a')} /><div/></Layout.Main>;
"#;

#[test]
fn typescript_symbols() {
    let ir = ir(Language::Tsx, TS);
    assert!(ir.diagnostics.is_empty(), "{:?}", ir.diagnostics);

    let greet = sym(&ir, "greet");
    assert_eq!(
        ir.symbols.iter().filter(|s| s.name == "greet").count(),
        1,
        "overloads merge"
    );
    assert!(greet.exported);
    assert_eq!(greet.doc.as_deref(), Some("Greets."));
    assert_eq!(
        greet.signature.as_deref(),
        Some("(name: string, n?: number): string")
    );
    assert_eq!(greet.locals, ["n", "name"]);
    assert!(!greet.attrs.contains_key("declaration_only"));

    let arrow = sym(&ir, "arrow");
    assert_eq!(arrow.kind, SymbolKind::Function);
    assert_eq!(arrow.locals, ["a", "b", "d"]);
    assert_eq!(arrow.attrs["async"], true);

    let widget = sym(&ir, "Widget");
    assert_eq!(widget.kind, SymbolKind::Class);
    assert_eq!(widget.bases[0].segments, ["Base"]);
    let implements: Vec<_> = widget
        .implements
        .iter()
        .map(|p| p.segments.join("."))
        .collect();
    assert_eq!(implements, ["Foo", "ns.Bar"]);
    assert_eq!(sym(&ir, "Widget.count").kind, SymbolKind::Variable);
    assert_eq!(sym(&ir, "Widget.handler").kind, SymbolKind::Method);
    assert_eq!(sym(&ir, "Widget.run").attrs["decorators"][0], "memo()");
    assert_eq!(sym(&ir, "Widget.make").attrs["static"], true);

    let shape = sym(&ir, "Shape");
    assert_eq!(shape.kind, SymbolKind::Interface);
    assert_eq!(shape.bases[0].segments, ["Foo"]);
    assert_eq!(sym(&ir, "Shape.area").attrs["declaration_only"], true);
    assert_eq!(sym(&ir, "Id").attrs["type_kind"], "alias");
    assert_eq!(sym(&ir, "Color").attrs["type_kind"], "enum");
    assert_eq!(sym(&ir, "NS").kind, SymbolKind::Namespace);
    assert!(sym(&ir, "NS.inner").exported);
    assert!(!sym(&ir, "helper").exported);
    assert!(sym(&ir, "local").exported, "export clause marks the symbol");
    assert_eq!(sym(&ir, "other").locals, ["q"]);
    assert_eq!(sym(&ir, "nested").attrs["in_anonymous_scope"], true);
    assert_eq!(
        ir.attrs["anonymous_locals"],
        serde_json::json!(["nested", "req", "res"])
    );
}

#[test]
fn typescript_imports_and_exports() {
    let ir = ir(Language::Tsx, TS);
    let get = |module: &str, local: Option<&str>| {
        ir.imports
            .iter()
            .find(|i| i.module == module && i.local.as_deref() == local)
            .unwrap_or_else(|| panic!("import {module} {local:?}"))
    };
    assert_eq!(get("react", Some("React")).kind, ImportKind::Default);
    let use_s = get("react", Some("useS"));
    assert_eq!(
        (use_s.kind, use_s.imported.as_deref(), use_s.type_only),
        (ImportKind::Named, Some("useState"), false)
    );
    assert!(get("react", Some("FC")).type_only);
    assert_eq!(get("node:path", Some("path")).kind, ImportKind::Namespace);
    assert_eq!(get("./side.css", None).kind, ImportKind::SideEffect);
    assert!(get("./types", Some("Foo")).type_only);
    assert_eq!(get("./cfg", Some("cfg")).kind, ImportKind::Namespace);
    let lazy = get("./lazy", None);
    assert_eq!(
        (lazy.kind, lazy.scope),
        (ImportKind::Dynamic, Some(sym(&ir, "Widget.make").id))
    );

    let exports: Vec<(ExportKind, &str, Option<&str>, Option<&str>)> = ir
        .exports
        .iter()
        .map(|e| {
            (
                e.kind,
                e.name.as_str(),
                e.local.as_deref(),
                e.source.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        exports,
        [
            (ExportKind::Wildcard, "*", None, Some("./all")),
            (ExportKind::Namespace, "ns", None, Some("./ns")),
            (ExportKind::Named, "b", Some("a"), Some("./re")),
            (ExportKind::Named, "c", Some("default"), Some("./re")),
            (ExportKind::Named, "greet", Some("greet"), None),
            (ExportKind::Named, "arrow", Some("arrow"), None),
            (ExportKind::Default, "default", Some("Widget"), None),
            (ExportKind::Named, "Shape", Some("Shape"), None),
            (ExportKind::Named, "Id", Some("Id"), None),
            (ExportKind::Named, "Color", Some("Color"), None),
            (ExportKind::Named, "local", Some("local"), None),
            (ExportKind::Named, "renamed", Some("other"), None),
        ]
    );
}

#[test]
fn typescript_references() {
    let ir = ir(Language::Tsx, TS);
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("greet"),
        &["helper"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("greet"),
        &["path", "join"]
    ));
    assert!(has_ref(&ir, ReferenceKind::New, Some("arrow"), &["Widget"]));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("Widget.count"),
        &["compute"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("Widget.handler"),
        &["this", "run"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("Widget.run"),
        &["super", "run"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("Widget.run"),
        &["Widget", "make"]
    ));
    assert!(has_ref(
        &ir,
        ReferenceKind::Call,
        Some("NS.inner"),
        &["helper"]
    ));
    assert!(has_ref(&ir, ReferenceKind::Call, None, &["app", "get"]));
    assert!(has_ref(
        &ir,
        ReferenceKind::Jsx,
        Some("View"),
        &["Layout", "Main"]
    ));
    assert!(has_ref(&ir, ReferenceKind::Jsx, Some("View"), &["Button"]));
    assert!(has_ref(&ir, ReferenceKind::Call, Some("View"), &["greet"]));
    // Host elements are not component references.
    assert!(!ir.references.iter().any(|r| r.path == ["div"]));
    // A call on a computed receiver has no static path.
    assert!(ir
        .references
        .iter()
        .any(|r| r.path.is_empty() && r.member.is_none() && r.text == "getIt()"));
}

const JS: &str = r#"const fs = require('fs');
const { join, resolve: res } = require('path');
require('./polyfill');
class A extends B.C { #priv() {} go() { this.#priv(); } }
function f() { var g = function named() {}; g(); }
exports.f = f;
module.exports.A = A;
module.exports = { f, a: A, m() {} };
if (x) { let blockScoped = 1; }
(function () { var iife = 1; f(); })();
"#;

#[test]
fn javascript_commonjs() {
    let ir = ir(Language::Javascript, JS);
    assert!(ir.diagnostics.is_empty());
    assert_eq!(ir.attrs["commonjs"], true);
    let imports: Vec<(ImportKind, &str, Option<&str>, Option<&str>)> = ir
        .imports
        .iter()
        .map(|i| {
            (
                i.kind,
                i.module.as_str(),
                i.imported.as_deref(),
                i.local.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        imports,
        [
            (ImportKind::Namespace, "fs", None, Some("fs")),
            (ImportKind::Named, "path", Some("join"), Some("join")),
            (ImportKind::Named, "path", Some("resolve"), Some("res")),
            (ImportKind::SideEffect, "./polyfill", None, None),
        ]
    );
    let exports: Vec<(&str, Option<&str>)> = ir
        .exports
        .iter()
        .map(|e| (e.name.as_str(), e.local.as_deref()))
        .collect();
    assert_eq!(
        exports,
        [
            ("f", Some("f")),
            ("A", Some("A")),
            ("a", Some("A")),
            ("m", None)
        ]
    );
    assert_eq!(sym(&ir, "A").bases[0].segments, ["B", "C"]);
    assert_eq!(sym(&ir, "A.#priv").kind, SymbolKind::Method);
    assert_eq!(sym(&ir, "f").locals, ["g", "named"]);
    assert_eq!(
        ir.attrs["anonymous_locals"],
        serde_json::json!(["blockScoped", "iife"])
    );
}

#[test]
fn default_export_forms() {
    let anon = ir(Language::Javascript, "export default () => run();\n");
    assert_eq!(sym(&anon, "default").kind, SymbolKind::Function);
    assert_eq!(anon.exports[0].local.as_deref(), Some("default"));
    assert!(has_ref(
        &anon,
        ReferenceKind::Call,
        Some("default"),
        &["run"]
    ));

    let class = ir(Language::Typescript, "export default class { go() {} }\n");
    assert_eq!(sym(&class, "default").kind, SymbolKind::Class);
    assert_eq!(sym(&class, "default.go").kind, SymbolKind::Method);

    let ident = ir(Language::Typescript, "function a() {}\nexport default a;\n");
    assert_eq!(ident.exports[0].local.as_deref(), Some("a"));
    assert!(sym(&ident, "a").exported);

    let assign = ir(Language::Typescript, "class K {}\nexport = K;\n");
    assert_eq!(assign.attrs["export_assignment"], true);
    assert_eq!(assign.exports[0].local.as_deref(), Some("K"));
}

#[test]
fn syntax_errors_become_diagnostics() {
    for (language, src) in [
        (
            Language::Python,
            "def ok():\n    pass\n\ndef broken(:\n    pass\n",
        ),
        (Language::Typescript, "function ok() {}\nconst broken = ;\n"),
        (Language::Javascript, "function ok() {}\nclass {\n"),
    ] {
        let ir = ir(language, src);
        assert!(ir.has_syntax_errors(), "{language:?}");
        assert!(!ir.diagnostics.is_empty());
        assert!(
            ir.symbols.iter().any(|s| s.name == "ok"),
            "recovery keeps valid symbols"
        );
    }
}

#[test]
fn limits_truncate_instead_of_failing() {
    let src: String = (0..50)
        .map(|i| format!("def f{i}():\n    g{i}()\n"))
        .collect();
    let options = ParseOptions {
        limits: Limits {
            max_symbols: 10,
            max_references: 5,
            max_imports: 1,
        },
        ..ParseOptions::default()
    };
    let ir = parse(Language::Python, src.as_bytes(), &options).unwrap();
    assert_eq!(ir.symbols.len(), 10);
    assert_eq!(ir.references.len(), 5);
    assert_eq!(ir.attrs["truncated"], true);
}

#[test]
fn deep_nesting_is_bounded() {
    let depth = 5_000;
    let src = format!("x = {}1{}\n", "(".repeat(depth), ")".repeat(depth));
    let ir = ir(Language::Python, &src);
    assert_eq!(ir.attrs["truncated"], true);

    let src = format!("const x = {}1{};\n", "[".repeat(depth), "]".repeat(depth));
    let ts = parse(
        Language::Typescript,
        src.as_bytes(),
        &ParseOptions::default(),
    )
    .unwrap();
    assert_eq!(ts.attrs["truncated"], true);
}

#[test]
fn timeout_is_reported() {
    let src = "x = 1\n".repeat(200_000);
    let options = ParseOptions {
        timeout: std::time::Duration::ZERO,
        ..ParseOptions::default()
    };
    assert_eq!(
        parse(Language::Python, src.as_bytes(), &options),
        Err(usie_parser::ParseError::Timeout)
    );
}

#[test]
fn empty_and_non_utf8_input() {
    for language in [
        Language::Python,
        Language::Javascript,
        Language::Typescript,
        Language::Tsx,
    ] {
        let empty = parse(language, b"", &ParseOptions::default()).unwrap();
        assert!(empty.symbols.is_empty());
        let _ = parse(
            language,
            &[0xff, 0xfe, 0x00, b'a', 0x80, b'(', b'\n'],
            &ParseOptions::default(),
        )
        .unwrap();
    }
}

#[test]
fn output_is_deterministic_modulo_timing() {
    let mut a = ir(Language::Tsx, TS);
    let mut b = ir(Language::Tsx, TS);
    a.stats.parse_micros = 0;
    a.stats.extract_micros = 0;
    b.stats.parse_micros = 0;
    b.stats.extract_micros = 0;
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}
