//! USIE semantic engine: cross-file symbol resolution.
//!
//! Takes the per-file IR of a repository version and establishes
//! relationships between symbols, files and external packages. Every
//! relationship carries a [`Confidence`] and the identifier of the rule that
//! produced it. The engine never guesses silently: what it cannot establish
//! is reported as an unresolved reference with a reason.
//!
//! The rules are specified in `docs/specifications/semantic-resolution.md`.

mod builtins;
mod cfamily;
mod golang;
mod java;
pub mod model;
mod modules;
mod paths;
mod rust;

pub use model::*;

use cfamily::CIndex;
use golang::{declared_type, is_type_kind, GoIndex};
use java::{is_classic, JavaIndex};
use modules::ModuleIndex;
use rust::RustIndex;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use usie_ir::{
    Confidence, ExportKind, FileIr, ImportKind, LanguageFamily, ReferenceKind, Symbol, SymbolKind,
};

const MAX_CHAIN: u32 = 16;

/// Resolve a whole project. Deterministic: identical input yields identical output.
pub fn resolve(project: &Project) -> Resolution {
    Resolver::new(project).run()
}

pub fn file_key(path: &str) -> String {
    format!("file:{path}")
}

/// Key of a directory node; the repository root is the `repo` node.
pub fn dir_key(dir: &str) -> String {
    if dir.is_empty() {
        "repo".to_string()
    } else {
        format!("dir:{dir}")
    }
}

pub(crate) type SymRef = (usize, u32);

/// An interned rule identifier `<family prefix>.<suffix>`.
pub(crate) fn family_rule(family: LanguageFamily, suffix: &str) -> &'static str {
    intern(format!("{}.{suffix}", family.rule_prefix()))
}

fn intern(rule: String) -> &'static str {
    // The set of rule identifiers is small and fixed by the code, so the
    // interned strings are bounded.
    static RULES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = RULES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(r) = set.get(rule.as_str()) {
        return r;
    }
    let leaked: &'static str = Box::leak(rule.into_boxed_str());
    set.insert(leaked);
    leaked
}

