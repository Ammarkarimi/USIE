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
pub mod model;
mod modules;
mod paths;

pub use model::*;

use modules::ModuleIndex;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
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

type SymRef = (usize, u32);

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    Symbol(usize, u32),
    File(usize),
    /// A repository file that is not parsed source (stylesheet, JSON, …).
    Asset(String),
    External(String),
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
    fn receiver(rule: &'static str, reason: impl Into<String>) -> Miss {
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
    methods_by_name: HashMap<(LanguageFamily, &'a str), Vec<SymRef>>,
    bases: RefCell<HashMap<SymRef, Vec<SymRef>>>,
    /// Memoized module-surface lookups, keyed by (file, name). `None` marks a
    /// lookup in progress, which is how import cycles are detected.
    surface_memo: RefCell<HashMap<(usize, String), Option<Res>>>,
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
        Resolver {
            files,
            modules,
            methods_by_name,
            bases: RefCell::new(HashMap::new()),
            surface_memo: RefCell::new(HashMap::new()),
            externals: RefCell::new(BTreeMap::new()),
            edges: BTreeMap::new(),
            refs: Vec::new(),
        }
    }

    fn run(mut self) -> Resolution {
        let mut nodes = Vec::new();
        for fi in 0..self.files.len() {
            self.structure(fi, &mut nodes);
        }
        for fi in 0..self.files.len() {
            self.heritage(fi);
        }
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
                    if let Ok(Found {
                        target: Target::Symbol(tf, tid),
                        ..
                    }) = &res
                    {
                        let target_kind = self.files[*tf].ir.symbols[*tid as usize].kind;
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
            let text = format!("{}{}", ".".repeat(imp.level as usize), imp.module);
            let res = match family {
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
            let mut res = if r.path.is_empty() {
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
                    LanguageFamily::Python => Err(Miss::new(
                        "callee.module",
                        "a module object is not callable",
                    )),
                };
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
                        rule: "es.call.super-constructor",
                        ..
                    }),
                ) => "CALLS",
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
        // Prefer callables so `Class.method` wins over a same-named field.
        kids.iter()
            .copied()
            .find(|k| {
                ir.symbols[*k as usize].name == name && ir.symbols[*k as usize].kind.is_callable()
            })
            .or_else(|| {
                kids.iter()
                    .copied()
                    .find(|k| ir.symbols[*k as usize].name == name)
            })
    }

    /// A member of a class or interface, searching resolved base classes.
    /// The flag reports whether the member was inherited.
    fn member(&self, class: SymRef, name: &str, depth: u32) -> Option<(SymRef, bool)> {
        if let Some(id) = self.child_named(class.0, class.1, name) {
            return Some(((class.0, id), false));
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
        for seg in rest {
            cur = match cur.target {
                Target::Symbol(f, id) => {
                    let sym = self.sym((f, id));
                    if !matches!(
                        sym.kind,
                        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Namespace
                    ) {
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
                            return Err(Miss::new(
                                "member.not-found",
                                format!(
                                    "`{}` has no statically known member `{seg}`",
                                    sym.qualified_name
                                ),
                            ))
                        }
                    }
                }
                Target::File(tf) => {
                    let found = match self.files[tf].family {
                        LanguageFamily::Ecmascript => self.es_export(tf, seg, 0)?,
                        LanguageFamily::Python => self.py_attr(tf, seg, 0)?,
                    };
                    Found {
                        confidence: found.confidence.min(cur.confidence),
                        rule: cur.rule,
                        ..found
                    }
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
    fn memoized(&self, fi: usize, name: &str, compute: impl FnOnce() -> Res) -> Res {
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
