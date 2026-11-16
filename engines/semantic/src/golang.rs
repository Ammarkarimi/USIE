//! Go resolution rules.
//!
//! A Go package is the set of files in one directory that share a `package`
//! clause. Package-level names are visible in every file of the package;
//! imports bind a package name per file. Import paths are mapped to
//! directories through the `module` paths of the repository's `go.mod`
//! files, then `vendor/`, then the standard library and declared modules.

use crate::builtins;
use crate::model::ProjectConfig;
use crate::paths;
use crate::{FileIx, Found, Miss, Res, Resolver, SymRef, Target};
use std::collections::HashMap;
use usie_ir::{Confidence, ImportKind, LanguageFamily, SymbolKind};

pub(crate) struct GoPackage {
    pub dir: String,
    pub name: String,
}

/// Where an import path leads, before any graph node is created.
enum ImportDest {
    Repo(usize, &'static str),
    Std,
    Module(String, bool),
    Missing(String),
}

pub(crate) struct GoIndex {
    pub packages: Vec<GoPackage>,
    pkg_of: HashMap<usize, usize>,
    by_dir: HashMap<String, Vec<usize>>,
    /// Package-level declarations (not methods) by (package, name).
    scope: HashMap<(usize, String), Vec<SymRef>>,
    /// Methods by (package, receiver type name).
    methods: HashMap<(usize, String), Vec<SymRef>>,
    /// Repository modules, longest path first.
    modules: Vec<(String, String)>,
    /// Declared module requirements, longest first.
    deps: Vec<String>,
    /// Per file: import binding names → (import index, confidence of the name).
    names: HashMap<usize, HashMap<String, (usize, Confidence)>>,
}

impl GoIndex {
    pub(crate) fn new(config: &ProjectConfig, files: &[FileIx<'_>]) -> Self {
        let mut index = GoIndex {
            packages: Vec::new(),
            pkg_of: HashMap::new(),
            by_dir: HashMap::new(),
            scope: HashMap::new(),
            methods: HashMap::new(),
            modules: config
                .go_modules
                .iter()
                .map(|m| (m.dir.trim_matches('/').to_string(), m.path.clone()))
                .collect(),
            deps: config
                .dependencies
                .iter()
                .filter(|d| d.ecosystem == "go")
                .map(|d| d.name.clone())
                .collect(),
            names: HashMap::new(),
        };
        index
            .modules
            .sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.cmp(b)));
        index
            .deps
            .sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        let mut pkg_ids: HashMap<(String, String), usize> = HashMap::new();
        for (fi, f) in files.iter().enumerate() {
            if f.family != LanguageFamily::Go {
                continue;
            }
            let name =
                f.ir.attrs
                    .get("package")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            let key = (f.dir.to_string(), name.clone());
            let pkg = *pkg_ids.entry(key).or_insert_with(|| {
                index.packages.push(GoPackage {
                    dir: f.dir.to_string(),
                    name,
                });
                let id = index.packages.len() - 1;
                index.by_dir.entry(f.dir.to_string()).or_default().push(id);
                id
            });
            index.pkg_of.insert(fi, pkg);
            for sym in &f.ir.symbols {
                if sym.parent.is_some() {
                    continue;
                }
                match sym.attrs.get("receiver_type").and_then(|v| v.as_str()) {
                    Some(recv) => index
                        .methods
                        .entry((pkg, recv.to_string()))
                        .or_default()
                        .push((fi, sym.id)),
                    None => index
                        .scope
                        .entry((pkg, sym.name.clone()))
                        .or_default()
                        .push((fi, sym.id)),
                }
            }
        }
        index
    }

    /// The importable package of a directory: not an external test package
    /// (`foo_test`) when another package shares the directory.
    fn package_in(&self, dir: &str) -> Option<usize> {
        let ids = self.by_dir.get(dir)?;
        ids.iter()
            .copied()
            .find(|p| !self.packages[*p].name.ends_with("_test"))
            .or_else(|| ids.first().copied())
    }

