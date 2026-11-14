//! End-to-end resolution tests: real source is parsed, then resolved.

use usie_ir::{Confidence, Language};
use usie_parser::{parse, ParseOptions};
use usie_semantic::{
    resolve, DeclaredDependency, EdgeRecord, Outcome, Project, ProjectConfig, ReferenceRecord,
    Resolution, SourceFile, TsConfig, WorkspacePackage,
};

fn language(path: &str) -> Option<Language> {
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "py" => Language::Python,
        "js" | "jsx" | "mjs" | "cjs" => Language::Javascript,
        "ts" => Language::Typescript,
        "tsx" => Language::Tsx,
        _ => return None,
    })
}

fn project(config: ProjectConfig, files: &[(&str, &str)]) -> Resolution {
    let mut config = config;
    let mut sources = Vec::new();
    for (path, src) in files {
        config.paths.push(path.to_string());
        if let Some(lang) = language(path) {
            let ir = parse(lang, src.as_bytes(), &ParseOptions::default()).unwrap();
            sources.push(SourceFile {
                path: path.to_string(),
                ir,
            });
        }
    }
    resolve(&Project {
        config,
        files: sources,
    })
}

fn edge<'a>(r: &'a Resolution, src: &str, kind: &str, dst: &str) -> &'a EdgeRecord {
    r.edges
        .iter()
        .find(|e| e.src == src && e.kind == kind && e.dst == dst)
        .unwrap_or_else(|| {
            let near: Vec<String> = r
                .edges
                .iter()
                .filter(|e| e.kind == kind && (e.src == src || e.dst == dst))
                .map(|e| format!("{} -{}-> {}", e.src, e.kind, e.dst))
                .collect();
            panic!("no edge {src} -{kind}-> {dst}; similar: {near:#?}")
        })
}

fn no_edge(r: &Resolution, src: &str, kind: &str) {
    let found: Vec<&EdgeRecord> = r
        .edges
        .iter()
        .filter(|e| e.src == src && e.kind == kind)
        .collect();
    assert!(found.is_empty(), "unexpected edges: {found:#?}");
}

fn reference<'a>(r: &'a Resolution, path: &str, text: &str) -> &'a ReferenceRecord {
    r.references
        .iter()
        .find(|x| x.path == path && x.text == text)
        .unwrap_or_else(|| panic!("no reference {text} in {path}"))
}

fn check(e: &EdgeRecord, confidence: Confidence, rule: &str) {
    assert_eq!((e.confidence, e.rule.as_str()), (confidence, rule), "{e:?}");
}

use Confidence::{Confirmed, Inferred, Probable};

