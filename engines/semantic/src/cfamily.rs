//! C and C++ resolution rules.
//!
//! There are no modules: a name is visible when it is declared in the file
//! or in a header the file includes, directly or transitively. Includes are
//! resolved relative to the including file, then in configured include
//! directories, then by a unique path suffix in the repository. A call
//! resolves to the declaration that makes the name visible and, when the
//! repository contains the definition, to the definition: confirmed when
//! there is exactly one non-`static` definition, probable when several
//! exist (alternative builds). Names that are defined in the repository but
//! not declared in anything the file includes are matched by name and
//! marked inferred. Member functions defined outside their class
//! (`void Cart::add()`) are members of the class; calls of `virtual`
//! functions are probable.

use crate::builtins;
use crate::golang::local_type;
use crate::model::ProjectConfig;
use crate::{paths, FileIx, Found, Miss, Res, Resolver, SymRef, Target};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use usie_ir::{Confidence, ImportKind, LanguageFamily, SymbolKind};

pub(crate) struct CIndex {
    /// Namespace-level symbols by qualified name (`ns.Class`, `fn`).
    globals: HashMap<String, Vec<SymRef>>,
    by_path: HashMap<String, usize>,
    all_paths: HashSet<String>,
    /// Repository paths by file name, for suffix matching.
    by_name: HashMap<String, Vec<String>>,
    include_dirs: Vec<String>,
    /// Out-of-class member function definitions by class.
    methods: HashMap<SymRef, Vec<SymRef>>,
    visible: RefCell<HashMap<usize, Rc<HashSet<usize>>>>,
}

const C_STD_HEADERS: &[&str] = &[
    "assert.h",
    "complex.h",
    "ctype.h",
    "errno.h",
    "fenv.h",
    "float.h",
    "inttypes.h",
    "iso646.h",
    "limits.h",
    "locale.h",
    "math.h",
    "setjmp.h",
    "signal.h",
    "stdalign.h",
    "stdarg.h",
    "stdatomic.h",
    "stdbool.h",
    "stddef.h",
    "stdint.h",
    "stdio.h",
    "stdlib.h",
    "stdnoreturn.h",
    "string.h",
    "tgmath.h",
    "threads.h",
    "time.h",
    "uchar.h",
    "wchar.h",
    "wctype.h",
];
const POSIX_HEADERS: &[&str] = &[
    "arpa/",
    "dirent.h",
    "dlfcn.h",
    "fcntl.h",
    "glob.h",
    "grp.h",
    "netdb.h",
    "netinet/",
    "poll.h",
    "pthread.h",
    "pwd.h",
    "regex.h",
    "sched.h",
    "semaphore.h",
    "spawn.h",
    "strings.h",
    "sys/",
    "syslog.h",
    "termios.h",
    "unistd.h",
    "utime.h",
];