    pub(crate) fn pkg_of(&self, file: usize) -> Option<usize> {
        self.pkg_of.get(&file).copied()
    }
}

/// The package name an import path conventionally declares: the last
/// element without a major-version suffix (`/v2`, `.v3`) or `go-` prefix.
fn guess_package_name(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 1 {
        if let Some(last) = parts.last() {
            if last.len() > 1
                && last.starts_with('v')
                && last[1..].bytes().all(|b| b.is_ascii_digit())
            {
                parts.pop();
            }
        }
    }
    let mut name = parts.last().copied().unwrap_or(path);
    if let Some((stem, version)) = name.rsplit_once(".v") {
        if !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()) {
            name = stem;
        }
    }
    let name = name.strip_prefix("go-").unwrap_or(name);
    let name = name
        .strip_suffix(".go")
        .or_else(|| name.strip_suffix("-go"))
        .unwrap_or(name);
    name.replace(['-', '.'], "_")
}

/// Names an external package may declare: the conventional name, and for
/// dashed or dotted names (`anthropic-sdk-go`) each of their parts.
fn package_name_candidates(path: &str) -> Vec<String> {
    let main = guess_package_name(path);
    let mut out = vec![main.clone()];
    if main.contains('_') {
        for part in main.split('_') {
            if !part.is_empty() && part != "go" && !out.iter().any(|o| o == part) {
                out.push(part.to_string());
            }
        }
    }
    out
}

/// The module an undeclared import path most likely belongs to.
fn guess_module_root(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let n = match parts[0] {
        "github.com" | "gitlab.com" | "bitbucket.org" | "golang.org" => 3,
        _ => 2,
    };
    parts[..n.min(parts.len())].join("/")
}

impl<'a> Resolver<'a> {
    fn go_dest(&self, from: usize, path: &str) -> ImportDest {
        let go = &self.go;
        for (dir, module) in &go.modules {
            let rest = if path == module {
                Some("")
            } else {
                path.strip_prefix(module.as_str())
                    .and_then(|r| r.strip_prefix('/'))
            };
            let Some(rest) = rest else { continue };
            let target = paths::join(dir, rest).unwrap_or_default();
            return match go.package_in(&target) {
                Some(p) => ImportDest::Repo(p, "go.import.module"),
                None => ImportDest::Missing(format!(
                    "`{path}` is in module `{module}` but directory `{}` has no Go package",
                    if target.is_empty() { "." } else { &target }
                )),
            };
        }
        // Vendored copies, searched from the importing directory upwards.
        let mut dir = Some(self.files[from].dir);
        while let Some(d) = dir {
            if let Some(vendored) = paths::join(d, &format!("vendor/{path}")) {
                if let Some(p) = go.package_in(&vendored) {
                    return ImportDest::Repo(p, "go.import.vendor");
                }
            }
            dir = paths::parent(d);
        }
        let first = path.split('/').next().unwrap_or("");
        if !first.contains('.') {
            // The standard library's import paths have no dot in the first
            // element; module paths must have one.
            return ImportDest::Std;
        }
        for dep in &go.deps {
            if path == dep
                || (path.starts_with(dep.as_str()) && path.as_bytes().get(dep.len()) == Some(&b'/'))
            {
                return ImportDest::Module(dep.clone(), true);
            }
        }
        ImportDest::Module(guess_module_root(path), false)
    }

    /// Resolve the import at index `idx` of Go file `fi`.
    pub(crate) fn go_import(&self, fi: usize, idx: usize) -> Res {
        let path = self.files[fi].ir.imports[idx].module.as_str();
        match self.go_dest(fi, path) {
            ImportDest::Repo(p, rule) => Ok(Found {
                target: Target::GoPackage(p),
                confidence: Confidence::Confirmed,
                rule,
            }),
            ImportDest::Std => Ok(self.external(
                "go",
                path,
                &[("stdlib", true)],
                Confidence::Confirmed,
                "go.import.stdlib",
            )),
            ImportDest::Module(module, true) => Ok(self.external(
                "go",
                &module,
                &[("declared", true)],
                Confidence::Confirmed,
                "go.import.declared-module",
            )),
            ImportDest::Module(module, false) => Ok(self.external(
                "go",
                &module,
                &[("declared", false)],
                // No go.mod requirement: the module boundary is a guess.
                Confidence::Probable,
                "go.import.undeclared-module",
            )),
            ImportDest::Missing(reason) => Err(Miss::new("go.import.module", reason)),
        }
    }