#[test]
fn typescript_imports_calls_and_reexports() {
    let r = project(
        ProjectConfig {
            dependencies: vec![DeclaredDependency { ecosystem: "npm".into(), name: "react".into() }],
            ..Default::default()
        },
        &[
            ("src/math.ts", "export function add(a: number, b: number) { return a + b; }\nexport default function mul() {}\nfunction hidden() {}\n"),
            ("src/index.ts", "export * from './math';\nexport { add as plus } from './math';\nexport * as math from './math';\n"),
            ("src/style.css", "body {}"),
            (
                "src/app.ts",
                "import { add, plus } from './index';\nimport mul from './math.js';\nimport * as m from './math';\nimport React from 'react';\nimport fs from 'node:fs';\nimport _ from 'lodash';\nimport './style.css';\nimport { gone } from './missing';\nimport { nope } from './math';\n\
                 export function run(cb: () => void) {\n  add(1, 2); plus(1, 2); mul(); m.add(1, 2); React.createElement('a'); fs.readFileSync('x'); _.map([]);\n  cb(); gone(); nope(); parseInt('1'); mystery();\n}\n",
            ),
        ],
    );
    let run = "sym:src/app.ts#run";
    let add = "sym:src/math.ts#add";

    // Through `export *`, a renamed re-export, a default import written with
    // the emitted `.js` extension, and a namespace import.
    let calls = edge(&r, run, "CALLS", add);
    assert_eq!(calls.count, 3);
    check(calls, Confirmed, "es.export.wildcard");
    assert_eq!(calls.evidence.as_ref().unwrap().line, 11);
    check(
        edge(&r, run, "CALLS", "sym:src/math.ts#mul"),
        Confirmed,
        "es.export.local",
    );

    check(
        edge(&r, run, "CALLS", "ext:npm:react"),
        Confirmed,
        "es.import.declared-dependency",
    );
    check(
        edge(&r, run, "CALLS", "ext:node:fs"),
        Confirmed,
        "es.import.node-builtin",
    );
    check(
        edge(&r, run, "CALLS", "ext:npm:lodash"),
        Probable,
        "es.import.undeclared-package",
    );

    check(
        edge(&r, "file:src/app.ts", "IMPORTS", "file:src/index.ts"),
        Confirmed,
        "es.import.relative",
    );
    check(
        edge(&r, "file:src/app.ts", "IMPORTS", "file:src/math.ts"),
        Confirmed,
        "es.import.relative",
    );
    edge(&r, "file:src/app.ts", "IMPORTS", "file:src/style.css");
    edge(&r, "file:src/index.ts", "IMPORTS", "file:src/math.ts");
    edge(&r, "file:src/math.ts", "EXPORTS", add);
    check(
        edge(&r, "file:src/index.ts", "EXPORTS", add),
        Confirmed,
        "es.export.reexport",
    );
    edge(&r, "file:src/math.ts", "CONTAINS", add);

    // What cannot be established is reported, with a reason, and not guessed.
    let cb = reference(&r, "src/app.ts", "cb");
    assert_eq!(
        (cb.outcome, cb.rule.as_str()),
        (Outcome::Unresolved, "scope.local-binding")
    );
    let missing = reference(&r, "src/app.ts", "./missing");
    assert_eq!(
        (missing.outcome, missing.confidence),
        (Outcome::Unresolved, Confidence::Unknown)
    );
    assert!(missing.reason.as_ref().unwrap().contains("no file"));
    assert_eq!(
        reference(&r, "src/app.ts", "gone").outcome,
        Outcome::Unresolved
    );
    let nope = reference(&r, "src/app.ts", "nope");
    assert_eq!(
        (nope.outcome, nope.rule.as_str()),
        (Outcome::Unresolved, "es.export.not-found")
    );
    assert_eq!(
        reference(&r, "src/app.ts", "parseInt").outcome,
        Outcome::Builtin
    );
    let mystery = reference(&r, "src/app.ts", "mystery");
    assert_eq!(
        (mystery.outcome, mystery.rule.as_str()),
        (Outcome::Unresolved, "scope.unbound")
    );

    let react = r.nodes.iter().find(|n| n.key == "ext:npm:react").unwrap();
    assert_eq!(
        (react.kind.as_str(), react.attrs["declared"].as_bool()),
        ("package", Some(true))
    );
    assert_eq!(r.stats.references_by_outcome["unresolved"], 5);
    assert_eq!(r.stats.files, 3);
}

