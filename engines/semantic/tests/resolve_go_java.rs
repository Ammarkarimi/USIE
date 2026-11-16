//! End-to-end resolution tests for Go and Java: real source is parsed, then resolved.

use usie_ir::{Confidence, Language};
use usie_parser::{parse, ParseOptions};
use usie_semantic::{
    resolve, DeclaredDependency, EdgeRecord, GoModule, Outcome, Project, ProjectConfig,
    ReferenceRecord, Resolution, SourceFile,
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

use Confidence::{Confirmed, Probable};

fn dep(ecosystem: &str, name: &str) -> DeclaredDependency {
    DeclaredDependency {
        ecosystem: ecosystem.into(),
        name: name.into(),
    }
}

fn go_project() -> Resolution {
    project(
        ProjectConfig {
            go_modules: vec![GoModule {
                dir: String::new(),
                path: "example.com/shop".into(),
            }],
            dependencies: vec![dep("go", "github.com/go-chi/chi/v5")],
            ..ProjectConfig::default()
        },
        &[
            (
                "main.go",
                r#"package main

import (
	"fmt"

	"example.com/shop/store"
	"github.com/acme/util"
	"github.com/go-chi/chi/v5"
	"golang.org/x/sync/errgroup"
)

func main() {
	var st *store.Store = store.NewStore()
	st.Save(store.Item{Name: "x"})
	s := store.NewStore()
	s.Unknown()
	fmt.Println("ok")
	chi.NewRouter()
	var g errgroup.Group
	g.Go(nil)
	util.Do()
	run()
	store.hidden()
	len("x")
}

func run() {}

func viaResult() {
	a, err := store.Load()
	a.Save(store.Item{})
	_ = err
	b := fmt.Sprint(1)
	b.Len()
}
"#,
            ),
            (
                "store/store.go",
                r#"package store

// Store keeps items.
type Store struct {
	db *DB
	Logger
}

func NewStore() *Store { return &Store{} }

func (s *Store) Save(it Item) error {
	s.db.Exec()
	s.Log()
	platform()
	return s.validate(it)
}

func hidden() {}

func Load() (*Store, error) { return NewStore(), nil }
"#,
            ),
            (
                "store/item.go",
                r#"package store

type Item struct{ Name string }

type DB struct{}

func (d *DB) Exec() {}

func (s *Store) validate(it Item) error { return nil }
"#,
            ),
            (
                "store/log.go",
                r#"package store

type Logger struct{}

func (l Logger) Log() {}

type Saver interface {
	Save(Item) error
}

func use(sv Saver) { sv.Save(Item{}) }

type Celsius float64

func convert() { Celsius(1) }
"#,
            ),
            ("store/os_linux.go", "package store\n\nfunc platform() string { return \"linux\" }\n"),
            ("store/os_windows.go", "package store\n\nfunc platform() string { return \"windows\" }\n"),
            (
                "store/store_test.go",
                "package store_test\n\nimport \"example.com/shop/store\"\n\nfunc TestNew() { store.NewStore() }\n",
            ),
            (
                "vendor/github.com/acme/util/util.go",
                "package util\n\nfunc Do() {}\n",
            ),
        ],
    )
}

#[test]
fn go_imports_resolve_to_packages_modules_and_stdlib() {
    let r = go_project();
    check(
        edge(&r, "file:main.go", "IMPORTS", "dir:store"),
        Confirmed,
        "go.import.module",
    );
    check(
        edge(&r, "file:main.go", "IMPORTS", "ext:go:fmt"),
        Confirmed,
        "go.import.stdlib",
    );
    check(
        edge(
            &r,
            "file:main.go",
            "IMPORTS",
            "ext:go:github.com/go-chi/chi/v5",
        ),
        Confirmed,
        "go.import.declared-module",
    );
    check(
        edge(&r, "file:main.go", "IMPORTS", "ext:go:golang.org/x/sync"),
        Probable,
        "go.import.undeclared-module",
    );
    check(
        edge(
            &r,
            "file:main.go",
            "IMPORTS",
            "dir:vendor/github.com/acme/util",
        ),
        Confirmed,
        "go.import.vendor",
    );
    // An external test package imports the package under test.
    check(
        edge(&r, "file:store/store_test.go", "IMPORTS", "dir:store"),
        Confirmed,
        "go.import.module",
    );
}

#[test]
fn go_calls_use_packages_receivers_and_declared_types() {
    let r = go_project();
    let main = "sym:main.go#main";
    check(
        edge(&r, main, "CALLS", "sym:store/store.go#NewStore"),
        Confirmed,
        "go.import.module",
    );
    check(
        edge(&r, main, "CALLS", "sym:main.go#run"),
        Confirmed,
        "go.package-symbol",
    );
    // `var st *store.Store`: the declared type gives the method.
    check(
        edge(&r, main, "CALLS", "sym:store/store.go#Store.Save"),
        Confirmed,
        "go.typed-value",
    );
    check(
        edge(&r, main, "INSTANTIATES", "sym:store/item.go#Item"),
        Confirmed,
        "go.import.module",
    );
    check(
        edge(&r, main, "CALLS", "ext:go:fmt"),
        Confirmed,
        "go.import.stdlib",
    );
    check(
        edge(
            &r,
            main,
            "CALLS",
            "sym:vendor/github.com/acme/util/util.go#Do",
        ),
        Confirmed,
        "go.import.vendor",
    );
    // `var g errgroup.Group`: a method of an external type stays with its module.
    check(
        edge(&r, main, "CALLS", "ext:go:golang.org/x/sync"),
        Probable,
        "go.typed-value",
    );

    let save = "sym:store/store.go#Store.Save";
    // Receiver → field of declared type → method declared in another file.
    check(
        edge(&r, save, "CALLS", "sym:store/item.go#DB.Exec"),
        Confirmed,
        "go.typed-value",
    );
    check(
        edge(&r, save, "CALLS", "sym:store/item.go#Store.validate"),
        Confirmed,
        "go.typed-value",
    );
    // Promoted from the embedded Logger.
    check(
        edge(&r, save, "CALLS", "sym:store/log.go#Logger.Log"),
        Probable,
        "go.typed-value",
    );
    // Declared in two build-constrained files.
    check(
        edge(&r, save, "CALLS", "sym:store/os_linux.go#platform"),
        Probable,
        "go.package-symbol.build-variant",
    );
    check(
        edge(
            &r,
            "sym:store/store.go#Store",
            "EXTENDS",
            "sym:store/log.go#Logger",
        ),
        Confirmed,
        "go.package-symbol",
    );
    check(
        edge(
            &r,
            "sym:store/store.go#NewStore",
            "INSTANTIATES",
            "sym:store/store.go#Store",
        ),
        Confirmed,
        "go.package-symbol",
    );
    // Interface calls are dynamically dispatched.
    check(
        edge(
            &r,
            "sym:store/log.go#use",
            "CALLS",
            "sym:store/log.go#Saver.Save",
        ),
        Probable,
        "go.typed-value",
    );
    // Conversions are references, not calls.
    check(
        edge(
            &r,
            "sym:store/log.go#convert",
            "REFERENCES",
            "sym:store/log.go#Celsius",
        ),
        Confirmed,
        "go.package-symbol",
    );
    check(
        edge(
            &r,
            "sym:store/store_test.go#TestNew",
            "CALLS",
            "sym:store/store.go#NewStore",
        ),
        Confirmed,
        "go.import.module",
    );
}

#[test]
fn go_reports_what_it_cannot_resolve() {
    let r = go_project();
    let unknown = reference(&r, "main.go", "s.Unknown");
    assert_eq!(unknown.outcome, Outcome::Unresolved);
    assert!(
        unknown
            .reason
            .as_deref()
            .unwrap()
            .contains("has no member `Unknown`"),
        "{unknown:?}"
    );
    let hidden = reference(&r, "main.go", "store.hidden");
    assert_eq!(hidden.outcome, Outcome::Unresolved);
    assert!(
        hidden.reason.as_deref().unwrap().contains("not exported"),
        "{hidden:?}"
    );
    assert_eq!(reference(&r, "main.go", "len").outcome, Outcome::Builtin);
    let external = reference(&r, "main.go", "b.Len");
    assert!(
        external
            .reason
            .as_deref()
            .unwrap()
            .contains("outside the repository"),
        "{external:?}"
    );
    // Methods are not package-level names.
    assert!(r
        .nodes
        .iter()
        .any(|n| n.key == "sym:store/store.go#Store.Save" && n.kind == "method" && n.exported));
    assert!(r
        .nodes
        .iter()
        .any(|n| n.key == "sym:store/store.go#hidden" && !n.exported));
}

fn java_project() -> Resolution {
    project(
        ProjectConfig {
            dependencies: vec![dep("maven", "org.springframework.boot:spring-boot-starter-web")],
            ..ProjectConfig::default()
        },
        &[
            (
                "src/main/java/com/acme/app/App.java",
                r#"package com.acme.app;

import com.acme.app.service.UserService;
import com.acme.app.model.*;
import java.util.List;
import org.springframework.boot.SpringApplication;
import static com.acme.app.util.Strings.trim;

/** Entry point. */
public class App {
    public static void main(String[] args) {
        SpringApplication.run(App.class, args);
        UserService svc = new UserService(new Repo());
        svc.register("x");
        List<User> users = svc.all();
        users.size();
        trim(" a ");
        helper();
        Config.load();
        com.acme.app.util.Strings.trim("b");
        String s = "x";
        s.length();
        var v = new UserService(null);
        v.all();
        undefinedThing();
    }

    static void helper() {}
}
"#,
            ),
            (
                "src/main/java/com/acme/app/service/UserService.java",
                r#"package com.acme.app.service;

import com.acme.app.Repo;
import com.acme.app.model.User;
import java.util.List;

public class UserService extends BaseService {
    private final Repo repo;

    public UserService(Repo repo) {
        super();
        this.repo = repo;
    }

    public void register(String name) {
        repo.save(new User(name));
        this.audit();
        validate(name);
    }

    private void audit() {}

    public List<User> all() { return repo.findAll(); }
}
"#,
            ),
            (
                "src/main/java/com/acme/app/service/BaseService.java",
                "package com.acme.app.service;\n\npublic abstract class BaseService {\n    protected void validate(String s) {}\n}\n",
            ),
            (
                "src/main/java/com/acme/app/Repo.java",
                "package com.acme.app;\n\nimport com.acme.app.model.User;\nimport java.util.List;\n\npublic class Repo implements Store<User> {\n    public void save(User u) {}\n    public List<User> findAll() { return null; }\n}\n",
            ),
            (
                "src/main/java/com/acme/app/Store.java",
                "package com.acme.app;\n\npublic interface Store<T> {\n    void save(T item);\n}\n",
            ),
            (
                "src/main/java/com/acme/app/Config.java",
                "package com.acme.app;\n\nclass Config {\n    static void load() {}\n}\n",
            ),
            (
                "src/main/java/com/acme/app/model/User.java",
                "package com.acme.app.model;\n\npublic record User(String name) {}\n",
            ),
            (
                "src/main/java/com/acme/app/util/Strings.java",
                "package com.acme.app.util;\n\npublic final class Strings {\n    public static String trim(String s) { return s.trim(); }\n}\n",
            ),
            (
                "src/main/java/com/acme/app/web/Controller.java",
                r#"package com.acme.app.web;

import org.springframework.web.servlet.mvc.AbstractController;

@Deprecated
public class Controller extends AbstractController {
    void handle() { getServletContext(); }
}
"#,
            ),
        ],
    )
}

#[test]
fn java_imports_resolve_types_members_and_packages() {
    let r = java_project();
    let app = "file:src/main/java/com/acme/app/App.java";
    check(
        edge(
            &r,
            app,
            "IMPORTS",
            "sym:src/main/java/com/acme/app/service/UserService.java#UserService",
        ),
        Confirmed,
        "java.import.type",
    );
    check(
        edge(&r, app, "IMPORTS", "dir:src/main/java/com/acme/app/model"),
        Confirmed,
        "java.import.on-demand",
    );
    check(
        edge(&r, app, "IMPORTS", "ext:jdk:java.util"),
        Confirmed,
        "java.import.jdk",
    );
    check(
        edge(&r, app, "IMPORTS", "ext:maven:org.springframework.boot"),
        Probable,
        "java.import.declared-dependency",
    );
    check(
        edge(
            &r,
            app,
            "IMPORTS",
            "sym:src/main/java/com/acme/app/util/Strings.java#Strings.trim",
        ),
        Confirmed,
        "java.import.static-member",
    );
    check(
        edge(
            &r,
            "file:src/main/java/com/acme/app/web/Controller.java",
            "IMPORTS",
            "ext:maven:org.springframework.web",
        ),
        Probable,
        "java.import.undeclared-package",
    );
}

#[test]
fn java_calls_follow_scopes_declared_types_and_inheritance() {
    let r = java_project();
    let p = "src/main/java/com/acme/app/";
    let main = format!("sym:{p}App.java#App.main");
    let svc = format!("sym:{p}service/UserService.java#UserService");
    check(
        edge(&r, &main, "INSTANTIATES", &svc),
        Confirmed,
        "java.import.type",
    );
    check(
        edge(&r, &main, "INSTANTIATES", &format!("sym:{p}Repo.java#Repo")),
        Confirmed,
        "java.scope.package-type",
    );
    // Instance methods may be overridden: probable.
    check(
        edge(&r, &main, "CALLS", &format!("{svc}.register")),
        Probable,
        "java.typed-value",
    );
    check(
        edge(&r, &main, "CALLS", &format!("{svc}.all")),
        Probable,
        "java.typed-value",
    );
    check(
        edge(
            &r,
            &main,
            "CALLS",
            &format!("sym:{p}util/Strings.java#Strings.trim"),
        ),
        Confirmed,
        "java.import.static-member",
    );
    check(
        edge(&r, &main, "CALLS", &format!("sym:{p}App.java#App.helper")),
        Confirmed,
        "java.scope.member",
    );
    check(
        edge(
            &r,
            &main,
            "CALLS",
            &format!("sym:{p}Config.java#Config.load"),
        ),
        Confirmed,
        "java.scope.package-type",
    );
    check(
        edge(&r, &main, "CALLS", "ext:maven:org.springframework.boot"),
        Probable,
        "java.import.declared-dependency",
    );
    check(
        edge(&r, &main, "CALLS", "ext:jdk:java.util"),
        Confirmed,
        "java.typed-value",
    );

    let register = format!("{svc}.register");
    check(
        edge(
            &r,
            &register,
            "CALLS",
            &format!("sym:{p}Repo.java#Repo.save"),
        ),
        Probable,
        "java.typed-value",
    );
    check(
        edge(&r, &register, "CALLS", &format!("{svc}.audit")),
        Confirmed,
        "java.call.this-member",
    );
    check(
        edge(
            &r,
            &register,
            "CALLS",
            &format!("sym:{p}service/BaseService.java#BaseService.validate"),
        ),
        Probable,
        "java.scope.member",
    );
    check(
        edge(
            &r,
            &register,
            "INSTANTIATES",
            &format!("sym:{p}model/User.java#User"),
        ),
        Confirmed,
        "java.import.type",
    );
    check(
        edge(
            &r,
            &format!("{svc}.UserService"),
            "CALLS",
            &format!("sym:{p}service/BaseService.java#BaseService"),
        ),
        Confirmed,
        "java.call.super-constructor",
    );
    check(
        edge(
            &r,
            &svc,
            "EXTENDS",
            &format!("sym:{p}service/BaseService.java#BaseService"),
        ),
        Confirmed,
        "java.scope.package-type",
    );
    check(
        edge(
            &r,
            &format!("sym:{p}Repo.java#Repo"),
            "IMPLEMENTS",
            &format!("sym:{p}Store.java#Store"),
        ),
        Confirmed,
        "java.scope.package-type",
    );

    let handle = format!("sym:{p}web/Controller.java#Controller.handle");
    check(
        edge(&r, &handle, "CALLS", "ext:maven:org.springframework.web"),
        Probable,
        "java.member.inherited-external",
    );
}

#[test]
fn java_reports_builtins_and_unresolved_names() {
    let r = java_project();
    let path = "src/main/java/com/acme/app/App.java";
    assert_eq!(reference(&r, path, "s.length()").outcome, Outcome::Builtin);
    let missing = reference(&r, path, "undefinedThing()");
    assert_eq!(missing.outcome, Outcome::Unresolved);
    assert!(missing.reason.is_some());
    // `var v = new UserService(null)` infers the type from the constructor.
    let v = reference(&r, path, "v.all()");
    assert_eq!(v.outcome, Outcome::Resolved);
    let fq = reference(&r, path, "com.acme.app.util.Strings.trim(\"b\")");
    assert_eq!(fq.rule, "java.scope.qualified-name");
    no_edge(
        &r,
        "sym:src/main/java/com/acme/app/Config.java#Config",
        "EXTENDS",
    );
    let app = r
        .nodes
        .iter()
        .find(|n| n.key.ends_with("App.java#App"))
        .unwrap();
    assert!(app.exported);
    assert_eq!(app.doc.as_deref(), Some("Entry point."));
}
