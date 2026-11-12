//! Module resolution: mapping import specifiers to repository files or
//! external packages.

use crate::builtins;
use crate::model::{NodeRecord, ProjectConfig, TsConfig, WorkspacePackage};
use crate::paths;
use crate::{FileIx, Found, Miss, Res, Resolver, Target};
use std::collections::{BTreeMap, HashMap, HashSet};
use usie_ir::{Confidence, LanguageFamily};

const ES_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".d.ts", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];
/// TypeScript sources are imported by their emitted JavaScript names.
const ES_OUTPUT_TO_SOURCE: &[(&str, &[&str])] = &[
    (".js", &[".ts", ".tsx", ".d.ts"]),
    (".jsx", &[".tsx"]),
    (".mjs", &[".mts"]),
    (".cjs", &[".cts"]),
];
const PY_EXTENSIONS: &[&str] = &[".py", ".pyi"];

struct PyModule {
    file: usize,
    /// Import root the module name is relative to.
    root: String,
    /// False for the repository-root fallback of a file whose real import
    /// root is elsewhere.
    proper: bool,
}

pub(crate) struct ModuleIndex<'a> {
    by_path: HashMap<&'a str, usize>,
    all_paths: HashSet<&'a str>,
    tsconfigs: &'a [TsConfig],
    packages: &'a [WorkspacePackage],
    npm_deps: HashSet<&'a str>,
    pypi_deps: HashSet<String>,
    python_roots: Vec<String>,
    py_modules: HashMap<String, Vec<PyModule>>,
    py_top_level: HashSet<String>,
    py_root_of: HashMap<usize, String>,
}

