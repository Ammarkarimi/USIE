//! Rust resolution rules.
//!
//! Crates come from `Cargo.toml` packages (`ProjectConfig::rust_crates`, or
//! inferred from `Cargo.toml` paths). Module paths follow the standard file
//! layout: `src/lib.rs` (or `src/main.rs` without a library) is the crate
//! root, `src/a.rs` and `src/a/mod.rs` are `crate::a`, `src/a/b.rs` is
//! `crate::a::b`; `src/main.rs` next to a library, `src/bin/*`, `tests/*`,
//! `examples/*`, `benches/*` and `build.rs` are separate crate roots that
//! refer to the library by its package name. `#[path]` attributes and
//! `mod` declarations without a matching file are not modelled.
//!
//! Names in a function resolve through locals, then the items, `use`
//! bindings and glob imports of the innermost enclosing module (Rust does
//! not look into outer modules), then submodules, then extern crates (other
//! packages of the workspace, declared dependencies, `std`/`core`/`alloc`),
//! then the prelude. Methods are found in every `impl` block of the
//! type in the repository; trait impls make the trait's provided methods
//! members of the type.

use crate::builtins;
use crate::golang::local_type;
use crate::model::{Evidence, ProjectConfig};
use crate::{paths, FileIx, Found, Miss, Res, Resolver, SymRef, Target};
use std::collections::{HashMap, HashSet};
use usie_ir::{Confidence, ImportKind, LanguageFamily, SymbolKind};

struct Crate {
    /// Package name with `-` replaced by `_`; `None` for binaries, tests
    /// and examples, which nothing else can name.
    name: Option<String>,
    root: Option<usize>,
}

pub(crate) struct RustIndex {
    crates: Vec<Crate>,
    /// File → (crate, module path).
    module_of: HashMap<usize, (usize, Vec<String>)>,
    modules: HashMap<(usize, String), usize>,
    lib_by_name: HashMap<String, usize>,
    deps: HashSet<String>,
    /// Methods of a type, from all its `impl` blocks.
    methods: HashMap<SymRef, Vec<SymRef>>,
    /// `impl Trait for Type` relationships for IMPLEMENTS edges.
    impls: Vec<(SymRef, Found, Evidence)>,
}

/// A module: a file or an inline `mod` block.
#[derive(Clone, Copy)]
pub(crate) enum ModRef {
    File(usize),
    Ns(usize, u32),
}

const STD_CRATES: &[&str] = &["alloc", "core", "proc_macro", "std", "test"];