#[test]
fn typescript_classes_and_members() {
    let r = project(
        ProjectConfig::default(),
        &[
            ("base.ts", "export class Base { save() {} static create() { return new Base(); } }\nexport interface Store { get(): void }\n"),
            (
                "user.ts",
                "import { Base, Store } from './base';\n\
                 export class User extends Base implements Store {\n  constructor() { super(); }\n  get() {}\n  rename() { this.save(); this.get(); this.missing(); super.save(); this.repo.persistAll(); }\n}\n\
                 export class Repo { persistAll() {} }\n\
                 export function make(u: any) { const x = new User(); User.create(); Base.create(); u.persistAll(); u.get(); return x; }\n",
            ),
        ],
    );
    check(
        edge(&r, "sym:user.ts#User", "EXTENDS", "sym:base.ts#Base"),
        Confirmed,
        "es.export.local",
    );
    check(
        edge(&r, "sym:user.ts#User", "IMPLEMENTS", "sym:base.ts#Store"),
        Confirmed,
        "es.export.local",
    );
    let rename = "sym:user.ts#User.rename";
    // Inherited through the resolved base class; dispatch is dynamic.
    let save = edge(&r, rename, "CALLS", "sym:base.ts#Base.save");
    assert_eq!(save.count, 2);
    check(save, Probable, "es.call.this-method");
    check(
        edge(&r, rename, "CALLS", "sym:user.ts#User.get"),
        Probable,
        "es.call.this-method",
    );
    check(
        edge(
            &r,
            "sym:user.ts#User.constructor",
            "CALLS",
            "sym:base.ts#Base",
        ),
        Probable,
        "es.call.super-constructor",
    );
    assert_eq!(
        reference(&r, "user.ts", "this.missing").outcome,
        Outcome::Unresolved
    );

    let make = "sym:user.ts#make";
    check(
        edge(&r, make, "INSTANTIATES", "sym:user.ts#User"),
        Confirmed,
        "scope.module-symbol",
    );
    // `User.create()` finds the static member on the base class (probable);
    // `Base.create()` names it directly (confirmed). The edge keeps the
    // strongest evidence and counts both occurrences.
    let create = edge(&r, make, "CALLS", "sym:base.ts#Base.create");
    assert_eq!(create.count, 2);
    check(create, Confirmed, "es.export.local");
    assert_eq!(reference(&r, "user.ts", "User.create").confidence, Probable);
    check(
        edge(
            &r,
            "sym:base.ts#Base.create",
            "INSTANTIATES",
            "sym:base.ts#Base",
        ),
        Confirmed,
        "scope.module-symbol",
    );

    // Unknown receiver: only a repository-unique, non-ubiquitous method name
    // yields an edge, and it is marked as inferred.
    check(
        edge(&r, make, "CALLS", "sym:user.ts#Repo.persistAll"),
        Inferred,
        "heuristic.unique-method-name",
    );
    check(
        edge(&r, rename, "CALLS", "sym:user.ts#Repo.persistAll"),
        Inferred,
        "heuristic.unique-method-name",
    );
    assert_eq!(
        reference(&r, "user.ts", "u.get").outcome,
        Outcome::Unresolved
    );
}

#[test]
fn typescript_path_aliases_and_workspace_packages() {
    let mut paths = std::collections::BTreeMap::new();
    paths.insert("@/*".to_string(), vec!["src/*".to_string()]);
    paths.insert("@lib".to_string(), vec!["src/lib/index.ts".to_string()]);
    let r = project(
        ProjectConfig {
            tsconfigs: vec![TsConfig { dir: "web".into(), base_url: Some(".".into()), paths }],
            packages: vec![WorkspacePackage { name: "@acme/core".into(), dir: "packages/core".into(), entries: vec!["lib/main.ts".into()] }],
            ..Default::default()
        },
        &[
            ("web/src/lib/index.ts", "export const helper = () => {};\n"),
            ("web/src/util/fmt.ts", "export function fmt() {}\n"),
            ("packages/core/lib/main.ts", "export function core() {}\n"),
            ("packages/core/src/extra.ts", "export function extra() {}\n"),
            (
                "web/src/page.tsx",
                "import { helper } from '@lib';\nimport { fmt } from '@/util/fmt';\nimport { fmt as viaBase } from 'src/util/fmt';\nimport { core } from '@acme/core';\nimport { extra } from '@acme/core/extra';\nimport { x } from '@/nowhere';\n\
                 export const Page = () => { helper(); fmt(); viaBase(); core(); extra(); return <Card />; };\nfunction Card() { return null; }\n",
            ),
        ],
    );
    let page = "sym:web/src/page.tsx#Page";
    check(
        edge(&r, page, "CALLS", "sym:web/src/lib/index.ts#helper"),
        Confirmed,
        "es.export.local",
    );
    let fmt = edge(&r, page, "CALLS", "sym:web/src/util/fmt.ts#fmt");
    assert_eq!((fmt.count, fmt.confidence), (2, Confirmed));
    check(
        edge(&r, page, "CALLS", "sym:packages/core/lib/main.ts#core"),
        Probable,
        "es.export.local",
    );
    check(
        edge(&r, page, "CALLS", "sym:packages/core/src/extra.ts#extra"),
        Probable,
        "es.export.local",
    );
    check(
        edge(&r, page, "REFERENCES", "sym:web/src/page.tsx#Card"),
        Confirmed,
        "scope.module-symbol",
    );
    check(
        edge(
            &r,
            "file:web/src/page.tsx",
            "IMPORTS",
            "file:web/src/lib/index.ts",
        ),
        Confirmed,
        "es.import.tsconfig-paths",
    );
    check(
        edge(
            &r,
            "file:web/src/page.tsx",
            "IMPORTS",
            "file:packages/core/lib/main.ts",
        ),
        Probable,
        "es.import.workspace-package",
    );
    let nowhere = reference(&r, "web/src/page.tsx", "@/nowhere");
    assert_eq!(
        (nowhere.outcome, nowhere.rule.as_str()),
        (Outcome::Unresolved, "es.import.tsconfig-paths")
    );
}