impl<'a> ModuleIndex<'a> {
    pub(crate) fn new(config: &'a ProjectConfig, files: &[FileIx<'a>]) -> Self {
        let by_path: HashMap<&str, usize> = files.iter().enumerate().map(|(i, f)| (f.path, i)).collect();
        let mut all_paths: HashSet<&str> = config.paths.iter().map(String::as_str).collect();
        all_paths.extend(files.iter().map(|f| f.path));
        let python_roots: Vec<String> = config.python_roots.iter().map(|r| r.trim_matches('/').to_string()).collect();

        let mut index = ModuleIndex {
            by_path,
            all_paths,
            tsconfigs: &config.tsconfigs,
            packages: &config.packages,
            npm_deps: config.dependencies.iter().filter(|d| d.ecosystem == "npm").map(|d| d.name.as_str()).collect(),
            pypi_deps: config.dependencies.iter().filter(|d| d.ecosystem == "pypi").map(|d| normalize_pypi(&d.name)).collect(),
            python_roots,
            py_modules: HashMap::new(),
            py_top_level: HashSet::new(),
            py_root_of: HashMap::new(),
        };
        for (i, f) in files.iter().enumerate() {
            if f.family == LanguageFamily::Python {
                index.register_python(i, f.path);
            }
        }
        index
    }

    fn register_python(&mut self, file: usize, path: &str) {
        // The import root is the parent of the outermost enclosing package.
        let mut root = paths::dir_of(path);
        while !root.is_empty() && self.is_package_dir(root) {
            root = paths::dir_of(root);
        }
        self.py_root_of.insert(file, root.to_string());
        let mut roots: Vec<(String, bool)> = vec![(root.to_string(), true)];
        for extra in &self.python_roots {
            if extra != root && paths::is_under(path, extra) {
                roots.push((extra.clone(), true));
            }
        }
        if !root.is_empty() && !self.python_roots.iter().any(String::is_empty) {
            roots.push((String::new(), false));
        }
        for (root, proper) in roots {
            let Some(name) = python_module_name(paths::strip_dir(path, &root)) else {
                continue;
            };
            if proper {
                if let Some(top) = name.split('.').next() {
                    self.py_top_level.insert(top.to_string());
                }
            }
            self.py_modules.entry(name).or_default().push(PyModule { file, root, proper });
        }
    }

    fn is_package_dir(&self, dir: &str) -> bool {
        PY_EXTENSIONS.iter().any(|ext| self.all_paths.contains(format!("{dir}/__init__{ext}").as_str()))
    }

    /// The module or package `name` located directly in `dir`.
    pub(crate) fn py_file_at(&self, dir: &str, name: &str) -> Option<usize> {
        let base = paths::join(dir, name)?;
        for ext in PY_EXTENSIONS {
            if let Some(&f) = self.by_path.get(format!("{base}{ext}").as_str()) {
                return Some(f);
            }
        }
        for ext in PY_EXTENSIONS {
            if let Some(&f) = self.by_path.get(format!("{base}/__init__{ext}").as_str()) {
                return Some(f);
            }
        }
        None
    }

    /// Resolve an absolute dotted module name to a repository file.
    /// `Err(n)` means the name is ambiguous between `n` files.
    fn py_absolute(&self, from: usize, name: &str) -> Result<Option<(usize, Confidence)>, usize> {
        let Some(candidates) = self.py_modules.get(name) else {
            return Ok(None);
        };
        let from_root = self.py_root_of.get(&from).map(String::as_str).unwrap_or("");
        let on_path = |m: &PyModule| {
            m.proper && (m.root == from_root || m.root.is_empty() || m.root == "src" || self.python_roots.contains(&m.root))
        };
        type Accept<'f> = Box<dyn Fn(&PyModule) -> bool + 'f>;
        let tiers: [(Accept<'_>, Confidence); 3] = [
            (Box::new(on_path), Confidence::Confirmed),
            (Box::new(|m: &PyModule| m.proper), Confidence::Probable),
            (Box::new(|_: &PyModule| true), Confidence::Probable),
        ];
        for (accept, confidence) in tiers {
            let mut files: Vec<usize> = candidates.iter().filter(|m| accept(m)).map(|m| m.file).collect();
            files.sort_unstable();
            files.dedup();
            match files.len() {
                0 => continue,
                1 => return Ok(Some((files[0], confidence))),
                n => return Err(n),
            }
        }
        Ok(None)
    }

    /// A repository file for an extensionless ECMAScript module path.
    fn es_file(&self, base: &str) -> Option<Target> {
        let hit = |candidate: &str| -> Option<Target> {
            if let Some(&f) = self.by_path.get(candidate) {
                return Some(Target::File(f));
            }
            self.all_paths.get(candidate).map(|p| Target::Asset((*p).to_string()))
        };
        if !base.is_empty() {
            if let Some(t) = hit(base) {
                return Some(t);
            }
            for ext in ES_EXTENSIONS {
                if let Some(t) = hit(&format!("{base}{ext}")) {
                    return Some(t);
                }
            }
            for (output, sources) in ES_OUTPUT_TO_SOURCE {
                if let Some(stem) = base.strip_suffix(output) {
                    for source in *sources {
                        if let Some(t) = hit(&format!("{stem}{source}")) {
                            return Some(t);
                        }
                    }
                }
            }
        }
        for ext in ES_EXTENSIONS {
            let candidate = if base.is_empty() { format!("index{ext}") } else { format!("{base}/index{ext}") };
            if let Some(t) = hit(&candidate) {
                return Some(t);
            }
        }
        None
    }

    fn nearest_tsconfig(&self, dir: &str) -> Option<&'a TsConfig> {
        self.tsconfigs
            .iter()
            .filter(|t| t.dir == dir || paths::is_under(dir, &t.dir))
            .max_by_key(|t| t.dir.len())
    }
}

impl<'a> Resolver<'a> {
    fn external(&self, ecosystem: &str, name: &str, attrs: &[(&str, bool)], confidence: Confidence, rule: &'static str) -> Found {
        let key = format!("ext:{ecosystem}:{name}");
        self.externals.borrow_mut().entry(key.clone()).or_insert_with(|| {
            let mut map = BTreeMap::new();
            map.insert("ecosystem".to_string(), ecosystem.into());
            for (k, v) in attrs {
                map.insert((*k).to_string(), (*v).into());
            }
            NodeRecord {
                key: key.clone(),
                kind: "package".into(),
                name: name.to_string(),
                qualified_name: None,
                path: None,
                span: None,
                signature: None,
                doc: None,
                exported: false,
                attrs: map,
            }
        });
        Found { target: Target::External(key), confidence, rule }
    }