impl RustIndex {
    pub(crate) fn new(config: &ProjectConfig, files: &[FileIx<'_>]) -> Self {
        let mut index = RustIndex {
            crates: Vec::new(),
            module_of: HashMap::new(),
            modules: HashMap::new(),
            lib_by_name: HashMap::new(),
            deps: config
                .dependencies
                .iter()
                .filter(|d| d.ecosystem == "cargo")
                .map(|d| d.name.replace('-', "_"))
                .collect(),
            methods: HashMap::new(),
            impls: Vec::new(),
        };
        let mut packages: Vec<(String, String)> = config
            .rust_crates
            .iter()
            .map(|c| {
                (
                    c.dir.trim_matches('/').to_string(),
                    c.name.replace('-', "_"),
                )
            })
            .collect();
        if packages.is_empty() {
            // Without manifest facts, a directory with Cargo.toml is a
            // package named after the directory.
            for p in &config.paths {
                if paths::file_name(p) == "Cargo.toml" {
                    let dir = paths::dir_of(p).to_string();
                    let name = paths::file_name(if dir.is_empty() { "crate" } else { &dir })
                        .replace('-', "_");
                    packages.push((dir, name));
                }
            }
        }
        packages.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.cmp(b)));
        let all: HashSet<&str> = config.paths.iter().map(String::as_str).collect();
        let has = |p: &str| all.contains(p) || files.iter().any(|f| f.path == p);
        let package_crate: Vec<usize> = packages
            .iter()
            .map(|(_, name)| {
                index.crates.push(Crate {
                    name: Some(name.clone()),
                    root: None,
                });
                index.crates.len() - 1
            })
            .collect();
        for (fi, f) in files.iter().enumerate() {
            if f.family != LanguageFamily::Rust {
                continue;
            }
            let pkg = packages
                .iter()
                .position(|(dir, _)| paths::is_under(f.path, dir));
            let (crate_id, rel) = match pkg {
                Some(p) => (package_crate[p], paths::strip_dir(f.path, &packages[p].0)),
                None => (usize::MAX, f.path),
            };
            let in_pkg = |r: &str| match pkg {
                Some(p) => has(&paths::join(&packages[p].0, r).unwrap_or_default()),
                None => false,
            };
            let has_lib = in_pkg("src/lib.rs");
            let module: Option<Vec<String>> = if crate_id == usize::MAX {
                None
            } else if rel == "src/lib.rs" || (rel == "src/main.rs" && !has_lib) {
                Some(Vec::new())
            } else if rel == "src/main.rs" || rel.starts_with("src/bin/") {
                None
            } else if let Some(sub) = rel.strip_prefix("src/") {
                let stem = sub.strip_suffix(".rs").unwrap_or(sub);
                let stem = stem.strip_suffix("/mod").unwrap_or(stem);
                Some(stem.split('/').map(str::to_string).collect())
            } else {
                None
            };
            match module {
                Some(path) => {
                    if path.is_empty() {
                        index.crates[crate_id].root = Some(fi);
                    }
                    index.modules.insert((crate_id, path.join("::")), fi);
                    index.module_of.insert(fi, (crate_id, path));
                }
                None => {
                    // A separate crate root (binary, test, example, build
                    // script, or a file outside any package).
                    index.crates.push(Crate {
                        name: None,
                        root: Some(fi),
                    });
                    let id = index.crates.len() - 1;
                    index.modules.insert((id, String::new()), fi);
                    index.module_of.insert(fi, (id, Vec::new()));
                }
            }
        }
        for (i, c) in index.crates.iter().enumerate() {
            if let (Some(name), Some(_)) = (&c.name, c.root) {
                index.lib_by_name.entry(name.clone()).or_insert(i);
            }
        }
        index
    }
}

impl<'a> Resolver<'a> {
    fn rust_module_file(&self, crate_id: usize, path: &[String]) -> Option<usize> {
        self.rust.modules.get(&(crate_id, path.join("::"))).copied()
    }

    /// The innermost module enclosing `scope` in file `fi`.
    fn rust_enclosing_module(&self, fi: usize, scope: Option<u32>) -> ModRef {
        let ir = self.files[fi].ir;
        let mut cur = scope;
        while let Some(id) = cur {
            if ir.symbols[id as usize].kind == SymbolKind::Namespace {
                return ModRef::Ns(fi, id);
            }
            cur = ir.symbols[id as usize].parent;
        }
        ModRef::File(fi)
    }

    fn rust_parent_module(&self, m: ModRef) -> Option<ModRef> {
        match m {
            ModRef::Ns(fi, id) => {
                let parent = self.files[fi].ir.symbols[id as usize].parent;
                Some(self.rust_enclosing_module(fi, parent))
            }
            ModRef::File(fi) => {
                let (c, path) = self.rust.module_of.get(&fi)?;
                if path.is_empty() {
                    return None;
                }
                self.rust_module_file(*c, &path[..path.len() - 1])
                    .map(ModRef::File)
            }
        }
    }

    fn mod_found(&self, m: ModRef, rule: &'static str) -> Found {
        Found {
            target: match m {
                ModRef::File(f) => Target::File(f),
                ModRef::Ns(f, id) => Target::Symbol(f, id),
            },
            confidence: Confidence::Confirmed,
            rule,
        }
    }

    /// `std`, `core`, `alloc` and the other toolchain crates.
    fn std_external(&self) -> Found {
        self.external(
            "rust",
            "std",
            &[("stdlib", true)],
            Confidence::Confirmed,
            "rust.path.std",
        )
    }