#[test]
fn commonjs_modules() {
    let r = project(
        ProjectConfig::default(),
        &[
            ("lib.js", "function work() {}\nfunction other() {}\nmodule.exports = { work, renamed: other };\n"),
            ("single.js", "function only() {}\nmodule.exports = only;\n"),
            ("main.js", "const lib = require('./lib');\nconst { work: w } = require('./lib');\nconst only = require('./single');\nfunction go() { lib.work(); lib.renamed(); w(); only(); }\n"),
        ],
    );
    let go = "sym:main.js#go";
    let work = edge(&r, go, "CALLS", "sym:lib.js#work");
    assert_eq!(work.count, 2);
    check(work, Confirmed, "es.import.relative");
    edge(&r, go, "CALLS", "sym:lib.js#other");
    edge(&r, go, "CALLS", "sym:single.js#only");
    edge(&r, "file:lib.js", "EXPORTS", "sym:lib.js#other");
}

#[test]
fn shadowing_and_anonymous_scopes() {
    let r = project(
        ProjectConfig::default(),
        &[(
            "a.ts",
            "function helper() {}\nfunction target() {}\n\
             function shadow(helper: any) { helper(); function inner() {} inner(); }\n\
             function ok() { helper(); const f = () => target(); }\n\
             items.forEach((target) => { target(); helper(); });\n",
        )],
    );
    no_edge(&r, "sym:a.ts#shadow", "INSTANTIATES");
    assert_eq!(
        r.edges
            .iter()
            .filter(|e| e.src == "sym:a.ts#shadow" && e.kind == "CALLS")
            .count(),
        1
    );
    check(
        edge(&r, "sym:a.ts#shadow", "CALLS", "sym:a.ts#shadow.inner"),
        Confirmed,
        "scope.nested-definition",
    );
    edge(&r, "sym:a.ts#ok", "CALLS", "sym:a.ts#helper");
    edge(&r, "sym:a.ts#ok", "CALLS", "sym:a.ts#target");
    // `target` is rebound by a callback parameter at module level, so the
    // module-level call cannot be attributed to the function `target`.
    assert!(!r
        .edges
        .iter()
        .any(|e| e.src == "file:a.ts" && e.dst == "sym:a.ts#target" && e.kind == "CALLS"));
    edge(&r, "file:a.ts", "CALLS", "sym:a.ts#helper");
}