    /// Resolve an ECMAScript import specifier.
    pub(crate) fn es_module(&self, from: usize, spec: &str) -> Res {
        let m = &self.modules;
        let dir = self.files[from].dir;
        if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
            let Some(base) = paths::join(dir, spec) else {
                return Err(Miss::new("es.import.relative", format!("`{spec}` points outside the repository")));
            };
            return match m.es_file(&base) {
                Some(target) => Ok(Found { target, confidence: Confidence::Confirmed, rule: "es.import.relative" }),
                None => Err(Miss::new("es.import.relative", format!("no file in the repository matches `{spec}`"))),
            };
        }
        if spec.starts_with('/') {
            return Err(Miss::new("es.import.absolute", format!("absolute specifier `{spec}` depends on the host or bundler")));
        }
        if let Some(name) = spec.strip_prefix("node:") {
            let name = name.split('/').next().unwrap_or(name);
            return Ok(self.external("node", name, &[("builtin", true)], Confidence::Confirmed, "es.import.node-builtin"));
        }

        if let Some(ts) = m.nearest_tsconfig(dir) {
            let base_dir = match &ts.base_url {
                Some(b) => paths::join(&ts.dir, b),
                None => Some(ts.dir.clone()),
            };
            if let Some(base_dir) = &base_dir {
                // The most specific pattern (longest prefix) wins, as in tsc.
                let mut best: Option<(usize, &Vec<String>, &str)> = None;
                for (pattern, substitutions) in &ts.paths {
                    let captured = match pattern.split_once('*') {
                        Some((prefix, suffix)) => spec
                            .strip_prefix(prefix)
                            .and_then(|rest| rest.strip_suffix(suffix))
                            .map(|c| (prefix.len(), c)),
                        None => (pattern == spec).then_some((usize::MAX, "")),
                    };
                    if let Some((score, capture)) = captured {
                        if best.is_none_or(|(s, _, _)| score > s) {
                            best = Some((score, substitutions, capture));
                        }
                    }
                }
                if let Some((_, substitutions, capture)) = best {
                    for substitution in substitutions {
                        let candidate = substitution.replacen('*', capture, 1);
                        if let Some(target) = paths::join(base_dir, &candidate).and_then(|p| m.es_file(&p)) {
                            return Ok(Found { target, confidence: Confidence::Confirmed, rule: "es.import.tsconfig-paths" });
                        }
                    }
                    return Err(Miss::new(
                        "es.import.tsconfig-paths",
                        format!("`{spec}` matches a tsconfig path mapping but no mapped file exists"),
                    ));
                }
            }
            if ts.base_url.is_some() {
                if let Some(target) = base_dir.as_deref().and_then(|b| paths::join(b, spec)).and_then(|p| m.es_file(&p)) {
                    // A package of the same name in node_modules could shadow this.
                    return Ok(Found { target, confidence: Confidence::Probable, rule: "es.import.tsconfig-baseurl" });
                }
            }
        }

        let package = m
            .packages
            .iter()
            .filter(|p| spec == p.name || (spec.starts_with(p.name.as_str()) && spec.as_bytes().get(p.name.len()) == Some(&b'/')))
            .max_by_key(|p| p.name.len());
        if let Some(package) = package {
            let sub = spec[package.name.len()..].trim_start_matches('/');
            let mut candidates: Vec<String> = Vec::new();
            if sub.is_empty() {
                candidates.extend(package.entries.iter().cloned());
                candidates.push("src/index".into());
                candidates.push("index".into());
            } else {
                candidates.push(sub.to_string());
                candidates.push(format!("src/{sub}"));
            }
            for candidate in candidates {
                if let Some(target) = paths::join(&package.dir, &candidate).and_then(|p| m.es_file(&p)) {
                    // Package `exports` maps and build output are not modelled.
                    return Ok(Found { target, confidence: Confidence::Probable, rule: "es.import.workspace-package" });
                }
            }
            return Err(Miss::new(
                "es.import.workspace-package",
                format!("`{spec}` names workspace package `{}` but no entry file was found", package.name),
            ));
        }