    /// An item, `use` binding, submodule or glob import named `name` in
    /// module `m`.
    pub(crate) fn rust_module_member(&self, m: ModRef, name: &str) -> Res {
        let (fi, scope) = match m {
            ModRef::File(f) => (f, None),
            ModRef::Ns(f, id) => (f, Some(id)),
        };
        let key = format!(
            "\u{0}rust-member:{}:{name}",
            scope.map(|s| s as i64).unwrap_or(-1)
        );
        self.memoized(fi, &key, || self.rust_module_member_uncached(m, name))
    }

    fn rust_module_member_uncached(&self, m: ModRef, name: &str) -> Res {
        let (fi, scope) = match m {
            ModRef::File(f) => (f, None),
            ModRef::Ns(f, id) => (f, Some(id)),
        };
        let file = &self.files[fi];
        let ir = file.ir;
        // Items declared in the module.
        let item = match scope {
            Some(ns) => self.child_named(fi, ns, name),
            None => file.top.get(name).and_then(|ids| {
                ids.iter()
                    .copied()
                    .find(|id| !ir.symbols[*id as usize].attrs.contains_key("receiver_type"))
            }),
        };
        if let Some(id) = item {
            return Ok(Found {
                target: Target::Symbol(fi, id),
                confidence: Confidence::Confirmed,
                rule: "rust.path.item",
            });
        }
        if let Some(&imp) = file.bindings.get(&(scope, name)) {
            return self.rust_import(fi, imp);
        }
        // A file submodule.
        if let ModRef::File(f) = m {
            if let Some((c, path)) = self.rust.module_of.get(&f) {
                let mut sub = path.clone();
                sub.push(name.to_string());
                if let Some(sf) = self.rust_module_file(*c, &sub) {
                    return Ok(Found {
                        target: Target::File(sf),
                        confidence: Confidence::Confirmed,
                        rule: "rust.path.module",
                    });
                }
            }
        }
        // Glob imports.
        let mut hits: Vec<Found> = Vec::new();
        for (idx, imp) in ir.imports.iter().enumerate() {
            if imp.kind != ImportKind::Wildcard || imp.scope != scope {
                continue;
            }
            let Ok(base) = self.rust_glob_target(fi, idx) else {
                continue;
            };
            let found = match base.target {
                Target::File(tf) => self.rust_module_member(ModRef::File(tf), name).ok(),
                Target::Symbol(tf, tid) => match self.sym((tf, tid)).kind {
                    SymbolKind::Namespace => {
                        self.rust_module_member(ModRef::Ns(tf, tid), name).ok()
                    }
                    // `use Enum::*`
                    _ => self.member((tf, tid), name, 0).map(|(r, _)| Found {
                        target: Target::Symbol(r.0, r.1),
                        confidence: Confidence::Confirmed,
                        rule: "rust.use.glob",
                    }),
                },
                _ => None,
            };
            if let Some(f) = found {
                if !hits.iter().any(|h| h.target == f.target) {
                    hits.push(Found {
                        confidence: f.confidence.min(Confidence::Probable),
                        rule: "rust.use.glob",
                        ..f
                    });
                }
            }
        }
        match hits.len() {
            1 => Ok(hits.remove(0)),
            0 => Err(Miss::new(
                "rust.path.item",
                format!("module `{}` has no item `{name}`", self.rust_module_name(m)),
            )),
            n => Err(Miss::new(
                "rust.use.glob",
                format!("`{name}` is provided by {n} glob imports; ambiguous"),
            )),
        }
    }

    fn rust_module_name(&self, m: ModRef) -> String {
        match m {
            ModRef::Ns(f, id) => self.files[f].ir.symbols[id as usize].qualified_name.clone(),
            ModRef::File(f) => match self.rust.module_of.get(&f) {
                Some((_, p)) if !p.is_empty() => format!("crate::{}", p.join("::")),
                _ => "crate".into(),
            },
        }
    }

    fn rust_glob_target(&self, fi: usize, idx: usize) -> Res {
        let imp = &self.files[fi].ir.imports[idx];
        let path: Vec<String> = imp.module.split("::").map(str::to_string).collect();
        let key = format!("\u{0}rust-glob:{idx}");
        self.memoized(fi, &key, || self.rust_path(fi, imp.scope, &path, true))
    }