    /// Compute each Go file's import binding names. Needs the package index.
    pub(crate) fn go_bind_imports(&mut self) {
        let mut all = HashMap::new();
        for fi in 0..self.files.len() {
            if self.files[fi].family != LanguageFamily::Go {
                continue;
            }
            let mut names = HashMap::new();
            let mut guessed: Vec<(String, usize)> = Vec::new();
            for (idx, imp) in self.files[fi].ir.imports.iter().enumerate() {
                if imp.kind != ImportKind::Namespace {
                    continue;
                }
                let (name, confidence) = match &imp.local {
                    Some(alias) => (alias.clone(), Confidence::Confirmed),
                    None => match self.go_dest(fi, &imp.module) {
                        ImportDest::Repo(p, _) => {
                            (self.go.packages[p].name.clone(), Confidence::Confirmed)
                        }
                        ImportDest::Std => (
                            imp.module.rsplit('/').next().unwrap_or("").to_string(),
                            Confidence::Confirmed,
                        ),
                        _ => {
                            // The real name is the package clause of code
                            // outside the repository: try the conventional
                            // forms, without displacing any other binding.
                            for candidate in package_name_candidates(&imp.module) {
                                guessed.push((candidate, idx));
                            }
                            continue;
                        }
                    },
                };
                names.insert(name, (idx, confidence));
            }
            for (name, idx) in guessed {
                names.entry(name).or_insert((idx, Confidence::Probable));
            }
            all.insert(fi, names);
        }
        self.go.names = all;
    }

    /// A package-level declaration. Several declarations of one name can
    /// only coexist under mutually exclusive build constraints, so any of
    /// them is a correct target for some build: `Probable`.
    pub(crate) fn go_package_symbol(&self, pkg: usize, name: &str) -> Option<Found> {
        let decls = self.go.scope.get(&(pkg, name.to_string()))?;
        let (f, id) = *decls.first()?;
        Some(Found {
            target: Target::Symbol(f, id),
            confidence: if decls.len() == 1 {
                Confidence::Confirmed
            } else {
                Confidence::Probable
            },
            rule: if decls.len() == 1 {
                "go.package-symbol"
            } else {
                "go.package-symbol.build-variant"
            },
        })
    }

    /// An exported member of an imported repository package.
    pub(crate) fn go_package_member(&self, pkg: usize, name: &str) -> Res {
        let p = &self.go.packages[pkg];
        if !name.chars().next().is_some_and(char::is_uppercase) {
            return Err(Miss::new(
                "go.package-member",
                format!("`{name}` is not exported by package `{}`", p.name),
            ));
        }
        self.go_package_symbol(pkg, name).ok_or_else(|| {
            Miss::new(
                "go.package-member",
                format!(
                    "package `{}` (`{}`) declares no `{name}`",
                    p.name,
                    if p.dir.is_empty() { "." } else { &p.dir }
                ),
            )
        })
    }

    /// A method declared with receiver type `class` anywhere in its package.
    pub(crate) fn go_method(&self, class: SymRef, name: &str) -> Option<SymRef> {
        let sym = self.sym(class);
        if sym.parent.is_some() {
            return None;
        }
        let pkg = self.go.pkg_of(class.0)?;
        self.go
            .methods
            .get(&(pkg, sym.name.clone()))?
            .iter()
            .copied()
            .find(|m| self.sym(*m).name == name)
    }