        let name = npm_package_name(spec);
        if name.is_empty() {
            return Err(Miss::new("es.import.invalid", format!("`{spec}` is not a valid module specifier")));
        }
        if m.npm_deps.contains(name) {
            return Ok(self.external("npm", name, &[("declared", true)], Confidence::Confirmed, "es.import.declared-dependency"));
        }
        if builtins::is_node_builtin(name) {
            return Ok(self.external("node", name, &[("builtin", true)], Confidence::Confirmed, "es.import.node-builtin"));
        }
        // Not in any manifest: likely a transitive or ambient package.
        Ok(self.external("npm", name, &[("declared", false)], Confidence::Probable, "es.import.undeclared-package"))
    }

    /// Resolve a Python module reference (`level` leading dots).
    pub(crate) fn py_module(&self, from: usize, module: &str, level: u32) -> Res {
        let m = &self.modules;
        if level > 0 {
            let mut base = self.files[from].dir;
            for _ in 1..level {
                match paths::parent(base) {
                    Some(p) => base = p,
                    None => return Err(Miss::new("py.import.relative", "relative import goes above the repository root")),
                }
            }
            let rel = module.replace('.', "/");
            let found = if rel.is_empty() {
                PY_EXTENSIONS.iter().find_map(|ext| {
                    let init = if base.is_empty() { format!("__init__{ext}") } else { format!("{base}/__init__{ext}") };
                    m.by_path.get(init.as_str()).copied()
                })
            } else {
                let (dir, name) = match rel.rfind('/') {
                    Some(i) => (paths::join(base, &rel[..i]), &rel[i + 1..]),
                    None => (Some(base.to_string()), rel.as_str()),
                };
                dir.and_then(|d| m.py_file_at(&d, name))
            };
            return match found {
                Some(f) => Ok(Found { target: Target::File(f), confidence: Confidence::Confirmed, rule: "py.import.relative" }),
                None => Err(Miss::new(
                    "py.import.relative",
                    format!("no module `{}{module}` relative to `{}`", ".".repeat(level as usize), self.files[from].path),
                )),
            };
        }

        let top = module.split('.').next().unwrap_or(module);
        if top.is_empty() {
            return Err(Miss::new("py.import.invalid", "empty module name"));
        }
        let stdlib = builtins::is_python_stdlib(top);
        match m.py_absolute(from, module) {
            // A repository module that is not on the importer's path does not
            // shadow the standard library.
            Ok(Some((file, confidence))) if !(stdlib && confidence != Confidence::Confirmed) => {
                return Ok(Found { target: Target::File(file), confidence, rule: "py.import.absolute" });
            }
            Err(n) if !stdlib => {
                return Err(Miss::new("py.import.absolute", format!("module `{module}` matches {n} files in the repository; ambiguous")));
            }
            _ => {}
        }
        if stdlib {
            return Ok(self.external("python", top, &[("stdlib", true)], Confidence::Confirmed, "py.import.stdlib"));
        }
        if m.py_top_level.contains(top) {
            return Err(Miss::new(
                "py.import.absolute",
                format!("package `{top}` is in the repository but module `{module}` was not found"),
            ));
        }
        if m.pypi_deps.contains(&normalize_pypi(top)) {
            return Ok(self.external("python", top, &[("declared", true)], Confidence::Confirmed, "py.import.declared-dependency"));
        }
        // Distribution names often differ from import names (PyYAML → yaml),
        // so absence from the manifest is not evidence of an error.
        Ok(self.external("python", top, &[("declared", false)], Confidence::Probable, "py.import.undeclared-module"))
    }
}

/// `a/b/c.py` → `a.b.c`; `a/b/__init__.py` → `a.b`. `None` if not a module.
fn python_module_name(rel: &str) -> Option<String> {
    let stem = PY_EXTENSIONS.iter().find_map(|ext| rel.strip_suffix(ext))?;
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    if stem.is_empty() || stem == "__init__" {
        return None;
    }
    Some(stem.replace('/', "."))
}

fn npm_package_name(spec: &str) -> &str {
    let mut parts = spec.splitn(3, '/');
    let first = parts.next().unwrap_or("");
    if first.starts_with('@') {
        match parts.next() {
            Some(second) if !second.is_empty() => &spec[..first.len() + 1 + second.len()],
            _ => "",
        }
    } else {
        first
    }
}

/// PEP 503 name normalization.
fn normalize_pypi(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !prev_sep {
                out.push('-');
            }
            prev_sep = true;
        } else {
            out.extend(ch.to_lowercase());
            prev_sep = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(python_module_name("a/b/c.py").as_deref(), Some("a.b.c"));
        assert_eq!(python_module_name("a/b/__init__.py").as_deref(), Some("a.b"));
        assert_eq!(python_module_name("__init__.py"), None);
        assert_eq!(python_module_name("a/b.txt"), None);
        assert_eq!(npm_package_name("lodash/fp"), "lodash");
        assert_eq!(npm_package_name("@scope/pkg/sub/path"), "@scope/pkg");
        assert_eq!(npm_package_name("@scope"), "");
        assert_eq!(normalize_pypi("Typing_Extensions"), "typing-extensions");
        assert_eq!(normalize_pypi("zope.interface"), "zope-interface");
    }
}