#[test]
fn python_packages_and_imports() {
    let r = project(
        ProjectConfig {
            dependencies: vec![DeclaredDependency { ecosystem: "pypi".into(), name: "Requests".into() }],
            ..Default::default()
        },
        &[
            ("src/app/__init__.py", "from .core import Engine\n__all__ = ['Engine']\n"),
            ("src/app/core.py", "import os.path\nimport requests\nimport yaml\nfrom . import util\nfrom .util import slug as make_slug, missing\nfrom app.models.user import User\n\nclass Engine(Base):\n    def start(self):\n        util.slug('a'); make_slug('b'); os.path.join('a'); requests.get('u'); yaml.safe_load('')\n        u = User(); u.save(); self.stop(); self.nope(); print(u); missing()\n    def stop(self): pass\n"),
            ("src/app/util.py", "def slug(s): return s\n"),
            ("src/app/models/__init__.py", ""),
            ("src/app/models/user.py", "from ..core import Engine\nclass User:\n    def save(self): pass\n"),
            ("tests/test_app.py", "from app import Engine\nimport app.util\nfrom app.models import user\n\ndef test():\n    Engine().start(); app.util.slug('x'); user.User()\n"),
        ],
    );
    let start = "sym:src/app/core.py#Engine.start";
    let slug = edge(&r, start, "CALLS", "sym:src/app/util.py#slug");
    assert_eq!(slug.count, 2);
    check(slug, Confirmed, "py.import.submodule");
    check(
        edge(&r, start, "CALLS", "ext:python:os"),
        Confirmed,
        "py.import.stdlib",
    );
    check(
        edge(&r, start, "CALLS", "ext:python:requests"),
        Confirmed,
        "py.import.declared-dependency",
    );
    check(
        edge(&r, start, "CALLS", "ext:python:yaml"),
        Probable,
        "py.import.undeclared-module",
    );
    check(
        edge(&r, start, "INSTANTIATES", "sym:src/app/models/user.py#User"),
        Confirmed,
        "py.module-attribute",
    );
    check(
        edge(&r, start, "CALLS", "sym:src/app/core.py#Engine.stop"),
        Probable,
        "py.call.self-method",
    );
    assert_eq!(
        reference(&r, "src/app/core.py", "self.nope").outcome,
        Outcome::Unresolved
    );
    assert_eq!(
        reference(&r, "src/app/core.py", "print").outcome,
        Outcome::Builtin
    );
    let missing = reference(&r, "src/app/core.py", "missing");
    assert_eq!(
        (missing.outcome, missing.rule.as_str()),
        (Outcome::Unresolved, "py.module-attribute")
    );
    // `u` is a local whose type is not tracked; `save` is unique in the repository.
    check(
        edge(&r, start, "CALLS", "sym:src/app/models/user.py#User.save"),
        Inferred,
        "heuristic.unique-method-name",
    );
    // `Base` is not defined or imported anywhere.
    assert_eq!(
        reference(&r, "src/app/core.py", "Base").outcome,
        Outcome::Unresolved
    );

    check(
        edge(
            &r,
            "file:src/app/core.py",
            "IMPORTS",
            "file:src/app/util.py",
        ),
        Confirmed,
        "py.import.submodule",
    );
    edge(
        &r,
        "file:src/app/core.py",
        "IMPORTS",
        "file:src/app/__init__.py",
    );
    edge(
        &r,
        "file:src/app/core.py",
        "IMPORTS",
        "file:src/app/models/user.py",
    );
    edge(
        &r,
        "file:src/app/models/user.py",
        "IMPORTS",
        "file:src/app/core.py",
    );
    check(
        edge(
            &r,
            "file:src/app/__init__.py",
            "EXPORTS",
            "sym:src/app/core.py#Engine",
        ),
        Confirmed,
        "py.reexport",
    );
    let engine = r
        .nodes
        .iter()
        .find(|n| n.key == "sym:src/app/core.py#Engine")
        .unwrap();
    assert!(engine.exported);

    // From outside the package, through the `src` layout and a re-export.
    let test = "sym:tests/test_app.py#test";
    check(
        edge(&r, test, "INSTANTIATES", "sym:src/app/core.py#Engine"),
        Confirmed,
        "py.reexport",
    );
    edge(&r, test, "CALLS", "sym:src/app/util.py#slug");
    edge(&r, test, "INSTANTIATES", "sym:src/app/models/user.py#User");
    edge(
        &r,
        "file:tests/test_app.py",
        "IMPORTS",
        "file:src/app/models/user.py",
    );
}