    /// Resolve a `use` of the import at `idx` (a named leaf or a glob).
    pub(crate) fn rust_import(&self, fi: usize, idx: usize) -> Res {
        let imp = &self.files[fi].ir.imports[idx];
        if imp.kind == ImportKind::Wildcard {
            return self.rust_glob_target(fi, idx);
        }
        let mut path: Vec<String> = if imp.module.is_empty() {
            Vec::new()
        } else {
            imp.module.split("::").map(str::to_string).collect()
        };
        path.push(imp.imported.clone().unwrap_or_default());
        let key = format!("\u{0}rust-use:{idx}");
        self.memoized(fi, &key, || self.rust_path(fi, imp.scope, &path, true))
    }

    /// Resolve a path as seen from `scope` of file `fi`. `use_path` paths do
    /// not see locals.
    fn rust_path(&self, fi: usize, scope: Option<u32>, path: &[String], use_path: bool) -> Res {
        let first = path[0].as_str();
        let here = self.rust_enclosing_module(fi, scope);
        let start = match first {
            "crate" => {
                let root = self
                    .rust
                    .module_of
                    .get(&fi)
                    .and_then(|(c, _)| self.rust.crates[*c].root);
                match root {
                    Some(r) => Some((self.mod_found(ModRef::File(r), "rust.path.crate"), 1)),
                    None => {
                        return Err(Miss::new(
                            "rust.path.crate",
                            "the crate root of this file is not known",
                        ))
                    }
                }
            }
            "mod:self" | "self" if use_path || first == "mod:self" => {
                Some((self.mod_found(here, "rust.path.self"), 1))
            }
            "super" => {
                let mut m = here;
                let mut i = 0;
                while path.get(i).map(String::as_str) == Some("super") {
                    m = match self.rust_parent_module(m) {
                        Some(p) => p,
                        None => {
                            return Err(Miss::new(
                                "rust.path.super",
                                "`super` goes above the crate root",
                            ))
                        }
                    };
                    i += 1;
                }
                Some((self.mod_found(m, "rust.path.super"), i))
            }
            _ => None,
        };
        if let Some((base, consumed)) = start {
            return self.descend(base, &path[consumed..]);
        }
        if !use_path {
            // Locals of enclosing functions.
            let ir = self.files[fi].ir;
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
                    if let Some(ty) = local_type(sym, first) {
                        return self.typed_value(
                            fi,
                            Some(id),
                            first,
                            &ty,
                            &path[1..],
                            "rust.typed-value",
                        );
                    }
                    if let Some(res) =
                        self.call_result_value(fi, id, first, &path[1..], "rust.call-result")
                    {
                        return res;
                    }
                    return Err(Miss::receiver(
                        "scope.local-binding",
                        format!(
                            "`{first}` is a local binding of `{}` whose type is not declared; its value is not tracked",
                            sym.qualified_name
                        ),
                    ));
                }
                cur = sym.parent;
            }
        }
        // The innermost module.
        match self.rust_module_member(here, first) {
            Ok(found) => return self.descend(found, &path[1..]),
            Err(miss) if miss.rule == "rust.use.glob" => return Err(miss),
            Err(_) => {}
        }
        // Extern crates.
        if let Some(&c) = self.rust.lib_by_name.get(first) {
            let own = self.rust.module_of.get(&fi).map(|(c, _)| *c);
            if own != Some(c) {
                if let Some(root) = self.rust.crates[c].root {
                    let base = Found {
                        target: Target::File(root),
                        confidence: Confidence::Confirmed,
                        rule: "rust.path.workspace-crate",
                    };
                    return self.descend(base, &path[1..]);
                }
            }
        }
        if STD_CRATES.contains(&first) {
            return Ok(self.std_external());
        }
        if self.rust.deps.contains(first) {
            return Ok(self.external(
                "cargo",
                first,
                &[("declared", true)],
                Confidence::Confirmed,
                "rust.path.declared-crate",
            ));
        }
        if builtins::is_rust_prelude(first) {
            return Err(Miss {
                rule: "builtin",
                reason: String::new(),
                builtin: true,
                unknown_receiver: false,
            });
        }
        // A lower-case first segment followed by more segments is most
        // likely a crate that is not declared in a manifest the analysis saw.
        if path.len() > 1
            && first
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
        {
            return Ok(self.external(
                "cargo",
                first,
                &[("declared", false)],
                Confidence::Probable,
                "rust.path.undeclared-crate",
            ));
        }
        Err(Miss::new(
            "scope.unbound",
            format!(
                "no item, `use` or crate named `{first}` is visible in `{}`",
                self.rust_module_name(here)
            ),
        ))
    }

    pub(crate) fn rust_lookup(&self, fi: usize, scope: Option<u32>, path: &[String]) -> Res {
        self.rust_path(fi, scope, path, false)
    }

    /// A method declared in any `impl` block of the type.
    pub(crate) fn rust_method(&self, class: SymRef, name: &str) -> Option<SymRef> {
        self.rust
            .methods
            .get(&class)?
            .iter()
            .copied()
            .find(|m| self.sym(*m).name == name)
    }

    /// Attach `impl` methods to their types and traits to their
    /// implementors. Runs once, before resolution.
    pub(crate) fn rust_bind_impls(&mut self) {
        let mut methods: HashMap<SymRef, Vec<SymRef>> = HashMap::new();
        let mut impls: Vec<(SymRef, Found, Evidence)> = Vec::new();
        let mut seen: HashSet<(SymRef, String)> = HashSet::new();
        for fi in 0..self.files.len() {
            if self.files[fi].family != LanguageFamily::Rust {
                continue;
            }
            for sym in &self.files[fi].ir.symbols {
                let Some(ty) = sym.attrs.get("impl_type").and_then(|v| v.as_array()) else {
                    continue;
                };
                let ty: Vec<String> = ty
                    .iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect();
                if ty.is_empty() {
                    continue;
                }
                let Ok(Found {
                    target: Target::Symbol(tf, tid),
                    ..
                }) = self.rust_path(fi, sym.parent, &ty, true)
                else {
                    continue;
                };
                if !matches!(
                    self.sym((tf, tid)).kind,
                    SymbolKind::Class | SymbolKind::Interface | SymbolKind::Type
                ) {
                    continue;
                }
                methods.entry((tf, tid)).or_default().push((fi, sym.id));
                let Some(tr) = sym.attrs.get("impl_trait").and_then(|v| v.as_array()) else {
                    continue;
                };
                let tr: Vec<String> = tr
                    .iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect();
                if !seen.insert(((tf, tid), tr.join("::"))) {
                    continue;
                }
                if let Ok(found) = self.rust_path(fi, sym.parent, &tr, true) {
                    let evidence = Evidence {
                        path: self.files[fi].path.to_string(),
                        line: sym.span.start_line,
                        col: sym.span.start_col,
                    };
                    impls.push(((tf, tid), found, evidence));
                }
            }
        }
        for ((tf, tid), found, _) in &impls {
            if let Target::Symbol(rf, rid) = found.target {
                if self.sym((rf, rid)).kind == SymbolKind::Interface {
                    self.bases
                        .borrow_mut()
                        .entry((*tf, *tid))
                        .or_default()
                        .push((rf, rid));
                }
            }
        }
        self.rust.methods = methods;
        self.rust.impls = impls;
        // Lookups made while binding must not be reused: method tables were
        // empty during them.
        self.surface_memo.borrow_mut().clear();
    }

    /// IMPLEMENTS edges for `impl Trait for Type`.
    pub(crate) fn rust_impl_edges(&mut self) {
        let impls = std::mem::take(&mut self.rust.impls);
        for ((tf, tid), found, evidence) in &impls {
            let src = self.files[*tf].keys[*tid as usize].clone();
            let dst = self.target_key(&found.target);
            self.edge(
                src,
                dst,
                "IMPLEMENTS",
                found.confidence,
                "rust.impl.trait",
                evidence.clone(),
            );
        }
        self.rust.impls = impls;
    }
}
