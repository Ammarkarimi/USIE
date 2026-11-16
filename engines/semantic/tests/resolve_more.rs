//! End-to-end resolution tests for C#, Kotlin, Rust, C and C++: real source is parsed, then resolved.

use usie_ir::{Confidence, Language};
use usie_parser::{parse, ParseOptions};
use usie_semantic::{
    resolve, DeclaredDependency, EdgeRecord, Outcome, Project, ProjectConfig, ReferenceRecord,
    Resolution, RustCrate, SourceFile,
};

fn language(path: &str) -> Option<Language> {
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "py" => Language::Python,
        "js" | "jsx" | "mjs" | "cjs" => Language::Javascript,
        "ts" => Language::Typescript,
        "tsx" => Language::Tsx,
        "go" => Language::Go,
        "java" => Language::Java,
        "cs" => Language::Csharp,
        "kt" => Language::Kotlin,
        "rs" => Language::Rust,
        "c" => Language::C,
        "h" => Language::C,
        "cpp" | "cc" | "hpp" => Language::Cpp,
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

fn dep(ecosystem: &str, name: &str) -> DeclaredDependency {
    DeclaredDependency {
        ecosystem: ecosystem.into(),
        name: name.into(),
    }
}

#[test]
fn kotlin_packages_objects_and_final_by_default() {
    let r = project(
        ProjectConfig {
            dependencies: vec![dep("maven", "org.slf4j:slf4j-api")],
            ..ProjectConfig::default()
        },
        &[
            (
                "app/src/main/kotlin/com/acme/App.kt",
                r#"package com.acme

import com.acme.store.Repo
import com.acme.store.Registry
import com.acme.util.*
import org.slf4j.LoggerFactory

fun main() {
    val repo = Repo()
    repo.save("a")
    val found: Repo = Registry.find()
    found.save("b")
    normalize("x")
    helper()
    LoggerFactory.getLogger("x")
    println("done")
    listOf(1).map { it + 1 }
}

private fun helper() {}
"#,
            ),
            (
                "app/src/main/kotlin/com/acme/store/Repo.kt",
                r#"package com.acme.store

open class Base {
    open fun audit() {}
}

class Repo : Base(), Saver {
    private val cache = Cache()
    fun save(name: String) {
        cache.put(name)
        audit()
        this.audit()
    }
    companion object {
        fun create(): Repo = Repo()
    }
}

interface Saver {
    fun save(name: String)
}

class Cache {
    fun put(key: String) {}
}

object Registry {
    fun find(): Repo = Repo.create()
}
"#,
            ),
            (
                "app/src/main/kotlin/com/acme/util/Strings.kt",
                "package com.acme.util\n\nfun normalize(s: String): String = s.trim()\n",
            ),
        ],
    );
    let app = "sym:app/src/main/kotlin/com/acme/App.kt#main";
    let repo = "sym:app/src/main/kotlin/com/acme/store/Repo.kt#";
    check(
        edge(&r, app, "INSTANTIATES", &format!("{repo}Repo")),
        Confirmed,
        "kotlin.import.type",
    );
    // Kotlin methods are final unless `open`: confirmed.
    check(
        edge(&r, app, "CALLS", &format!("{repo}Repo.save")),
        Confirmed,
        "kotlin.call-result",
    );
    check(
        edge(&r, app, "CALLS", &format!("{repo}Registry.find")),
        Confirmed,
        "kotlin.import.type",
    );
    check(
        edge(
            &r,
            app,
            "CALLS",
            "sym:app/src/main/kotlin/com/acme/util/Strings.kt#normalize",
        ),
        Confirmed,
        "kotlin.import.on-demand",
    );
    check(
        edge(
            &r,
            app,
            "CALLS",
            "sym:app/src/main/kotlin/com/acme/App.kt#helper",
        ),
        Confirmed,
        "kotlin.scope.file-type",
    );
    check(
        edge(&r, app, "CALLS", "ext:maven:org.slf4j"),
        Probable,
        "kotlin.import.declared-dependency",
    );
    assert_eq!(
        reference(&r, "app/src/main/kotlin/com/acme/App.kt", "println").outcome,
        Outcome::Builtin
    );

    let save = format!("{repo}Repo.save");
    // `private val cache = Cache()`: the initializer gives the field's type.
    check(
        edge(&r, &save, "CALLS", &format!("{repo}Cache.put")),
        Confirmed,
        "kotlin.call-result",
    );
    // An `open` method may be overridden: probable.
    check(
        edge(&r, &save, "CALLS", &format!("{repo}Base.audit")),
        Probable,
        "kotlin.scope.member",
    );
    check(
        edge(
            &r,
            &format!("{repo}Repo"),
            "EXTENDS",
            &format!("{repo}Base"),
        ),
        Confirmed,
        "kotlin.scope.file-type",
    );
    check(
        edge(
            &r,
            &format!("{repo}Repo"),
            "IMPLEMENTS",
            &format!("{repo}Saver"),
        ),
        Confirmed,
        "kotlin.scope.file-type",
    );
    // Companion members are static members of the class.
    check(
        edge(
            &r,
            &format!("{repo}Registry.find"),
            "CALLS",
            &format!("{repo}Repo.create"),
        ),
        Confirmed,
        "kotlin.scope.file-type",
    );
}

#[test]
fn csharp_namespaces_usings_and_virtual_dispatch() {
    let r = project(
        ProjectConfig {
            dependencies: vec![dep("nuget", "Newtonsoft.Json")],
            ..ProjectConfig::default()
        },
        &[
            (
                "src/App/Program.cs",
                r#"using System;
using Acme.Store;
using Newtonsoft.Json;
using J = Newtonsoft.Json.JsonConvert;

namespace Acme.App
{
    public class Program
    {
        public static void Main(string[] args)
        {
            var repo = new Repo(new Db());
            repo.Save("a");
            IRepo r = repo;
            r.Save("b");
            var made = Factory.Make();
            made.Save("c");
            Console.WriteLine("x");
            J.SerializeObject(repo);
            Helpers.Log();
        }
    }
}
"#,
            ),
            (
                "src/App/Helpers.cs",
                "namespace Acme;\n\npublic static class Helpers { public static void Log() {} }\n",
            ),
            (
                "src/Store/Repo.cs",
                r#"using System.Collections.Generic;

namespace Acme.Store
{
    /// <summary>Stores things.</summary>
    [Serializable]
    public class Repo : BaseRepo, IRepo
    {
        private readonly Db _db;
        public Repo(Db db) : base(db) { _db = db; }
        public void Save(string name)
        {
            _db.Execute(name);
            this.Audit();
            base.Track();
            var list = new List<string>();
            list.Add(name);
        }
        public virtual void Audit() {}
    }

    public abstract class BaseRepo
    {
        protected BaseRepo(Db db) {}
        protected void Track() {}
    }

    public interface IRepo { void Save(string name); }

    public class Db { public void Execute(string sql) {} }

    public static class Factory { public static Repo Make() => new Repo(new Db()); }

    public class SerializableAttribute : System.Attribute {}
}
"#,
            ),
        ],
    );
    let main = "sym:src/App/Program.cs#Acme.App.Program.Main";
    let store = "sym:src/Store/Repo.cs#Acme.Store.";
    check(
        edge(&r, main, "INSTANTIATES", &format!("{store}Repo")),
        Confirmed,
        "csharp.import.on-demand",
    );
    // C# methods are non-virtual unless declared virtual: confirmed.
    check(
        edge(&r, main, "CALLS", &format!("{store}Repo.Save")),
        Confirmed,
        "csharp.typed-value",
    );
    // Through an interface: probable.
    check(
        edge(&r, main, "CALLS", &format!("{store}IRepo.Save")),
        Probable,
        "csharp.typed-value",
    );
    check(
        edge(&r, main, "CALLS", &format!("{store}Factory.Make")),
        Confirmed,
        "csharp.import.on-demand",
    );
    // An enclosing namespace's types are visible (Acme.App → Acme).
    check(
        edge(&r, main, "CALLS", "sym:src/App/Helpers.cs#Helpers.Log"),
        Confirmed,
        "csharp.scope.package-type",
    );
    check(
        edge(&r, main, "CALLS", "ext:dotnet:System"),
        Probable,
        "csharp.import.dotnet-system-type",
    );
    check(
        edge(&r, main, "CALLS", "ext:nuget:Newtonsoft.Json"),
        Probable,
        "csharp.import.declared-dependency",
    );

    let save = format!("{store}Repo.Save");
    check(
        edge(&r, &save, "CALLS", &format!("{store}Db.Execute")),
        Confirmed,
        "csharp.typed-value",
    );
    check(
        edge(&r, &save, "CALLS", &format!("{store}Repo.Audit")),
        Probable,
        "csharp.call.this-member",
    );
    check(
        edge(&r, &save, "CALLS", &format!("{store}BaseRepo.Track")),
        Confirmed,
        "csharp.call.super-method",
    );
    check(
        edge(
            &r,
            &format!("{store}Repo.Repo"),
            "CALLS",
            &format!("{store}BaseRepo"),
        ),
        Confirmed,
        "csharp.call.super-constructor",
    );
    check(
        edge(
            &r,
            &format!("{store}Repo"),
            "EXTENDS",
            &format!("{store}BaseRepo"),
        ),
        Confirmed,
        "csharp.scope.package-type",
    );
    check(
        edge(
            &r,
            &format!("{store}Repo"),
            "IMPLEMENTS",
            &format!("{store}IRepo"),
        ),
        Confirmed,
        "csharp.scope.package-type",
    );
    // `[Serializable]` names `SerializableAttribute`; attributes, like Java
    // annotations, are attributed to the scope enclosing the declaration.
    check(
        edge(
            &r,
            "sym:src/Store/Repo.cs#Acme.Store",
            "REFERENCES",
            &format!("{store}SerializableAttribute"),
        ),
        Confirmed,
        "csharp.scope.package-type",
    );
    let repo = r
        .nodes
        .iter()
        .find(|n| n.key == format!("{store}Repo"))
        .unwrap();
    assert_eq!(repo.doc.as_deref(), Some("Stores things."));
}

#[test]
fn rust_modules_uses_impls_and_traits() {
    let r = project(
        ProjectConfig {
            rust_crates: vec![
                RustCrate { dir: "core".into(), name: "shop-core".into() },
                RustCrate { dir: "app".into(), name: "app".into() },
            ],
            dependencies: vec![dep("cargo", "serde")],
            ..ProjectConfig::default()
        },
        &[
            (
                "core/src/lib.rs",
                "pub mod store;\npub mod util;\npub use store::Repo;\n",
            ),
            (
                "core/src/store.rs",
                r#"use crate::util::{normalize, Saver};
use std::collections::HashMap;

/// Keeps items.
pub struct Repo {
    items: HashMap<String, u32>,
    log: Log,
}

pub struct Log;

impl Log {
    pub fn write(&self, _m: &str) {}
}

impl Repo {
    pub fn new() -> Self {
        Repo { items: HashMap::new(), log: Log }
    }
    pub fn add(&mut self, name: &str) {
        let key = normalize(name);
        self.log.write(&key);
        self.save();
        Self::helper();
    }
    fn helper() {}
}

impl Saver for Repo {
    fn save(&self) {}
}
"#,
            ),
            (
                "core/src/util.rs",
                "pub fn normalize(s: &str) -> String { s.trim().to_string() }\n\npub trait Saver {\n    fn save(&self);\n    fn save_all(&self) { self.save(); }\n}\n\nmod inner { pub fn deep() {} }\npub fn call_inner() { inner::deep(); self::inner::deep(); super::util::normalize(\"x\"); }\n",
            ),
            (
                "app/src/main.rs",
                r#"use shop_core::Repo;
use shop_core::util::Saver;
use serde::Serialize;

fn main() {
    let mut repo = Repo::new();
    repo.add("x");
    repo.save_all();
    let other: Repo = make();
    other.save();
    println!("done");
    Vec::<u32>::new();
    missing();
}

fn make() -> Repo { Repo::new() }
"#,
            ),
        ],
    );
    let store = "sym:core/src/store.rs#";
    let main = "sym:app/src/main.rs#main";
    check(
        edge(
            &r,
            "file:app/src/main.rs",
            "IMPORTS",
            &format!("{store}Repo"),
        ),
        Confirmed,
        "rust.path.workspace-crate",
    );
    check(
        edge(&r, "file:app/src/main.rs", "IMPORTS", "ext:cargo:serde"),
        Confirmed,
        "rust.path.declared-crate",
    );
    check(
        edge(&r, main, "CALLS", &format!("{store}Repo.new")),
        Confirmed,
        "rust.path.workspace-crate",
    );
    check(
        edge(&r, main, "CALLS", &format!("{store}Repo.add")),
        Confirmed,
        "rust.call-result",
    );
    // A provided trait method, through `impl Saver for Repo`.
    check(
        edge(&r, main, "CALLS", "sym:core/src/util.rs#Saver.save_all"),
        Probable,
        "rust.call-result",
    );
    check(
        edge(&r, main, "CALLS", &format!("{store}Repo.save")),
        Confirmed,
        "rust.typed-value",
    );

    let add = format!("{store}Repo.add");
    check(
        edge(&r, &add, "CALLS", "sym:core/src/util.rs#normalize"),
        Confirmed,
        "rust.path.crate",
    );
    check(
        edge(&r, &add, "CALLS", &format!("{store}Log.write")),
        Confirmed,
        "rust.typed-value",
    );
    check(
        edge(&r, &add, "CALLS", &format!("{store}Repo.save")),
        Confirmed,
        "rust.typed-value",
    );
    check(
        edge(&r, &add, "CALLS", &format!("{store}Repo.helper")),
        Confirmed,
        "rust.path.item",
    );
    check(
        edge(
            &r,
            &format!("{store}Repo.new"),
            "INSTANTIATES",
            &format!("{store}Repo"),
        ),
        Confirmed,
        "rust.path.item",
    );
    check(
        edge(
            &r,
            &format!("{store}Repo"),
            "IMPLEMENTS",
            "sym:core/src/util.rs#Saver",
        ),
        Confirmed,
        "rust.impl.trait",
    );
    check(
        edge(&r, "file:core/src/store.rs", "IMPORTS", "ext:rust:std"),
        Confirmed,
        "rust.path.std",
    );

    let inner = "sym:core/src/util.rs#call_inner";
    check(
        edge(&r, inner, "CALLS", "sym:core/src/util.rs#inner.deep"),
        Confirmed,
        "rust.path.item",
    );
    assert_eq!(
        reference(&r, "app/src/main.rs", "Vec::<u32>::new").outcome,
        Outcome::Builtin
    );
    assert_eq!(
        reference(&r, "app/src/main.rs", "missing").outcome,
        Outcome::Unresolved
    );
    let repo = r
        .nodes
        .iter()
        .find(|n| n.key == format!("{store}Repo"))
        .unwrap();
    assert_eq!(repo.doc.as_deref(), Some("Keeps items."));
}

#[test]
fn c_includes_declarations_and_definitions() {
    let r = project(
        ProjectConfig {
            include_dirs: vec!["include".into()],
            ..ProjectConfig::default()
        },
        &[
            (
                "include/store/store.h",
                "#ifndef STORE_H\n#define STORE_H\n#include \"item.h\"\n/** A store. */\nstruct store { int count; struct item *first; };\nstruct store *store_new(void);\nvoid store_add(struct store *s, struct item *it);\n#define STORE_MAX(a, b) ((a) > (b) ? (a) : (b))\n#endif\n",
            ),
            ("include/store/item.h", "struct item { int id; };\nint item_id(const struct item *it);\n"),
            (
                "src/store.c",
                "#include <stdlib.h>\n#include \"store/store.h\"\n\nstatic void grow(struct store *s) { s->count++; }\n\nstruct store *store_new(void) { return calloc(1, sizeof(struct store)); }\n\nvoid store_add(struct store *s, struct item *it) {\n    grow(s);\n    item_id(it);\n    STORE_MAX(1, 2);\n}\n",
            ),
            ("src/item.c", "#include \"store/item.h\"\nint item_id(const struct item *it) { return it->id; }\n"),
            (
                "src/main.c",
                "#include <stdio.h>\n#include <openssl/ssl.h>\n#include \"store/store.h\"\n\nint main(void) {\n    struct store *s = store_new();\n    store_add(s, 0);\n    printf(\"%d\", s->count);\n    grow(s);\n    undeclared_helper();\n    return 0;\n}\n",
            ),
            ("src/helpers.c", "void undeclared_helper(void) {}\n"),
        ],
    );
    let main = "sym:src/main.c#main";
    check(
        edge(
            &r,
            "file:src/main.c",
            "IMPORTS",
            "file:include/store/store.h",
        ),
        Confirmed,
        "c.include.include-dir",
    );
    check(
        edge(
            &r,
            "file:include/store/store.h",
            "IMPORTS",
            "file:include/store/item.h",
        ),
        Confirmed,
        "c.include.relative",
    );
    check(
        edge(&r, "file:src/main.c", "IMPORTS", "ext:c:libc"),
        Confirmed,
        "c.include.standard",
    );
    check(
        edge(&r, "file:src/main.c", "IMPORTS", "ext:c:openssl"),
        Probable,
        "c.include.system",
    );
    // Declared in an included header, defined in another file: the definition.
    check(
        edge(&r, main, "CALLS", "sym:src/store.c#store_new"),
        Confirmed,
        "c.scope.definition",
    );
    check(
        edge(&r, main, "CALLS", "sym:src/store.c#store_add"),
        Confirmed,
        "c.scope.definition",
    );
    // Not declared anywhere `main.c` includes: a name match only.
    check(
        edge(&r, main, "CALLS", "sym:src/helpers.c#undeclared_helper"),
        Inferred,
        "heuristic.c.global-name",
    );
    // `static` functions are private to their file.
    assert_eq!(
        reference(&r, "src/main.c", "grow").outcome,
        Outcome::Unresolved
    );
    assert_eq!(
        reference(&r, "src/main.c", "printf").outcome,
        Outcome::Builtin
    );
    let add = "sym:src/store.c#store_add";
    check(
        edge(&r, add, "CALLS", "sym:src/store.c#grow"),
        Confirmed,
        "c.scope.definition",
    );
    // Through `store.h` → `item.h`, defined in `item.c`.
    check(
        edge(&r, add, "CALLS", "sym:src/item.c#item_id"),
        Confirmed,
        "c.scope.definition",
    );
    check(
        edge(&r, add, "CALLS", "sym:include/store/store.h#STORE_MAX"),
        Confirmed,
        "c.scope.definition",
    );
    let store = r
        .nodes
        .iter()
        .find(|n| n.key == "sym:include/store/store.h#store")
        .unwrap();
    assert_eq!(store.doc.as_deref(), Some("A store."));
}

#[test]
fn cpp_namespaces_classes_and_virtual_calls() {
    let r = project(
        ProjectConfig::default(),
        &[
            (
                "src/cart.hpp",
                "#pragma once\n#include <vector>\nnamespace shop {\nclass Repo { public: void save(int v); };\nclass Base { public: virtual int total() const; };\nclass Cart : public Base {\n public:\n  void add(int v);\n  int total() const override;\n private:\n  Repo repo_;\n  std::vector<int> items_;\n};\nnamespace util { void log(const char *m); }\n}\n",
            ),
            (
                "src/cart.cpp",
                "#include \"cart.hpp\"\nnamespace shop {\nvoid Repo::save(int v) {}\nint Base::total() const { return 0; }\nvoid Cart::add(int v) {\n  items_.push_back(v);\n  repo_.save(v);\n  this->total();\n  util::log(\"x\");\n  Repo *p = new Repo();\n  p->save(v);\n}\nint Cart::total() const { return 1; }\nvoid util::log(const char *m) {}\n}\n",
            ),
            (
                "src/main.cpp",
                "#include \"cart.hpp\"\nint main() {\n  shop::Cart c;\n  c.add(1);\n  shop::Base *b = &c;\n  b->total();\n  std::sort(0, 0);\n}\n",
            ),
        ],
    );
    let add = "sym:src/cart.cpp#shop.Cart.add";
    check(
        edge(&r, add, "CALLS", "sym:src/cart.cpp#shop.Repo.save"),
        Confirmed,
        "c.typed-value",
    );
    check(
        edge(&r, add, "CALLS", "ext:cpp:std"),
        Confirmed,
        "c.typed-value",
    );
    // `total` is virtual (declared `override`/`virtual` in the class).
    check(
        edge(&r, add, "CALLS", "sym:src/cart.cpp#shop.Cart.total"),
        Probable,
        "c.call.this-member",
    );
    check(
        edge(&r, add, "CALLS", "sym:src/cart.cpp#shop.util.log"),
        Confirmed,
        "c.scope.definition",
    );
    check(
        edge(&r, add, "INSTANTIATES", "sym:src/cart.hpp#shop.Repo"),
        Confirmed,
        "c.scope.definition",
    );
    let main = "sym:src/main.cpp#main";
    check(
        edge(&r, main, "CALLS", "sym:src/cart.cpp#shop.Cart.add"),
        Confirmed,
        "c.typed-value",
    );
    check(
        edge(&r, main, "CALLS", "sym:src/cart.cpp#shop.Base.total"),
        Probable,
        "c.typed-value",
    );
    check(
        edge(&r, main, "CALLS", "ext:cpp:std"),
        Confirmed,
        "c.scope.std",
    );
    check(
        edge(
            &r,
            "sym:src/cart.hpp#shop.Cart",
            "EXTENDS",
            "sym:src/cart.hpp#shop.Base",
        ),
        Confirmed,
        "c.scope.definition",
    );
}