#[test]
fn python_inheritance_and_wildcards() {
    let r = project(
        ProjectConfig::default(),
        &[
            ("shapes.py", "from typing import Generic\nclass Shape(Generic[T]):\n    def area(self): pass\n    @classmethod\n    def unit(cls): return cls()\nclass Square(Shape[int]):\n    def area(self):\n        return super().area()\n    def describe(self): return self.area()\n"),
            ("everything.py", "from shapes import *\n"),
            ("main.py", "from everything import Square\nfrom shapes import *\nimport json\n\ndef run():\n    Square.unit(); Shape(); json.dumps(1)\n"),
            ("json.py", "def dumps(x): pass\n"),
        ],
    );
    check(
        edge(&r, "sym:shapes.py#Square", "EXTENDS", "sym:shapes.py#Shape"),
        Confirmed,
        "scope.module-symbol",
    );
    check(
        edge(&r, "sym:shapes.py#Shape", "EXTENDS", "ext:python:typing"),
        Confirmed,
        "py.import.stdlib",
    );
    check(
        edge(
            &r,
            "sym:shapes.py#Square.area",
            "CALLS",
            "sym:shapes.py#Shape.area",
        ),
        Probable,
        "py.call.super-method",
    );
    check(
        edge(
            &r,
            "sym:shapes.py#Square.describe",
            "CALLS",
            "sym:shapes.py#Square.area",
        ),
        Probable,
        "py.call.self-method",
    );
    check(
        edge(
            &r,
            "sym:shapes.py#Shape.unit",
            "INSTANTIATES",
            "sym:shapes.py#Shape",
        ),
        Probable,
        "py.call.cls",
    );
    let run = "sym:main.py#run";
    // Inherited classmethod through a wildcard re-export: never stronger than probable.
    check(
        edge(&r, run, "CALLS", "sym:shapes.py#Shape.unit"),
        Probable,
        "py.import.wildcard",
    );
    check(
        edge(&r, run, "INSTANTIATES", "sym:shapes.py#Shape"),
        Probable,
        "py.import.wildcard",
    );
    // A top-level `json.py` next to the importer really does shadow the stdlib.
    check(
        edge(&r, run, "CALLS", "sym:json.py#dumps"),
        Confirmed,
        "py.import.absolute",
    );
}

#[test]
fn python_package_init_and_class_body() {
    let r = project(
        ProjectConfig::default(),
        &[
            ("lib/__init__.py", "from . import fmt\nfrom .fmt import *\n"),
            ("lib/fmt.py", "def render(): pass\n"),
            ("use.py", "from lib import fmt, render\nimport lib\n\ndef go():\n    fmt.render(); render(); lib.fmt.render()\n"),
            ("model.py", "def validator(f): return f\nclass M:\n    def check(f): return f\n    @property\n    def size(self): pass\n    @size.setter\n    def size(self, v): pass\n    @check\n    def a(self): pass\n    limit = validator(1)\n"),
        ],
    );
    // `from . import fmt` in the package's own `__init__` must not loop.
    let render = edge(&r, "sym:use.py#go", "CALLS", "sym:lib/fmt.py#render");
    assert_eq!(render.count, 3);
    assert_eq!(render.confidence, Confirmed);
    check(
        edge(&r, "sym:model.py#M", "REFERENCES", "sym:model.py#M.size"),
        Confirmed,
        "py.property-accessor",
    );
    check(
        edge(&r, "sym:model.py#M", "REFERENCES", "sym:model.py#M.check"),
        Confirmed,
        "scope.class-member",
    );
    edge(&r, "sym:model.py#M", "CALLS", "sym:model.py#validator");
}

#[test]
fn wide_reexport_graphs_stay_fast() {
    // 40 barrel files that all re-export each other: without memoization a
    // single lookup would explore an exponential number of paths.
    let n = 40;
    let mut files: Vec<(String, String)> = (0..n)
        .map(|i| {
            let stars: String = (0..n)
                .filter(|j| *j != i)
                .map(|j| format!("export * from './b{j}';\n"))
                .collect();
            (
                format!("b{i}.ts"),
                format!("{stars}export function f{i}() {{}}\n"),
            )
        })
        .collect();
    let calls: String = (0..n).map(|i| format!("f{i}(); ")).collect();
    let names: String = (0..n).map(|i| format!("f{i}, ")).collect();
    files.push(("main.ts".into(), format!("import {{ {names}nothing }} from './b0';\nexport function run() {{ {calls}nothing(); }}\n")));
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let started = std::time::Instant::now();
    let r = project(ProjectConfig::default(), &borrowed);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "took {:?}",
        started.elapsed()
    );
    edge(&r, "sym:main.ts#run", "CALLS", "sym:b0.ts#f0");
    assert_eq!(
        reference(&r, "main.ts", "nothing").outcome,
        Outcome::Unresolved
    );
}