    pub(crate) fn go_lookup(&self, fi: usize, scope: Option<u32>, path: &[String]) -> Res {
        let file = &self.files[fi];
        let ir = file.ir;
        let first = path[0].as_str();

        // Function scopes, innermost first.
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
                        "go.typed-value",
                    );
                }
                if let Some(res) =
                    self.call_result_value(fi, id, first, &path[1..], "go.call-result")
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
        if file.anon_locals.contains(first) && scope.is_none() {
            return Err(Miss::receiver(
                "scope.local-binding",
                format!("`{first}` is bound inside a function literal; its value is not tracked"),
            ));
        }

        let pkg = self.go.pkg_of(fi);
        if let Some(found) = pkg.and_then(|p| self.go_package_symbol(p, first)) {
            return self.descend(found, &path[1..]);
        }
        if let Some(&(idx, name_confidence)) = self.go.names.get(&fi).and_then(|n| n.get(first)) {
            let base = self.go_import(fi, idx)?;
            let base = Found {
                confidence: base.confidence.min(name_confidence),
                ..base
            };
            if path.len() == 1 {
                return Err(Miss::new(
                    "go.package-not-value",
                    format!("`{first}` names an imported package, which is not a value"),
                ));
            }
            return self.descend(base, &path[1..]);
        }
        // Dot imports make the exported names of a package visible unqualified.
        let mut external_dot = false;
        for (idx, imp) in ir.imports.iter().enumerate() {
            if imp.kind != ImportKind::Wildcard {
                continue;
            }
            match self.go_import(fi, idx) {
                Ok(Found {
                    target: Target::GoPackage(p),
                    confidence,
                    ..
                }) => {
                    if let Ok(found) = self.go_package_member(p, first) {
                        let base = Found {
                            confidence: found.confidence.min(confidence),
                            rule: "go.import.dot",
                            ..found
                        };
                        return self.descend(base, &path[1..]);
                    }
                }
                Ok(_) => external_dot = true,
                Err(_) => {}
            }
        }
        if builtins::is_go_builtin(first) {
            return Err(Miss {
                rule: "builtin",
                reason: String::new(),
                builtin: true,
                unknown_receiver: false,
            });
        }
        Err(Miss::new(
            "scope.unbound",
            if external_dot {
                format!("no declaration of `{first}` in this package; it may come from a dot-imported package outside the repository")
            } else {
                format!("no declaration of `{first}` in this package and no import named `{first}`")
            },
        ))
    }
}

/// The declared type of a local binding, from `attrs.local_types`.
pub(crate) fn local_type(sym: &usie_ir::Symbol, name: &str) -> Option<Vec<String>> {
    let segments = sym.attrs.get("local_types")?.get(name)?.as_array()?;
    let out: Vec<String> = segments
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    (!out.is_empty()).then_some(out)
}

/// The declared type of a field or variable, from `attrs.type_path`.
pub(crate) fn declared_type(sym: &usie_ir::Symbol) -> Option<Vec<String>> {
    let segments = sym.attrs.get("type_path")?.as_array()?;
    let out: Vec<String> = segments
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    (!out.is_empty()).then_some(out)
}

pub(crate) fn is_type_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Type
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_guesses() {
        assert_eq!(guess_package_name("github.com/go-chi/chi/v5"), "chi");
        assert_eq!(guess_package_name("gopkg.in/yaml.v3"), "yaml");
        assert_eq!(guess_package_name("github.com/mattn/go-sqlite3"), "sqlite3");
        assert_eq!(guess_package_name("github.com/a/b/v2/sub"), "sub");
        assert_eq!(guess_module_root("github.com/a/b/c/d"), "github.com/a/b");
        assert_eq!(
            package_name_candidates("github.com/anthropics/anthropic-sdk-go"),
            vec!["anthropic_sdk", "anthropic", "sdk"]
        );
        assert_eq!(
            guess_module_root("google.golang.org/grpc/codes"),
            "google.golang.org/grpc"
        );
    }
}