impl CIndex {
    pub(crate) fn new(config: &ProjectConfig, files: &[FileIx<'_>]) -> Self {
        let mut index = CIndex {
            globals: HashMap::new(),
            by_path: HashMap::new(),
            all_paths: config.paths.iter().cloned().collect(),
            by_name: HashMap::new(),
            include_dirs: config
                .include_dirs
                .iter()
                .map(|d| d.trim_matches('/').to_string())
                .collect(),
            methods: HashMap::new(),
            visible: RefCell::new(HashMap::new()),
        };
        for f in files {
            index.all_paths.insert(f.path.to_string());
        }
        for p in &index.all_paths {
            index
                .by_name
                .entry(paths::file_name(p).to_string())
                .or_default()
                .push(p.clone());
        }
        for v in index.by_name.values_mut() {
            v.sort();
        }
        for (fi, f) in files.iter().enumerate() {
            if f.family != LanguageFamily::C {
                continue;
            }
            index.by_path.insert(f.path.to_string(), fi);
            for sym in &f.ir.symbols {
                // Only namespace-level declarations are global. Qualified
                // definitions (`void ns::f() {}`, `void C::m() {}`) are
                // registered under their qualified name too, so `ns::f`
                // finds its definition.
                let mut p = sym.parent;
                let mut global = true;
                while let Some(id) = p {
                    let ps = &f.ir.symbols[id as usize];
                    if ps.kind != SymbolKind::Namespace {
                        global = false;
                        break;
                    }
                    p = ps.parent;
                }
                if global {
                    index
                        .globals
                        .entry(sym.qualified_name.clone())
                        .or_default()
                        .push((fi, sym.id));
                }
            }
        }
        index
    }
}

fn std_header(path: &str) -> Option<(&'static str, &'static str)> {
    if C_STD_HEADERS.contains(&path) {
        return Some(("c", "libc"));
    }
    // `<cstdio>` and the C++ library headers have no extension.
    if !path.contains('.') && !path.contains('/') {
        return Some(("cpp", "std"));
    }
    if POSIX_HEADERS
        .iter()
        .any(|h| path == *h || (h.ends_with('/') && path.starts_with(h)))
    {
        return Some(("c", "posix"));
    }
    None
}

impl<'a> Resolver<'a> {
    /// Resolve `#include` number `idx` of file `fi`.
    pub(crate) fn c_include(&self, fi: usize, idx: usize) -> Res {
        let imp = &self.files[fi].ir.imports[idx];
        if imp.kind != ImportKind::SideEffect {
            return self.c_using(fi, idx);
        }
        let path = imp.module.as_str();
        let system = imp.level == 1;
        let c = &self.c;
        let hit = |candidate: &str, rule: &'static str, confidence: Confidence| -> Option<Found> {
            if let Some(&f) = c.by_path.get(candidate) {
                return Some(Found {
                    target: Target::File(f),
                    confidence,
                    rule,
                });
            }
            c.all_paths.contains(candidate).then(|| Found {
                target: Target::Asset(candidate.to_string()),
                confidence,
                rule,
            })
        };
        if !system {
            if let Some(found) = paths::join(self.files[fi].dir, path)
                .and_then(|p| hit(&p, "c.include.relative", Confidence::Confirmed))
            {
                return Ok(found);
            }
        }
        for dir in &c.include_dirs {
            if let Some(found) = paths::join(dir, path)
                .and_then(|p| hit(&p, "c.include.include-dir", Confidence::Confirmed))
            {
                return Ok(found);
            }
        }
        // A unique (or nearest) repository file with this path suffix.
        let name = paths::file_name(path);
        let candidates: Vec<&String> = c
            .by_name
            .get(name)
            .map(|v| {
                v.iter()
                    .filter(|p| {
                        p.as_str() == path
                            || (p.ends_with(path)
                                && p.as_bytes().get(p.len() - path.len() - 1) == Some(&b'/'))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !candidates.is_empty() && !(system && std_header(path).is_some()) {
            let from = self.files[fi].dir;
            let best = candidates
                .iter()
                .max_by_key(|p| {
                    let common = from
                        .split('/')
                        .zip(p.split('/'))
                        .take_while(|(a, b)| a == b)
                        .count();
                    (common, std::cmp::Reverse(p.len()))
                })
                .copied()
                .unwrap_or(candidates[0]);
            if let Some(found) = hit(best, "c.include.path-suffix", Confidence::Probable) {
                return Ok(found);
            }
        }
        if let Some((eco, lib)) = std_header(path) {
            return Ok(self.external(
                eco,
                lib,
                &[("stdlib", true)],
                Confidence::Confirmed,
                "c.include.standard",
            ));
        }
        if system {
            let first = path.split('/').next().unwrap_or(path);
            let lib = first
                .strip_suffix(".h")
                .or_else(|| first.strip_suffix(".hpp"))
                .unwrap_or(first);
            return Ok(self.external(
                "c",
                lib,
                &[("declared", false)],
                Confidence::Probable,
                "c.include.system",
            ));
        }
        Err(Miss::new(
            "c.include.relative",
            format!("no file in the repository matches `{path}`"),
        ))
    }

    /// `using namespace N;` / `using N::x;`.
    fn c_using(&self, fi: usize, idx: usize) -> Res {
        let imp = &self.files[fi].ir.imports[idx];
        let mut path: Vec<String> = imp
            .module
            .split("::")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if let Some(i) = &imp.imported {
            path.push(i.clone());
        }
        if path.is_empty() {
            return Err(Miss::new("c.using", "empty using declaration"));
        }
        if path[0] == "std" {
            return Ok(self.external(
                "cpp",
                "std",
                &[("stdlib", true)],
                Confidence::Confirmed,
                "c.using.std",
            ));
        }
        let fqn = path.join(".");
        if let Some(cands) = self.c.globals.get(&fqn) {
            if let Some(&(f, id)) = cands.first() {
                return Ok(Found {
                    target: Target::Symbol(f, id),
                    confidence: Confidence::Confirmed,
                    rule: "c.using",
                });
            }
        }
        // A namespace spread over several files has no single symbol.
        let prefix = format!("{fqn}.");
        if self.c.globals.keys().any(|k| k.starts_with(&prefix)) {
            return Err(Miss {
                rule: "builtin",
                reason: String::new(),
                builtin: true,
                unknown_receiver: false,
            });
        }
        Ok(self.external(
            "c",
            &path[0],
            &[("declared", false)],
            Confidence::Probable,
            "c.using.external",
        ))
    }

    /// Files whose declarations are visible in `fi`: itself and everything
    /// it includes, transitively.
    fn c_visible(&self, fi: usize) -> Rc<HashSet<usize>> {
        if let Some(v) = self.c.visible.borrow().get(&fi) {
            return v.clone();
        }
        let mut seen: HashSet<usize> = HashSet::new();
        let mut stack = vec![fi];
        while let Some(f) = stack.pop() {
            if !seen.insert(f) || seen.len() > 5000 {
                continue;
            }
            for (idx, imp) in self.files[f].ir.imports.iter().enumerate() {
                if imp.kind != ImportKind::SideEffect {
                    continue;
                }
                if let Ok(Found {
                    target: Target::File(t),
                    ..
                }) = self.c_include(f, idx)
                {
                    stack.push(t);
                }
            }
        }
        let v = Rc::new(seen);
        self.c.visible.borrow_mut().insert(fi, v.clone());
        v
    }

    /// Pick the target among the declarations and definitions of `fqn`.
    fn c_global(&self, fi: usize, fqn: &str) -> Option<Found> {
        let cands: Vec<SymRef> = self
            .c
            .globals
            .get(fqn)?
            .iter()
            .copied()
            .filter(|(f, id)| *f == fi || !self.sym((*f, *id)).attrs.contains_key("static"))
            .collect();
        if cands.is_empty() {
            return None;
        }
        let visible = self.c_visible(fi);
        let declared = cands.iter().any(|(f, _)| visible.contains(f));
        let mut defs: Vec<SymRef> = cands
            .iter()
            .copied()
            .filter(|r| !self.sym(*r).attrs.contains_key("declaration"))
            .collect();
        // Prefer a type definition over a forward declaration or typedef of
        // the same name, and a definition in this file over others.
        defs.sort_by_key(|r| {
            (
                r.0 != fi,
                !visible.contains(&r.0),
                self.sym(*r).kind != SymbolKind::Class,
            )
        });
        let (target, confidence, rule) = match (declared, defs.len()) {
            (true, 1) => (defs[0], Confidence::Confirmed, "c.scope.definition"),
            (true, 0) => {
                let decl = cands
                    .iter()
                    .copied()
                    .find(|(f, _)| visible.contains(f))
                    .unwrap_or(cands[0]);
                (decl, Confidence::Confirmed, "c.scope.declaration")
            }
            (true, _) => {
                // Same-file or the only visible definition wins outright.
                if defs[0].0 == fi
                    || (visible.contains(&defs[0].0) && !visible.contains(&defs[1].0))
                {
                    (defs[0], Confidence::Confirmed, "c.scope.definition")
                } else {
                    (defs[0], Confidence::Probable, "c.link.variant")
                }
            }
            (false, 1) => (defs[0], Confidence::Inferred, "heuristic.c.global-name"),
            (false, _) => return None,
        };
        Some(Found {
            target: Target::Symbol(target.0, target.1),
            confidence,
            rule,
        })
    }

    /// The class a method belongs to (in-class or `Class::method`).
    fn c_enclosing_class(&self, fi: usize, scope: Option<u32>) -> Option<SymRef> {
        let ir = self.files[fi].ir;
        let mut cur = scope;
        while let Some(id) = cur {
            let sym = &ir.symbols[id as usize];
            if sym.kind == SymbolKind::Class {
                return Some((fi, id));
            }
            if let Some(ty) = sym.attrs.get("impl_type").and_then(|v| v.as_array()) {
                let ty: Vec<String> = ty
                    .iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect();
                if let Ok(Found {
                    target: Target::Symbol(tf, tid),
                    ..
                }) = self.c_name(fi, sym.parent, &ty)
                {
                    if self.sym((tf, tid)).kind == SymbolKind::Class {
                        return Some((tf, tid));
                    }
                }
                return None;
            }
            cur = sym.parent;
        }
        None
    }

    fn c_namespaces(&self, fi: usize, scope: Option<u32>) -> Vec<String> {
        let ir = self.files[fi].ir;
        let mut out = Vec::new();
        let mut cur = scope;
        while let Some(id) = cur {
            let sym = &ir.symbols[id as usize];
            if sym.kind == SymbolKind::Namespace {
                out.push(sym.qualified_name.clone());
            }
            cur = sym.parent;
        }
        out.push(String::new());
        out
    }

    /// A namespace-level name: the longest prefix of `path` that names a
    /// global, tried in each enclosing namespace and each `using namespace`.
    fn c_name(&self, fi: usize, scope: Option<u32>, path: &[String]) -> Res {
        let mut prefixes = self.c_namespaces(fi, scope);
        for imp in self.files[fi].ir.imports.iter() {
            if imp.kind == ImportKind::Wildcard {
                prefixes.push(imp.module.replace("::", "."));
            }
        }
        for prefix in &prefixes {
            for k in (1..=path.len()).rev() {
                let name = path[..k].join(".");
                let fqn = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}.{name}")
                };
                if let Some(found) = self.c_global(fi, &fqn) {
                    return self.descend(found, &path[k..]);
                }
            }
        }
        // `using N::x;`
        if let Some(&imp) = self.files[fi].bindings.get(&(None, path[0].as_str())) {
            let base = self.c_using(fi, imp)?;
            return self.descend(base, &path[1..]);
        }
        Err(Miss::new("scope.unbound", String::new()))
    }

    pub(crate) fn c_lookup(&self, fi: usize, scope: Option<u32>, path: &[String]) -> Res {
        let ir = self.files[fi].ir;
        let first = path[0].as_str();
        if first == "this" {
            let Some(class) = self.c_enclosing_class(fi, scope) else {
                return Err(Miss::new("c.receiver", "`this` outside a member function"));
            };
            if path.len() == 1 {
                return Err(Miss::new(
                    "c.receiver",
                    "calling `this` is not a member call",
                ));
            }
            return match self.member(class, &path[1], 0) {
                Some((m, inherited)) => {
                    let base = Found {
                        target: Target::Symbol(m.0, m.1),
                        confidence: self.c_member_confidence(m, inherited),
                        rule: "c.call.this-member",
                    };
                    self.descend(base, &path[2..])
                }
                None => Err(Miss::new(
                    "c.call.this-member",
                    format!(
                        "`{}` is not a member of `{}` or its resolved bases",
                        path[1],
                        self.sym(class).qualified_name
                    ),
                )),
            };
        }
        if first == "std" {
            return Ok(self.external(
                "cpp",
                "std",
                &[("stdlib", true)],
                Confidence::Confirmed,
                "c.scope.std",
            ));
        }
        // Locals and parameters.
        let mut cur = scope;
        while let Some(id) = cur {
            let sym = &ir.symbols[id as usize];
            if sym
                .locals
                .binary_search_by(|l| l.as_str().cmp(first))
                .is_ok()
            {
                if let Some(ty) = local_type(sym, first) {
                    return self.typed_value(fi, Some(id), first, &ty, &path[1..], "c.typed-value");
                }
                if let Some(res) =
                    self.call_result_value(fi, id, first, &path[1..], "c.call-result")
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
        // Members of the enclosing class.
        if let Some(class) = self.c_enclosing_class(fi, scope) {
            if let Some((m, inherited)) = self.member(class, first, 0) {
                let base = Found {
                    target: Target::Symbol(m.0, m.1),
                    confidence: self.c_member_confidence(m, inherited),
                    rule: "c.scope.member",
                };
                return self.descend(base, &path[1..]);
            }
        }
        match self.c_name(fi, scope, path) {
            Err(miss) if miss.reason.is_empty() && !miss.builtin => {}
            other => return other,
        }
        if builtins::is_c_builtin(first) {
            return Err(Miss {
                rule: "builtin",
                reason: String::new(),
                builtin: true,
                unknown_receiver: false,
            });
        }
        // Names from `using namespace` of a namespace outside the repository.
        let external: Vec<&str> = ir
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Wildcard)
            .map(|i| i.module.as_str())
            .collect();
        if external.len() == 1 && external[0] == "std" {
            return Ok(Found {
                confidence: Confidence::Probable,
                rule: "c.using.std",
                ..self.external(
                    "cpp",
                    "std",
                    &[("stdlib", true)],
                    Confidence::Probable,
                    "c.using.std",
                )
            });
        }
        Err(Miss::new(
            "scope.unbound",
            format!("no declaration of `{first}` is visible here or defined in the repository (macros from unresolved headers and system declarations are not seen)"),
        ))
    }

    /// Member function definitions outside the class.
    pub(crate) fn c_method(&self, class: SymRef, name: &str) -> Option<SymRef> {
        self.c
            .methods
            .get(&class)?
            .iter()
            .copied()
            .find(|m| self.sym(*m).name == name)
    }

    /// Calls of `virtual` member functions may dispatch to an override.
    pub(crate) fn c_member_confidence(&self, member: SymRef, inherited: bool) -> Confidence {
        let sym = self.sym(member);
        let mut is_virtual = sym.attrs.contains_key("virtual");
        if !is_virtual && sym.attrs.contains_key("impl_type") {
            // The out-of-class definition: look at the in-class declaration.
            if let Some(class) = self.c_enclosing_class(member.0, Some(member.1)) {
                is_virtual = self
                    .child_named(class.0, class.1, &sym.name)
                    .is_some_and(|d| self.sym((class.0, d)).attrs.contains_key("virtual"));
            }
        }
        if is_virtual || inherited {
            Confidence::Probable
        } else {
            Confidence::Confirmed
        }
    }

    /// Attach out-of-class member function definitions to their classes.
    pub(crate) fn c_bind_methods(&mut self) {
        let mut methods: HashMap<SymRef, Vec<SymRef>> = HashMap::new();
        for fi in 0..self.files.len() {
            if self.files[fi].family != LanguageFamily::C {
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
                if let Ok(Found {
                    target: Target::Symbol(tf, tid),
                    ..
                }) = self.c_name(fi, sym.parent, &ty)
                {
                    if self.sym((tf, tid)).kind == SymbolKind::Class {
                        methods.entry((tf, tid)).or_default().push((fi, sym.id));
                    }
                }
            }
        }
        self.c.methods = methods;
        self.surface_memo.borrow_mut().clear();
    }
}