#[test]
fn ambiguity_is_reported_not_guessed() {
    let r = project(
        ProjectConfig::default(),
        &[
            ("a/tool.py", "def run(): pass\n"),
            ("b/tool.py", "def run(): pass\n"),
            ("c/main.py", "import tool\ntool.run()\n"),
            ("x.ts", "export function dup() {}\n"),
            ("y.ts", "export function dup() {}\n"),
            ("z.ts", "export * from './x';\nexport * from './y';\n"),
            ("w.ts", "import { dup } from './z';\ndup();\n"),
        ],
    );
    let tool = reference(&r, "c/main.py", "tool");
    assert_eq!(tool.outcome, Outcome::Unresolved);
    assert!(tool.reason.as_ref().unwrap().contains("ambiguous"));
    assert_eq!(
        reference(&r, "c/main.py", "tool.run").outcome,
        Outcome::Unresolved
    );
    let dup = reference(&r, "w.ts", "dup");
    assert_eq!(
        (dup.outcome, dup.rule.as_str()),
        (Outcome::Unresolved, "es.export.wildcard")
    );
}

#[test]
fn cycles_terminate() {
    let r = project(
        ProjectConfig::default(),
        &[
            ("a.ts", "export * from './b';\nexport { x } from './b';\nimport { y } from './b';\ny();\n"),
            ("b.ts", "export * from './a';\nexport { x } from './a';\nexport { y } from './a';\n"),
            ("p.py", "from q import *\nfrom q import thing\nthing()\nclass A(B): pass\n"),
            ("q.py", "from p import *\nfrom p import thing, A\nclass B(A):\n    def m(self): self.zzz()\n"),
        ],
    );
    assert_eq!(reference(&r, "a.ts", "y").outcome, Outcome::Unresolved);
    assert_eq!(reference(&r, "p.py", "thing").outcome, Outcome::Unresolved);
    assert_eq!(
        reference(&r, "q.py", "self.zzz").outcome,
        Outcome::Unresolved
    );
}

#[test]
fn output_is_deterministic_and_keys_are_unique() {
    let files = [
        (
            "m.py",
            "def f(): pass\ndef f(): pass\nclass C:\n    def f(self): f()\n",
        ),
        (
            "n.ts",
            "export function g(): void;\nexport function g(a?: number): void {}\ng();\n",
        ),
    ];
    let a = project(ProjectConfig::default(), &files);
    let b = project(ProjectConfig::default(), &files);
    // Timing fields live in the IR, not in the resolution, so equality is exact.
    assert_eq!(a, b);
    let mut keys: Vec<&str> = a.nodes.iter().map(|n| n.key.as_str()).collect();
    assert!(keys.contains(&"sym:m.py#f") && keys.contains(&"sym:m.py#f~2"));
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), total);
    // Two definitions of `f`: the call is attributed, but not as confirmed.
    check(
        edge(&a, "sym:m.py#C.f", "CALLS", "sym:m.py#f"),
        Probable,
        "scope.module-symbol",
    );
    let node_keys: std::collections::HashSet<&str> =
        a.nodes.iter().map(|n| n.key.as_str()).collect();
    for e in &a.edges {
        assert!(
            e.src.starts_with("file:") || node_keys.contains(e.src.as_str()),
            "dangling src {}",
            e.src
        );
        assert!(
            e.dst.starts_with("file:") || node_keys.contains(e.dst.as_str()),
            "dangling dst {}",
            e.dst
        );
    }
}