/// `java.*` rules applied to a Kotlin or C# file are reported as
/// `kotlin.*` / `csharp.*`.
fn localize_rule(family: LanguageFamily, rule: &'static str) -> &'static str {
    match (family, rule.strip_prefix("java.")) {
        (LanguageFamily::Kotlin | LanguageFamily::Csharp, Some(rest)) => family_rule(family, rest),
        _ => rule,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    Symbol(usize, u32),
    File(usize),
    /// A repository file that is not parsed source (stylesheet, JSON, …).
    Asset(String),
    External(String),
    /// A Go package of the repository (index into `GoIndex::packages`).
    GoPackage(usize),
    /// A Java package of the repository, by name.
    JavaPackage(String),
}

#[derive(Debug, Clone)]
pub(crate) struct Found {
    pub target: Target,
    pub confidence: Confidence,
    pub rule: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct Miss {
    pub rule: &'static str,
    pub reason: String,
    pub builtin: bool,
    /// The receiver exists but its type is unknown, so the unique-method-name
    /// heuristic may be applied.
    pub unknown_receiver: bool,
}

impl Miss {
    pub(crate) fn new(rule: &'static str, reason: impl Into<String>) -> Miss {
        Miss {
            rule,
            reason: reason.into(),
            builtin: false,
            unknown_receiver: false,
        }
    }
    pub(crate) fn receiver(rule: &'static str, reason: impl Into<String>) -> Miss {
        Miss {
            rule,
            reason: reason.into(),
            builtin: false,
            unknown_receiver: true,
        }
    }
}

pub(crate) type Res = Result<Found, Miss>;

pub(crate) struct FileIx<'a> {
    pub path: &'a str,
    pub dir: &'a str,
    pub ir: &'a FileIr,
    pub family: LanguageFamily,
    /// Module-level symbols by name, in declaration order.
    top: HashMap<&'a str, Vec<u32>>,
    children: HashMap<u32, Vec<u32>>,
    /// Import bindings by (scope, local name); the last declaration wins.
    bindings: HashMap<(Option<u32>, &'a str), usize>,
    keys: Vec<String>,
    anon_locals: HashSet<&'a str>,
    commonjs: bool,
}

struct EdgeAcc {
    confidence: Confidence,
    rule: &'static str,
    evidence: Option<Evidence>,
    count: u32,
}

pub(crate) struct Resolver<'a> {
    pub files: Vec<FileIx<'a>>,
    pub modules: ModuleIndex<'a>,
    pub(crate) go: GoIndex,
    pub(crate) java: JavaIndex,
    pub(crate) rust: RustIndex,
    pub(crate) c: CIndex,
    methods_by_name: HashMap<(LanguageFamily, &'a str), Vec<SymRef>>,
    bases: RefCell<HashMap<SymRef, Vec<SymRef>>>,
    /// Memoized module-surface lookups, keyed by (file, name). `None` marks a
    /// lookup in progress, which is how import cycles are detected.
    surface_memo: RefCell<HashMap<(usize, String), Option<Res>>>,
    /// Nesting of `call_result_value` lookups (`a := f(); b := a.g()`).
    call_depth: std::cell::Cell<u32>,
    pub externals: RefCell<BTreeMap<String, NodeRecord>>,
    edges: BTreeMap<(String, String, &'static str), EdgeAcc>,
    refs: Vec<ReferenceRecord>,
}

impl<'a> Resolver<'a> {
    fn new(project: &'a Project) -> Self {
        let mut files = Vec::with_capacity(project.files.len());
        let mut methods_by_name: HashMap<(LanguageFamily, &'a str), Vec<SymRef>> = HashMap::new();
        for (fi, f) in project.files.iter().enumerate() {
            let ir = &f.ir;
            let family = ir.language.family();
            let mut top: HashMap<&str, Vec<u32>> = HashMap::new();
            let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
            let mut keys = Vec::with_capacity(ir.symbols.len());
            let mut seen: HashMap<&str, u32> = HashMap::new();
            for sym in &ir.symbols {
                match sym.parent {
                    None => top.entry(sym.name.as_str()).or_default().push(sym.id),
                    Some(p) => children.entry(p).or_default().push(sym.id),
                }
                let n = seen.entry(sym.qualified_name.as_str()).or_insert(0);
                *n += 1;
                keys.push(if *n == 1 {
                    format!("sym:{}#{}", f.path, sym.qualified_name)
                } else {
                    format!("sym:{}#{}~{}", f.path, sym.qualified_name, n)
                });
                if sym.kind == SymbolKind::Method {
                    methods_by_name
                        .entry((family, sym.name.as_str()))
                        .or_default()
                        .push((fi, sym.id));
                }
            }
            let mut bindings = HashMap::new();
            for (i, imp) in ir.imports.iter().enumerate() {
                if let Some(local) = &imp.local {
                    bindings.insert((imp.scope, local.as_str()), i);
                }
            }
            let anon_locals = ir
                .attrs
                .get("anonymous_locals")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            files.push(FileIx {
                path: f.path.as_str(),
                dir: paths::dir_of(&f.path),
                ir,
                family,
                top,
                children,
                bindings,
                keys,
                anon_locals,
                commonjs: ir
                    .attrs
                    .get("commonjs")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            });
        }
        let modules = ModuleIndex::new(&project.config, &files);
        let go = GoIndex::new(&project.config, &files);
        let java = JavaIndex::new(&project.config, &files);
        let rust = RustIndex::new(&project.config, &files);
        let c = CIndex::new(&project.config, &files);
        let mut resolver = Resolver {
            files,
            modules,
            go,
            java,
            rust,
            c,
            methods_by_name,
            bases: RefCell::new(HashMap::new()),
            surface_memo: RefCell::new(HashMap::new()),
            call_depth: std::cell::Cell::new(0),
            externals: RefCell::new(BTreeMap::new()),
            edges: BTreeMap::new(),
            refs: Vec::new(),
        };
        resolver.go_bind_imports();
        resolver.rust_bind_impls();
        resolver.c_bind_methods();
        resolver
    }

    fn run(mut self) -> Resolution {
        let mut nodes = Vec::new();
        for fi in 0..self.files.len() {
            self.structure(fi, &mut nodes);
        }
        for fi in 0..self.files.len() {
            self.heritage(fi);
        }
        self.rust_impl_edges();
        for fi in 0..self.files.len() {
            self.imports(fi);
        }
        let mut exported: HashSet<String> = HashSet::new();
        for fi in 0..self.files.len() {
            self.exports(fi, &mut exported);
        }
        for fi in 0..self.files.len() {
            self.references(fi);
        }

        for node in nodes.iter_mut() {
            if exported.contains(&node.key) {
                node.exported = true;
            }
        }
        let symbols = nodes.len() as u64;
        let externals = self.externals.into_inner();
        let external_packages = externals.len() as u64;
        nodes.extend(externals.into_values());

        let mut stats = ResolutionStats {
            files: self.files.len() as u64,
            symbols,
            external_packages,
            ..ResolutionStats::default()
        };
        let mut edges = Vec::with_capacity(self.edges.len());
        for ((src, dst, kind), acc) in self.edges {
            *stats.edges_by_kind.entry(kind.to_string()).or_default() += 1;
            *stats
                .edges_by_confidence
                .entry(acc.confidence.as_str().to_string())
                .or_default() += 1;
            edges.push(EdgeRecord {
                src,
                dst,
                kind: kind.to_string(),
                confidence: acc.confidence,
                rule: acc.rule.to_string(),
                evidence: acc.evidence,
                count: acc.count,
            });
        }
        stats.edges = edges.len() as u64;
        stats.references = self.refs.len() as u64;
        for r in &self.refs {
            let label = match r.outcome {
                Outcome::Resolved => "resolved",
                Outcome::External => "external",
                Outcome::Builtin => "builtin",
                Outcome::Unresolved => "unresolved",
            };
            *stats
                .references_by_outcome
                .entry(label.to_string())
                .or_default() += 1;
        }
        Resolution {
            nodes,
            edges,
            references: self.refs,
            stats,
        }
    }

    // ---- phases -----------------------------------------------------------

    /// Symbol nodes and CONTAINS edges.
    fn structure(&mut self, fi: usize, nodes: &mut Vec<NodeRecord>) {
        let (path, ir) = (self.files[fi].path, self.files[fi].ir);
        let fkey = file_key(path);
        for sym in &ir.symbols {
            let key = self.files[fi].keys[sym.id as usize].clone();
            let mut attrs = sym.attrs.clone();
            attrs.insert("language".into(), ir.language.as_str().into());
            nodes.push(NodeRecord {
                key: key.clone(),
                kind: sym.kind.as_str().to_string(),
                name: sym.name.clone(),
                qualified_name: Some(sym.qualified_name.clone()),
                path: Some(path.to_string()),
                span: Some(sym.span),
                signature: sym.signature.clone(),
                doc: sym.doc.clone(),
                exported: sym.exported,
                attrs,
            });
            let parent = match sym.parent {
                Some(p) => self.files[fi].keys[p as usize].clone(),
                None => fkey.clone(),
            };
            let evidence = Evidence {
                path: path.to_string(),
                line: sym.span.start_line,
                col: sym.span.start_col,
            };
            self.edge(
                parent,
                key,
                "CONTAINS",
                Confidence::Confirmed,
                "structure.declaration",
                evidence,
            );
        }
    }

    /// EXTENDS / IMPLEMENTS edges, and the base-class table used for member lookup.
    fn heritage(&mut self, fi: usize) {
        let ir = self.files[fi].ir;
        for sym in &ir.symbols {
            for (list, kind, label) in [
                (&sym.bases, "EXTENDS", "extends"),
                (&sym.implements, "IMPLEMENTS", "implements"),
            ] {
                for base in list {
                    let res = if base.segments.is_empty() {
                        Err(Miss::new(
                            "heritage.computed",
                            "base is a computed expression",
                        ))
                    } else {
                        self.lookup(fi, sym.parent, &base.segments)
                    };
                    let mut kind = kind;
                    let mut label = label;
                    if let Ok(Found {
                        target: Target::Symbol(tf, tid),
                        ..
                    }) = &res
                    {
                        let target_kind = self.files[*tf].ir.symbols[*tid as usize].kind;
                        // A C# base list names the base class and the
                        // implemented interfaces alike.
                        if kind == "EXTENDS"
                            && target_kind == SymbolKind::Interface
                            && sym.kind == SymbolKind::Class
                            && self.files[fi].family == LanguageFamily::Csharp
                        {
                            kind = "IMPLEMENTS";
                            label = "implements";
                        }
                        if matches!(target_kind, SymbolKind::Class | SymbolKind::Interface)
                            && (*tf, *tid) != (fi, sym.id)
                        {
                            self.bases
                                .borrow_mut()
                                .entry((fi, sym.id))
                                .or_default()
                                .push((*tf, *tid));
                        }
                    }
                    self.record(
                        fi,
                        Some(sym.id),
                        label,
                        &base.text,
                        base.span.start_line,
                        base.span.start_col,
                        res,
                        kind,
                    );
                }
            }
        }
    }

    /// IMPORTS edges from import statements and re-export sources.
    fn imports(&mut self, fi: usize) {
        let ir = self.files[fi].ir;
        let family = self.files[fi].family;
        for (idx, imp) in ir.imports.iter().enumerate() {
            let text = match (family, imp.kind) {
                (f, ImportKind::Wildcard) if is_classic(f) => format!("{}.*", imp.module),
                (f, _) if is_classic(f) && !imp.module.is_empty() => {
                    format!("{}.{}", imp.module, imp.imported.as_deref().unwrap_or(""))
                }
                _ => format!("{}{}", ".".repeat(imp.level as usize), imp.module),
            };
            let res = match family {
                LanguageFamily::Go => self.go_import(fi, idx),
                LanguageFamily::Java | LanguageFamily::Kotlin | LanguageFamily::Csharp => {
                    self.java_import(fi, idx)
                }
                LanguageFamily::Rust => self.rust_import(fi, idx),
                LanguageFamily::C => self.c_include(fi, idx),
                LanguageFamily::Ecmascript => self.es_module(fi, &imp.module),
                LanguageFamily::Python => {
                    let module = self.py_module(fi, &imp.module, imp.level);
                    if imp.kind == ImportKind::Named {
                        // `from pkg import submodule`: the binding may be a
                        // module even when `pkg` itself has no `__init__`.
                        match (module, self.binding(fi, idx, 0)) {
                            (
                                Ok(m),
                                Ok(Found {
                                    target: Target::File(sub),
                                    confidence,
                                    rule,
                                }),
                            ) => {
                                let sub_res = Ok(Found {
                                    target: Target::File(sub),
                                    confidence,
                                    rule,
                                });
                                self.record(
                                    fi,
                                    None,
                                    "import",
                                    &text,
                                    imp.span.start_line,
                                    imp.span.start_col,
                                    sub_res,
                                    "IMPORTS",
                                );
                                Ok(m)
                            }
                            (
                                Err(_),
                                Ok(
                                    found @ Found {
                                        target: Target::File(_),
                                        ..
                                    },
                                ),
                            ) => Ok(found),
                            (m, _) => m,
                        }
                    } else {
                        module
                    }
                }
            };
            self.record(
                fi,
                None,
                "import",
                &text,
                imp.span.start_line,
                imp.span.start_col,
                res,
                "IMPORTS",
            );
        }
        if family == LanguageFamily::Ecmascript {
            for exp in &ir.exports {
                if let Some(source) = &exp.source {
                    let res = self.es_module(fi, source);
                    self.record(
                        fi,
                        None,
                        "import",
                        source,
                        exp.span.start_line,
                        exp.span.start_col,
                        res,
                        "IMPORTS",
                    );
                }
            }
        }
    }

    /// EXPORTS edges from a file to the symbols that form its public surface.
    fn exports(&mut self, fi: usize, exported: &mut HashSet<String>) {
        let ir = self.files[fi].ir;
        let family = self.files[fi].family;
        let path = self.files[fi].path;
        let mut done: HashSet<&str> = HashSet::new();
        for exp in &ir.exports {
            if exp.kind == ExportKind::Wildcard || !done.insert(exp.name.as_str()) {
                continue;
            }
            let res = match family {
                LanguageFamily::Ecmascript => self.es_export(fi, &exp.name, 0),
                LanguageFamily::Python => self.py_attr(fi, &exp.name, 0),
                // Export status is syntactic (`exported`); no export lists.
                _ => continue,
            };
            if let Ok(Found {
                target: Target::Symbol(tf, tid),
                confidence,
                rule,
            }) = res
            {
                let key = self.files[tf].keys[tid as usize].clone();
                exported.insert(key.clone());
                let evidence = Evidence {
                    path: path.to_string(),
                    line: exp.span.start_line,
                    col: exp.span.start_col,
                };
                self.edge(file_key(path), key, "EXPORTS", confidence, rule, evidence);
            }
        }
    }

    fn references(&mut self, fi: usize) {
        let ir = self.files[fi].ir;
        let family = self.files[fi].family;
        for r in &ir.references {
            let mut res = if r.path.is_empty() && !r.chain.is_empty() {
                self.resolve_chain(fi, r.scope, &r.chain)
            } else if r.path.is_empty() {
                match &r.member {
                    Some(_) => Err(Miss::receiver(
                        "receiver.dynamic",
                        "receiver is a computed expression",
                    )),
                    None => Err(Miss::new(
                        "callee.dynamic",
                        "callee is a computed expression",
                    )),
                }
            } else {
                self.lookup(fi, r.scope, &r.path)
            };
            // A namespace object is not itself callable: use the default export.
            if let (
                Ok(Found {
                    target: Target::File(tf),
                    confidence,
                    ..
                }),
                ReferenceKind::Call | ReferenceKind::New,
            ) = (&res, r.kind)
            {
                res = match family {
                    LanguageFamily::Ecmascript => {
                        self.es_export(*tf, "default", 0).map(|f| Found {
                            confidence: f.confidence.min(*confidence),
                            ..f
                        })
                    }
                    _ => Err(Miss::new(
                        "callee.module",
                        "a module object is not callable",
                    )),
                };
            }
            // `[Serializable]` names the class `SerializableAttribute`; that
            // name is preferred when it resolves.
            if r.kind == ReferenceKind::Decorator
                && family == LanguageFamily::Csharp
                && !r.path.is_empty()
                && !r.path.last().is_some_and(|l| l.ends_with("Attribute"))
            {
                let mut path = r.path.clone();
                if let Some(last) = path.last_mut() {
                    last.push_str("Attribute");
                }
                match self.lookup(fi, r.scope, &path) {
                    Ok(
                        found @ Found {
                            target: Target::Symbol(..),
                            ..
                        },
                    ) => res = Ok(found),
                    Ok(found) if res.is_err() => res = Ok(found),
                    _ => {}
                }
            }
            if let Err(miss) = &res {
                if miss.unknown_receiver {
                    let name = r
                        .member
                        .as_deref()
                        .or(r.path.last().map(String::as_str))
                        .unwrap_or("");
                    if let Some(found) = self.unique_method(family, name) {
                        res = Ok(found);
                    }
                }
            }
            let edge_kind = match (r.kind, &res) {
                (ReferenceKind::New, _) => "INSTANTIATES",
                // `super(...)` runs the base constructor on the same instance.
                (
                    ReferenceKind::Call,
                    Ok(Found {
                        rule: "es.call.super-constructor" | "java.call.super-constructor",
                        ..
                    }),
                ) => "CALLS",
                // A conversion to a named non-struct type (`Celsius(x)`).
                (
                    ReferenceKind::Call,
                    Ok(Found {
                        target: Target::Symbol(tf, tid),
                        ..
                    }),
                ) if self.files[*tf].ir.symbols[*tid as usize].kind == SymbolKind::Type => {
                    "REFERENCES"
                }
                (
                    ReferenceKind::Call,
                    Ok(Found {
                        target: Target::Symbol(tf, tid),
                        ..
                    }),
                ) if self.files[*tf].ir.symbols[*tid as usize].kind == SymbolKind::Class => {
                    "INSTANTIATES"
                }
                (ReferenceKind::Call, _) => "CALLS",
                (ReferenceKind::Jsx | ReferenceKind::Decorator, _) => "REFERENCES",
            };
            self.record(
                fi,
                r.scope,
                r.kind.as_str(),
                &r.text,
                r.span.start_line,
                r.span.start_col,
                res,
                edge_kind,
            );
        }
    }

    // ---- recording --------------------------------------------------------

    fn target_key(&self, target: &Target) -> String {
        match target {
            Target::Symbol(f, id) => self.files[*f].keys[*id as usize].clone(),
            Target::File(f) => file_key(self.files[*f].path),
            Target::Asset(path) => file_key(path),
            Target::External(key) => key.clone(),
            Target::GoPackage(p) => dir_key(&self.go.packages[*p].dir),
            Target::JavaPackage(name) => match self.java.package_dir(name, &self.files) {
                Some((dir, _)) => dir_key(&dir),
                None => "repo".to_string(),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        fi: usize,
        scope: Option<u32>,
        kind: &str,
        text: &str,
        line: u32,
        col: u32,
        res: Res,
        edge_kind: &'static str,
    ) {
        let path = self.files[fi].path;
        let source = match scope {
            Some(s) => self.files[fi].keys[s as usize].clone(),
            None => file_key(path),
        };
        // Rules shared by the JVM and .NET languages are reported under the
        // referencing file's language.
        let family = self.files[fi].family;
        let res = match res {
            Ok(found) => Ok(Found {
                rule: localize_rule(family, found.rule),
                ..found
            }),
            Err(miss) => Err(Miss {
                rule: localize_rule(family, miss.rule),
                ..miss
            }),
        };
        let rec = match res {
            Ok(found) => {
                let target = self.target_key(&found.target);
                let outcome = match found.target {
                    Target::External(_) => Outcome::External,
                    _ => Outcome::Resolved,
                };
                let evidence = Evidence {
                    path: path.to_string(),
                    line,
                    col,
                };
                self.edge(
                    source.clone(),
                    target.clone(),
                    edge_kind,
                    found.confidence,
                    found.rule,
                    evidence,
                );
                ReferenceRecord {
                    path: path.to_string(),
                    line,
                    col,
                    kind: kind.to_string(),
                    text: text.to_string(),
                    source,
                    outcome,
                    confidence: found.confidence,
                    target: Some(target),
                    rule: found.rule.to_string(),
                    reason: None,
                }
            }
            Err(miss) => ReferenceRecord {
                path: path.to_string(),
                line,
                col,
                kind: kind.to_string(),
                text: text.to_string(),
                source,
                outcome: if miss.builtin {
                    Outcome::Builtin
                } else {
                    Outcome::Unresolved
                },
                confidence: if miss.builtin {
                    Confidence::Confirmed
                } else {
                    Confidence::Unknown
                },
                target: None,
                rule: miss.rule.to_string(),
                reason: if miss.builtin {
                    None
                } else {
                    Some(miss.reason)
                },
            },
        };
        self.refs.push(rec);
    }

    fn edge(
        &mut self,
        src: String,
        dst: String,
        kind: &'static str,
        confidence: Confidence,
        rule: &'static str,
        evidence: Evidence,
    ) {
        let acc = self.edges.entry((src, dst, kind)).or_insert(EdgeAcc {
            confidence,
            rule,
            evidence: None,
            count: 0,
        });
        acc.count += 1;
        // Keep the strongest evidence; the first occurrence wins ties.
        if acc.evidence.is_none() || confidence < acc.confidence {
            acc.confidence = confidence;
            acc.rule = rule;
            acc.evidence = Some(evidence);
        }
    }

    // ---- name resolution --------------------------------------------------

    fn sym(&self, r: SymRef) -> &'a Symbol {
        &self.files[r.0].ir.symbols[r.1 as usize]
    }

    fn child_named(&self, fi: usize, parent: u32, name: &str) -> Option<u32> {
        let ir = self.files[fi].ir;
        let kids = self.files[fi].children.get(&parent)?;
        // Java constructors are not members; they share the class's name.
        let named = |k: &u32| {
            let sym = &ir.symbols[*k as usize];
            sym.name == name && !sym.attrs.contains_key("constructor")
        };
        // Prefer callables so `Class.method` wins over a same-named field.
        kids.iter()
            .copied()
            .find(|k| named(k) && ir.symbols[*k as usize].kind.is_callable())
            .or_else(|| kids.iter().copied().find(named))
    }

    /// A member of a class or interface, searching resolved base classes.
    /// The flag reports whether the member was inherited.
    fn member(&self, class: SymRef, name: &str, depth: u32) -> Option<(SymRef, bool)> {
        // A C++ member function defined outside its class: the definition
        // is the call target, not the in-class declaration.
        if self.files[class.0].family == LanguageFamily::C {
            if let Some(m) = self.c_method(class, name) {
                return Some((m, false));
            }
        }
        if let Some(id) = self.child_named(class.0, class.1, name) {
            return Some(((class.0, id), false));
        }
        // The other parts of a C# `partial` or Kotlin `expect`/`actual` type.
        if let Some(parts) = self.java.parts.get(&class) {
            for part in parts.iter().filter(|p| **p != class) {
                if let Some(id) = self.child_named(part.0, part.1, name) {
                    return Some(((part.0, id), false));
                }
            }
        }
        let found = match self.files[class.0].family {
            LanguageFamily::Go => self.go_method(class, name),
            LanguageFamily::Rust => self.rust_method(class, name),
            _ => None,
        };
        if let Some(m) = found {
            return Some((m, false));
        }
        self.inherited_member(class, name, depth)
    }

    fn inherited_member(&self, class: SymRef, name: &str, depth: u32) -> Option<(SymRef, bool)> {
        if depth >= MAX_CHAIN {
            return None;
        }
        let bases = self.bases.borrow().get(&class).cloned().unwrap_or_default();
        bases
            .into_iter()
            .find_map(|b| self.member(b, name, depth + 1))
            .map(|(m, _)| (m, true))
    }

    /// A module-level symbol by name with the confidence its declaration supports.
    pub(crate) fn top_symbol(&self, fi: usize, name: &str, rule: &'static str) -> Option<Found> {
        let ids = self.files[fi].top.get(name)?;
        let ir = self.files[fi].ir;
        let is_type = |id: &u32| {
            matches!(
                ir.symbols[*id as usize].kind,
                SymbolKind::Interface | SymbolKind::Type | SymbolKind::Namespace
            )
        };
        // Declaration merging (interface + class, function + namespace) is
        // not ambiguity; several value declarations of one name are.
        let values: Vec<u32> = ids.iter().copied().filter(|id| !is_type(id)).collect();
        let chosen = values.first().or(ids.first()).copied()?;
        let anonymous = ir.symbols[chosen as usize]
            .attrs
            .contains_key("in_anonymous_scope");
        let confidence = if values.len() > 1 || anonymous {
            Confidence::Probable
        } else {
            Confidence::Confirmed
        };
        Some(Found {
            target: Target::Symbol(fi, chosen),
            confidence,
            rule,
        })
    }

    fn enclosing_class(&self, fi: usize, scope: Option<u32>) -> Option<u32> {
        let ir = self.files[fi].ir;
        let mut cur = scope;
        while let Some(id) = cur {
            let sym = &ir.symbols[id as usize];
            if sym.kind == SymbolKind::Class {
                return Some(id);
            }
            cur = sym.parent;
        }
        None
    }

    /// Resolve a dotted name as seen from `scope` in file `fi`.
    pub(crate) fn lookup(&self, fi: usize, scope: Option<u32>, path: &[String]) -> Res {
        match self.files[fi].family {
            LanguageFamily::Go => return self.go_lookup(fi, scope, path),
            LanguageFamily::Java | LanguageFamily::Kotlin | LanguageFamily::Csharp => {
                return self.java_lookup(fi, scope, path)
            }
            LanguageFamily::Rust => return self.rust_lookup(fi, scope, path),
            LanguageFamily::C => return self.c_lookup(fi, scope, path),
            _ => {}
        }
        let file = &self.files[fi];
        let ir = file.ir;
        let first = path[0].as_str();
        let python = file.family == LanguageFamily::Python;

        // Receivers bound to the enclosing class.
        let receiver = if python {
            matches!(first, "self" | "cls" | "super")
        } else {
            matches!(first, "this" | "super")
        };
        if receiver {
            if let Some(class) = self.enclosing_class(fi, scope) {
                return self.receiver(fi, class, path);
            }
        }

        // Code directly in a class body sees the names bound in that body.
        if let Some(class) = scope.filter(|s| ir.symbols[*s as usize].kind == SymbolKind::Class) {
            if let Some(member) = self.child_named(fi, class, first) {
                let base = Found {
                    target: Target::Symbol(fi, member),
                    confidence: Confidence::Confirmed,
                    rule: "scope.class-member",
                };
                // `@prop.setter` refers to the property it extends.
                if python
                    && path.len() == 2
                    && matches!(path[1].as_str(), "setter" | "getter" | "deleter")
                {
                    return Ok(Found {
                        rule: "py.property-accessor",
                        ..base
                    });
                }
                return self.descend(base, &path[1..]);
            }
        }

        // Lexical scopes, innermost first.
        let mut cur = scope;
        while let Some(id) = cur {
            let sym = &ir.symbols[id as usize];
            if sym
                .locals
                .binary_search_by(|l| l.as_str().cmp(first))
                .is_ok()
            {
                if let Some(child) = self.child_named(fi, id, first) {
                    let base = Found {
                        target: Target::Symbol(fi, child),
                        confidence: Confidence::Confirmed,
                        rule: "scope.nested-definition",
                    };
                    return self.descend(base, &path[1..]);
                }
                if let Some(&imp) = file.bindings.get(&(Some(id), first)) {
                    let base = self.binding(fi, imp, 0)?;
                    return self.descend(base, &path[1..]);
                }
                if python {
                    if let Some(res) = self.py_dotted_import(fi, Some(id), path) {
                        return res;
                    }
                }
                return Err(Miss::receiver(
                    "scope.local-binding",
                    format!(
                        "`{first}` is a local binding of `{}`; its value is not tracked",
                        sym.qualified_name
                    ),
                ));
            }
            if sym.kind == SymbolKind::Namespace {
                if let Some(child) = self.child_named(fi, id, first) {
                    let base = Found {
                        target: Target::Symbol(fi, child),
                        confidence: Confidence::Confirmed,
                        rule: "scope.namespace-member",
                    };
                    return self.descend(base, &path[1..]);
                }
            }
            cur = sym.parent;
        }

        // Module scope.
        if scope.is_none() && file.anon_locals.contains(first) {
            let anonymous_def = file.top.get(first).and_then(|ids| {
                ids.iter().find(|id| {
                    ir.symbols[**id as usize]
                        .attrs
                        .contains_key("in_anonymous_scope")
                })
            });
            return match anonymous_def {
                Some(&id) => {
                    let base = Found { target: Target::Symbol(fi, id), confidence: Confidence::Probable, rule: "scope.anonymous-function-definition" };
                    self.descend(base, &path[1..])
                }
                None => Err(Miss::receiver(
                    "scope.local-binding",
                    format!("`{first}` is bound inside an anonymous function or block; its value is not tracked"),
                )),
            };
        }
        if let Some(base) = self.top_symbol(fi, first, "scope.module-symbol") {
            return self.descend(base, &path[1..]);
        }
        if let Some(&imp) = file.bindings.get(&(None, first)) {
            let base = self.binding(fi, imp, 0)?;
            return self.descend(base, &path[1..]);
        }
        if python {
            if let Some(res) = self.py_dotted_import(fi, None, path) {
                return res;
            }
            if let Some(found) = self.py_wildcard(fi, first, 0) {
                return self.descend(found, &path[1..]);
            }
        }
        let builtin = if python {
            builtins::is_python_builtin(first)
        } else {
            builtins::is_ecmascript_global(first)
        };
        if builtin {
            return Err(Miss {
                rule: "builtin",
                reason: String::new(),
                builtin: true,
                unknown_receiver: false,
            });
        }
        Err(Miss::new(
            "scope.unbound",
            format!("no definition or import of `{first}` in this file"),
        ))
    }

    /// `this.m`, `super.m`, `self.m`, `cls.m`.
    fn receiver(&self, fi: usize, class: u32, path: &[String]) -> Res {
        let python = self.files[fi].family == LanguageFamily::Python;
        let first = path[0].as_str();
        let is_super = first == "super";
        let class_name = &self.files[fi].ir.symbols[class as usize].qualified_name;
        match path.len() {
            1 if is_super && !python => {
                let base = self
                    .bases
                    .borrow()
                    .get(&(fi, class))
                    .and_then(|b| b.first().copied());
                match base {
                    Some((bf, bid)) => Ok(Found {
                        target: Target::Symbol(bf, bid),
                        confidence: Confidence::Probable,
                        rule: "es.call.super-constructor",
                    }),
                    None => Err(Miss::new(
                        "es.call.super-constructor",
                        format!("base class of `{class_name}` is not resolved"),
                    )),
                }
            }
            1 if first == "cls" => Ok(Found {
                target: Target::Symbol(fi, class),
                confidence: Confidence::Probable,
                rule: "py.call.cls",
            }),
            1 if is_super => Err(Miss {
                rule: "builtin",
                reason: String::new(),
                builtin: true,
                unknown_receiver: false,
            }),
            1 => Err(Miss::new(
                "receiver.instance-call",
                format!("calling an instance of `{class_name}`"),
            )),
            2 => {
                let name = path[1].as_str();
                let found = if is_super {
                    self.inherited_member((fi, class), name, 0)
                } else {
                    self.member((fi, class), name, 0)
                };
                let rule = match (python, is_super) {
                    (true, true) => "py.call.super-method",
                    (true, false) => "py.call.self-method",
                    (false, true) => "es.call.super-method",
                    (false, false) => "es.call.this-method",
                };
                match found {
                    // Dynamic dispatch may select an override in a subclass.
                    Some(((tf, tid), _)) => Ok(Found { target: Target::Symbol(tf, tid), confidence: Confidence::Probable, rule }),
                    None => Err(Miss::new(
                        rule,
                        format!("`{name}` is not a method or class-level attribute of `{class_name}` or its resolved base classes (attributes assigned on instances at runtime are not tracked)"),
                    )),
                }
            }
            _ => Err(Miss::receiver(
                "receiver.member-type-unknown",
                format!(
                    "the type of `{}.{}` is not statically known",
                    path[0], path[1]
                ),
            )),
        }
    }

    /// Follow member accesses from a resolved base.
    fn descend(&self, base: Found, rest: &[String]) -> Res {
        let mut cur = base;
        for (i, seg) in rest.iter().enumerate() {
            cur = match cur.target {
                Target::Symbol(f, id) => {
                    let sym = self.sym((f, id));
                    let family = self.files[f].family;
                    let typed = family.is_statically_typed();
                    if typed && sym.kind == SymbolKind::Variable {
                        // `val repo = Repo()`: the type of the initializer.
                        if declared_type(sym).is_none() {
                            if let Some(call) = sym.attrs.get("init_call") {
                                let rule = family_rule(family, "call-result");
                                if let Some(res) = self.call_value(
                                    f,
                                    sym.parent,
                                    &sym.name,
                                    call,
                                    &rest[i..],
                                    rule,
                                ) {
                                    let found = res?;
                                    return Ok(Found {
                                        confidence: found.confidence.min(cur.confidence),
                                        ..found
                                    });
                                }
                            }
                        }
                        if let Some(ty) = declared_type(sym) {
                            // A field or variable of a declared type: members
                            // of the type.
                            let rule = family_rule(family, "typed-value");
                            let found =
                                self.typed_value(f, sym.parent, &sym.name, &ty, &rest[i..], rule)?;
                            return Ok(Found {
                                confidence: found.confidence.min(cur.confidence),
                                ..found
                            });
                        }
                    }
                    if family == LanguageFamily::Rust && sym.kind == SymbolKind::Namespace {
                        let found = self.rust_module_member(rust::ModRef::Ns(f, id), seg)?;
                        cur = Found {
                            confidence: found.confidence.min(cur.confidence),
                            rule: cur.rule,
                            ..found
                        };
                        continue;
                    }
                    let has_members = matches!(
                        sym.kind,
                        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Namespace
                    ) || (family.is_statically_typed()
                        && sym.kind == SymbolKind::Type);
                    if !has_members {
                        return Err(Miss::receiver(
                            "member.value-type-unknown",
                            format!(
                                "`{}` is a {}; the type of its member `{seg}` is not tracked",
                                sym.qualified_name,
                                sym.kind.as_str()
                            ),
                        ));
                    }
                    match self.member((f, id), seg, 0) {
                        Some(((tf, tid), inherited)) => Found {
                            target: Target::Symbol(tf, tid),
                            confidence: cur.confidence.min(if inherited {
                                Confidence::Probable
                            } else {
                                Confidence::Confirmed
                            }),
                            rule: cur.rule,
                        },
                        None => {
                            if is_classic(family) {
                                if let Some(found) = self.java_external_base((f, id), 0) {
                                    return Ok(found);
                                }
                            }
                            return Err(Miss::new(
                                "member.not-found",
                                format!(
                                    "`{}` has no statically known member `{seg}`",
                                    sym.qualified_name
                                ),
                            ));
                        }
                    }
                }
                Target::File(tf) => {
                    let found = match self.files[tf].family {
                        LanguageFamily::Ecmascript => self.es_export(tf, seg, 0)?,
                        LanguageFamily::Python => self.py_attr(tf, seg, 0)?,
                        LanguageFamily::Rust => {
                            self.rust_module_member(rust::ModRef::File(tf), seg)?
                        }
                        _ => {
                            return Err(Miss::new(
                                "member.file",
                                "members of this file are not module attributes",
                            ))
                        }
                    };
                    Found {
                        confidence: found.confidence.min(cur.confidence),
                        rule: cur.rule,
                        ..found
                    }
                }
                Target::GoPackage(p) => {
                    let found = self.go_package_member(p, seg)?;
                    Found {
                        confidence: found.confidence.min(cur.confidence),
                        rule: cur.rule,
                        ..found
                    }
                }
                Target::JavaPackage(ref name) => {
                    return Err(Miss::new(
                        "member.package",
                        format!("`{name}.{seg}` is not a type in the repository"),
                    ))
                }
                Target::Asset(path) => {
                    return Err(Miss::new(
                        "member.asset",
                        format!("`{path}` is not analyzed source"),
                    ));
                }
                // Members of an external package stay attributed to the package.
                Target::External(_) => return Ok(cur),
            };
        }
        Ok(cur)
    }

    /// Follow a call chain (`Reference::chain`) through declared result types.
    fn resolve_chain(&self, fi: usize, scope: Option<u32>, chain: &[String]) -> Res {
        let family = self.files[fi].family;
        let rule = family_rule(family, "call-result");
        let mut found: Option<Found> = None;
        let mut value: Option<(Found, String)> = None;
        let mut i = 0;
        while i < chain.len() {
            let token = chain[i].as_str();
            if token == "()" {
                let callee = found.take().ok_or_else(|| {
                    Miss::receiver("receiver.dynamic", "the called value is not tracked")
                })?;
                value = Some(self.call_result_type(callee, rule)?);
                i += 1;
                continue;
            }
            if let Some(ty) = token.strip_prefix("type:") {
                let segments: Vec<String> = ty.split('.').map(str::to_string).collect();
                let resolved = match self.lookup(fi, scope, &segments) {
                    Ok(t) => t,
                    Err(miss) if miss.builtin => return Err(miss),
                    Err(miss) => {
                        return Err(Miss::receiver(
                            rule,
                            format!("type `{ty}` is not resolved ({})", miss.reason),
                        ))
                    }
                };
                value = Some((resolved, ty.to_string()));
                i += 1;
                continue;
            }
            let mut j = i;
            while j < chain.len() && chain[j] != "()" && !chain[j].starts_with("type:") {
                j += 1;
            }
            let names = &chain[i..j];
            found = Some(match value.take() {
                None => self.lookup(fi, scope, names)?,
                Some((ty, text)) => {
                    let confidence = ty.confidence;
                    let f = self.typed_member(family, ty, "the value", &text, names, rule)?;
                    Found {
                        confidence: f.confidence.min(confidence),
                        ..f
                    }
                }
            });
            i = j;
        }
        found.ok_or_else(|| Miss::receiver("receiver.dynamic", "receiver is a computed expression"))
    }

    /// The declared result type of calling `callee` (a function, method or,
    /// for conversions, a type), with a description for messages.
    fn call_result_type(&self, callee: Found, rule: &'static str) -> Result<(Found, String), Miss> {
        let (cf, cid) =
            match callee.target {
                Target::Symbol(cf, cid) => (cf, cid),
                Target::External(_) => return Err(Miss::receiver(
                    rule,
                    "the result of a function outside the repository has a type that is not known",
                )),
                _ => return Err(Miss::receiver(rule, "the called value is not a function")),
            };
        let csym = self.sym((cf, cid));
        if is_type_kind(csym.kind) {
            return Ok((callee, csym.name.clone()));
        }
        let ty: Vec<String> = csym
            .attrs
            .get("result_types")
            .and_then(|r| r.get(0))
            .and_then(|t| t.as_array())
            .map(|t| {
                t.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if ty.is_empty() {
            return Err(Miss::receiver(
                rule,
                format!(
                    "`{}` has no declared named result type",
                    csym.qualified_name
                ),
            ));
        }
        match self.lookup(cf, csym.parent, &ty) {
            Ok(t) => Ok((
                Found {
                    confidence: t.confidence.min(callee.confidence),
                    ..t
                },
                ty.join("."),
            )),
            Err(miss) if miss.builtin => Err(miss),
            Err(miss) => Err(Miss::receiver(
                rule,
                format!(
                    "result type `{}` of `{}` is not resolved ({})",
                    ty.join("."),
                    csym.qualified_name,
                    miss.reason
                ),
            )),
        }
    }

    /// A local initialized from a call (`attrs.local_calls`): members of the
    /// callee's declared result type. `None` when `name` is not such a local.
    pub(crate) fn call_result_value(
        &self,
        fi: usize,
        scope: u32,
        name: &str,
        rest: &[String],
        rule: &'static str,
    ) -> Option<Res> {
        let sym = &self.files[fi].ir.symbols[scope as usize];
        let call = sym.attrs.get("local_calls")?.get(name)?;
        self.call_value(fi, Some(scope), name, call, rest, rule)
    }

    /// The value of a call described by `{callee, result}`, looked up from
    /// `scope` of file `fi`, accessed with `rest`.
    fn call_value(
        &self,
        fi: usize,
        scope: Option<u32>,
        name: &str,
        call: &serde_json::Value,
        rest: &[String],
        rule: &'static str,
    ) -> Option<Res> {
        let callee: Vec<String> = call
            .get("callee")?
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        let index = call.get("result")?.as_u64()? as usize;
        if callee.is_empty() {
            return None;
        }
        let callee_text = callee.join(".");
        if self.call_depth.get() >= 8 {
            return Some(Err(Miss::receiver(
                rule,
                format!("`{name}` is initialized by a chain of calls too deep to follow"),
            )));
        }
        if rest.is_empty() {
            return Some(Err(Miss::receiver(
                rule,
                format!("`{name}` holds the result of `{callee_text}`; calling it is not tracked"),
            )));
        }
        self.call_depth.set(self.call_depth.get() + 1);
        let callee_res = self.lookup(fi, scope, &callee);
        self.call_depth.set(self.call_depth.get() - 1);
        let family = self.files[fi].family;
        let res = match callee_res {
            Ok(Found {
                target: Target::Symbol(cf, cid),
                confidence,
                ..
            }) => {
                let csym = self.sym((cf, cid));
                if is_type_kind(csym.kind) {
                    // A conversion `T(x)`: the value has type `T`.
                    let resolved = Found {
                        target: Target::Symbol(cf, cid),
                        confidence,
                        rule,
                    };
                    self.typed_member(family, resolved, name, &csym.name, rest, rule)
                } else {
                    let ty: Option<Vec<String>> = csym
                        .attrs
                        .get("result_types")
                        .and_then(|r| r.get(index))
                        .and_then(|t| t.as_array())
                        .map(|t| {
                            t.iter()
                                .filter_map(|s| s.as_str().map(str::to_string))
                                .collect()
                        });
                    match ty {
                        Some(ty) if !ty.is_empty() => {
                            self.typed_value(cf, csym.parent, name, &ty, rest, rule)
                                .map(|f| Found {
                                    confidence: f.confidence.min(confidence),
                                    ..f
                                })
                        }
                        _ => Err(Miss::receiver(
                            rule,
                            format!(
                                "`{name}` holds result {} of `{}`, which has no declared named type",
                                index + 1,
                                csym.qualified_name
                            ),
                        )),
                    }
                }
            }
            Ok(_) => Err(Miss::receiver(
                rule,
                format!("`{name}` holds the result of `{callee_text}`, defined outside the repository; its type is not known"),
            )),
            Err(miss) if miss.builtin => Err(Miss::receiver(
                rule,
                format!("`{name}` holds the result of builtin `{callee_text}`; its type is not tracked"),
            )),
            Err(miss) => Err(Miss::receiver(
                rule,
                format!(
                    "`{name}` holds the result of `{callee_text}`, which is not resolved ({})",
                    miss.reason
                ),
            )),
        };
        Some(res)
    }

    /// `rest` accessed on a value named `name` whose declared type is `ty`
    /// (as written, resolved from `scope` of file `fi`). Statically typed
    /// languages only.
    pub(crate) fn typed_value(
        &self,
        fi: usize,
        scope: Option<u32>,
        name: &str,
        ty: &[String],
        rest: &[String],
        rule: &'static str,
    ) -> Res {
        let family = self.files[fi].family;
        let type_text = ty.join(".");
        if rest.is_empty() {
            return Err(Miss::receiver(
                rule,
                format!("`{name}` is a value of type `{type_text}`; calling it is not tracked"),
            ));
        }
        let resolved = match self.lookup(fi, scope, ty) {
            Ok(found) => found,
            Err(miss) if miss.builtin => return Err(miss),
            Err(miss) => {
                return Err(Miss::receiver(
                    rule,
                    format!(
                        "type `{type_text}` of `{name}` is not resolved ({})",
                        miss.reason
                    ),
                ))
            }
        };
        self.typed_member(family, resolved, name, &type_text, rest, rule)
    }

    /// `rest` accessed on a value named `name` of the already resolved type
    /// `resolved`.
    fn typed_member(
        &self,
        family: LanguageFamily,
        resolved: Found,
        name: &str,
        type_text: &str,
        rest: &[String],
        rule: &'static str,
    ) -> Res {
        let (tf, tid) = match resolved.target {
            Target::Symbol(tf, tid) if is_type_kind(self.sym((tf, tid)).kind) => (tf, tid),
            Target::External(_) => return Ok(Found { rule, ..resolved }),
            _ => {
                return Err(Miss::receiver(
                    rule,
                    format!("`{type_text}` (type of `{name}`) is not a type"),
                ))
            }
        };
        let seg = rest[0].as_str();
        let type_sym = self.sym((tf, tid));
        // A type alias (`type A = B`, `typedef B A`): members of the aliased
        // type. Defined types (`type A B` in Go) have their own method set.
        if type_sym.kind == SymbolKind::Type
            && type_sym.attrs.get("type_kind").and_then(|v| v.as_str()) != Some("defined")
            && self.member((tf, tid), seg, 0).is_none()
        {
            if let Some(underlying) = type_sym
                .attrs
                .get("underlying")
                .and_then(|u| u.as_array())
                .map(|u| {
                    u.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .filter(|u| !u.is_empty())
            {
                if self.call_depth.get() < 8 {
                    self.call_depth.set(self.call_depth.get() + 1);
                    let target = self.lookup(tf, type_sym.parent, &underlying);
                    self.call_depth.set(self.call_depth.get() - 1);
                    match target {
                        Ok(t) if t.target != resolved.target => {
                            let confidence = t.confidence.min(resolved.confidence);
                            let t = Found { confidence, ..t };
                            return self.typed_member(
                                family,
                                t,
                                name,
                                &underlying.join("."),
                                rest,
                                rule,
                            );
                        }
                        Err(miss) if miss.builtin => return Err(miss),
                        _ => {}
                    }
                }
            }
        }
        let Some((m, inherited)) = self.member((tf, tid), seg, 0) else {
            if is_classic(family) {
                if let Some(found) = self.java_extension((tf, tid), seg) {
                    let confidence = found.confidence.min(resolved.confidence);
                    let found = Found {
                        confidence,
                        ..found
                    };
                    return self.descend(found, &rest[1..]);
                }
                if let Some(found) = self.java_external_base((tf, tid), 0) {
                    return Ok(found);
                }
            }
            return Err(Miss::new(
                rule,
                format!(
                    "`{}` (type of `{name}`) has no member `{seg}`",
                    type_sym.qualified_name
                ),
            ));
        };
        let confidence = match family {
            f if is_classic(f) => self.java_member_confidence(m, inherited),
            LanguageFamily::C => self.c_member_confidence(m, inherited),
            // Go method sets are static; calls through interfaces and
            // promoted members of embedded fields are not.
            _ if inherited || type_sym.kind == SymbolKind::Interface => Confidence::Probable,
            _ => Confidence::Confirmed,
        };
        let base = Found {
            target: Target::Symbol(m.0, m.1),
            confidence: confidence.min(resolved.confidence),
            rule,
        };
        self.descend(base, &rest[1..])
    }

    /// Last resort for calls on receivers of unknown type: a method name
    /// defined exactly once in the repository. Always `Inferred`.
    fn unique_method(&self, family: LanguageFamily, name: &str) -> Option<Found> {
        if name.is_empty() || builtins::is_ubiquitous_method(name) {
            return None;
        }
        match self.methods_by_name.get(&(family, name)).map(Vec::as_slice) {
            Some([(f, id)]) => Some(Found {
                target: Target::Symbol(*f, *id),
                confidence: Confidence::Inferred,
                rule: "heuristic.unique-method-name",
            }),
            _ => None,
        }
    }

    /// What an import binding refers to.
    pub(crate) fn binding(&self, fi: usize, import: usize, depth: u32) -> Res {
        if depth > MAX_CHAIN {
            return Err(Miss::new(
                "import.chain",
                "import chain is cyclic or too deep",
            ));
        }
        let imp = &self.files[fi].ir.imports[import];
        match self.files[fi].family {
            LanguageFamily::Go => self.go_import(fi, import),
            LanguageFamily::Java | LanguageFamily::Kotlin | LanguageFamily::Csharp => {
                self.java_import(fi, import)
            }
            LanguageFamily::Rust => self.rust_import(fi, import),
            LanguageFamily::C => self.c_include(fi, import),
            LanguageFamily::Ecmascript => {
                let module = self.es_module(fi, &imp.module)?;
                let Target::File(tf) = module.target else {
                    return Ok(module);
                };
                let name = match imp.kind {
                    ImportKind::Namespace => return Ok(module),
                    ImportKind::Default => "default",
                    ImportKind::Named => imp.imported.as_deref().unwrap_or(""),
                    _ => {
                        return Err(Miss::new(
                            "es.import.binding",
                            "import does not bind a name",
                        ))
                    }
                };
                let found = self.es_export(tf, name, depth + 1)?;
                Ok(Found {
                    confidence: found.confidence.min(module.confidence),
                    ..found
                })
            }
            LanguageFamily::Python => match imp.kind {
                ImportKind::Namespace => self.py_module(fi, &imp.module, imp.level),
                ImportKind::Named => {
                    let name = imp.imported.as_deref().unwrap_or("");
                    match self.py_module(fi, &imp.module, imp.level) {
                        // `from . import sub` inside a package's own
                        // `__init__`: the name can only be a submodule.
                        Ok(Found {
                            target: Target::File(tf),
                            ..
                        }) if tf == fi => match self.modules.py_file_at(self.files[fi].dir, name) {
                            Some(sub) => Ok(Found {
                                target: Target::File(sub),
                                confidence: Confidence::Confirmed,
                                rule: "py.import.submodule",
                            }),
                            None => Err(Miss::new(
                                "py.import.submodule",
                                format!("no submodule `{name}` next to `{}`", self.files[fi].path),
                            )),
                        },
                        Ok(Found {
                            target: Target::File(tf),
                            confidence,
                            ..
                        }) => {
                            let found = self.py_attr(tf, name, depth + 1)?;
                            Ok(Found {
                                confidence: found.confidence.min(confidence),
                                ..found
                            })
                        }
                        Ok(external) => Ok(external),
                        Err(miss) => {
                            // Namespace package: `from pkg import module`.
                            let dotted = if imp.module.is_empty() {
                                name.to_string()
                            } else {
                                format!("{}.{name}", imp.module)
                            };
                            match self.py_module(fi, &dotted, imp.level) {
                                Ok(
                                    found @ Found {
                                        target: Target::File(_),
                                        ..
                                    },
                                ) => Ok(Found {
                                    rule: "py.import.submodule",
                                    ..found
                                }),
                                _ => Err(miss),
                            }
                        }
                    }
                }
                _ => Err(Miss::new(
                    "py.import.binding",
                    "import does not bind a name",
                )),
            },
        }
    }

    /// Memoize a module-surface lookup and break cycles. A lookup that
    /// re-enters itself is a cyclic import chain and resolves to nothing.
    pub(crate) fn memoized(&self, fi: usize, name: &str, compute: impl FnOnce() -> Res) -> Res {
        let key = (fi, name.to_string());
        match self.surface_memo.borrow().get(&key) {
            Some(Some(res)) => return res.clone(),
            Some(None) => {
                return Err(Miss::new(
                    "import.cycle",
                    format!("`{name}` is defined by a cyclic import chain"),
                ))
            }
            None => {}
        }
        self.surface_memo.borrow_mut().insert(key.clone(), None);
        let res = compute();
        self.surface_memo
            .borrow_mut()
            .insert(key, Some(res.clone()));
        res
    }

    /// Resolve an exported name of an ECMAScript module, following re-exports.
    pub(crate) fn es_export(&self, fi: usize, name: &str, depth: u32) -> Res {
        self.memoized(fi, name, || self.es_export_uncached(fi, name, depth))
    }

    fn es_export_uncached(&self, fi: usize, name: &str, depth: u32) -> Res {
        if depth > MAX_CHAIN {
            return Err(Miss::new(
                "es.export.chain",
                "re-export chain is cyclic or too deep",
            ));
        }
        let file = &self.files[fi];
        let ir = file.ir;
        if let Some(exp) = ir
            .exports
            .iter()
            .find(|e| e.kind != ExportKind::Wildcard && e.name == name)
        {
            return match (&exp.source, exp.kind) {
                (Some(source), ExportKind::Namespace) => self.es_module(fi, source),
                (Some(source), _) => {
                    let module = self.es_module(fi, source)?;
                    let Target::File(tf) = module.target else {
                        return Ok(module);
                    };
                    let found =
                        self.es_export(tf, exp.local.as_deref().unwrap_or(name), depth + 1)?;
                    Ok(Found {
                        confidence: found.confidence.min(module.confidence),
                        rule: "es.export.reexport",
                        ..found
                    })
                }
                (None, _) => {
                    let Some(local) = exp.local.as_deref() else {
                        return Err(Miss::new(
                            "es.export.expression",
                            format!("export `{name}` of `{}` is an expression", file.path),
                        ));
                    };
                    if let Some(found) = self.top_symbol(fi, local, "es.export.local") {
                        return Ok(found);
                    }
                    match file.bindings.get(&(None, local)) {
                        Some(&imp) => self.binding(fi, imp, depth + 1).map(|f| Found {
                            rule: "es.export.reexport",
                            ..f
                        }),
                        None => Err(Miss::new(
                            "es.export.local",
                            format!("exported name `{local}` is not declared in `{}`", file.path),
                        )),
                    }
                }
            };
        }
        if name != "default" {
            let mut hits: Vec<Found> = Vec::new();
            for exp in ir.exports.iter().filter(|e| e.kind == ExportKind::Wildcard) {
                let Some(source) = &exp.source else { continue };
                let Ok(module) = self.es_module(fi, source) else {
                    continue;
                };
                let Target::File(tf) = module.target else {
                    continue;
                };
                if let Ok(found) = self.es_export(tf, name, depth + 1) {
                    if !hits.iter().any(|h| h.target == found.target) {
                        hits.push(Found {
                            confidence: found.confidence.min(module.confidence),
                            rule: "es.export.wildcard",
                            ..found
                        });
                    }
                }
            }
            match hits.len() {
                0 => {}
                1 => return Ok(hits.remove(0)),
                n => {
                    return Err(Miss::new(
                        "es.export.wildcard",
                        format!("`{name}` is re-exported from {n} modules; ambiguous"),
                    ))
                }
            }
        }
        if file.commonjs {
            if let Some(found) = self.top_symbol(fi, name, "js.cjs.name-match") {
                return Ok(Found {
                    confidence: Confidence::Probable,
                    ..found
                });
            }
        }
        Err(Miss::new(
            "es.export.not-found",
            format!("`{}` has no export named `{name}`", file.path),
        ))
    }

    /// Resolve a module-level attribute of a Python module.
    pub(crate) fn py_attr(&self, fi: usize, name: &str, depth: u32) -> Res {
        self.memoized(fi, name, || self.py_attr_uncached(fi, name, depth))
    }

    fn py_attr_uncached(&self, fi: usize, name: &str, depth: u32) -> Res {
        if depth > MAX_CHAIN {
            return Err(Miss::new(
                "py.attr.chain",
                "import chain is cyclic or too deep",
            ));
        }
        let file = &self.files[fi];
        if let Some(found) = self.top_symbol(fi, name, "py.module-attribute") {
            return Ok(found);
        }
        if let Some(&imp) = file.bindings.get(&(None, name)) {
            return self.binding(fi, imp, depth + 1).map(|f| Found {
                rule: "py.reexport",
                ..f
            });
        }
        if let Some(found) = self.py_wildcard(fi, name, depth + 1) {
            return Ok(found);
        }
        if paths::file_name(file.path).starts_with("__init__.") {
            if let Some(sub) = self.modules.py_file_at(file.dir, name) {
                return Ok(Found {
                    target: Target::File(sub),
                    confidence: Confidence::Confirmed,
                    rule: "py.import.submodule",
                });
            }
        }
        Err(Miss::new(
            "py.module-attribute",
            format!("`{}` defines no module-level name `{name}`", file.path),
        ))
    }

    /// A name possibly provided by `from m import *`. Always at most `Probable`.
    fn py_wildcard(&self, fi: usize, name: &str, depth: u32) -> Option<Found> {
        if depth > MAX_CHAIN {
            return None;
        }
        let ir = self.files[fi].ir;
        for imp in ir
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Wildcard && i.scope.is_none())
        {
            if let Ok(Found {
                target: Target::File(tf),
                ..
            }) = self.py_module(fi, &imp.module, imp.level)
            {
                if tf == fi {
                    continue;
                }
                if let Ok(found) = self.py_attr(tf, name, depth + 1) {
                    return Some(Found {
                        confidence: found.confidence.min(Confidence::Probable),
                        rule: "py.import.wildcard",
                        ..found
                    });
                }
            }
        }
        None
    }

    /// `import a.b.c` followed by `a.b.c.f()`: match the longest imported
    /// module prefix of the path.
    fn py_dotted_import(&self, fi: usize, scope: Option<u32>, path: &[String]) -> Option<Res> {
        let ir = self.files[fi].ir;
        let loaded: Vec<&str> = ir
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Namespace && i.scope == scope && i.level == 0)
            .filter(|i| i.local.as_deref() == Some(i.module.as_str()) && i.module.contains('.'))
            .filter(|i| i.module.split('.').next() == Some(path[0].as_str()))
            .map(|i| i.module.as_str())
            .collect();
        if loaded.is_empty() {
            return None;
        }
        for k in (1..path.len()).rev() {
            let prefix = path[..k].join(".");
            let is_loaded = loaded.iter().any(|m| {
                *m == prefix
                    || (m.len() > prefix.len()
                        && m.starts_with(&prefix)
                        && m.as_bytes()[prefix.len()] == b'.')
            });
            if !is_loaded {
                continue;
            }
            match self.py_module(fi, &prefix, 0) {
                Ok(
                    found @ Found {
                        target: Target::File(_),
                        ..
                    },
                ) => return Some(self.descend(found, &path[k..])),
                Ok(external) => return Some(Ok(external)),
                Err(_) => continue,
            }
        }
        // The package is imported but none of its modules is in the repository.
        Some(self.py_module(fi, loaded[0], 0))
    }
}
