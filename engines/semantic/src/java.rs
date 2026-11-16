//! Resolution rules for Java, Kotlin and C#.
//!
//! Types are identified by fully qualified name (package or namespace plus
//! the nesting of type declarations), independently of file paths. Simple
//! names are resolved in the order of the Java Language Specification
//! (§6.4, §7.5): locals, members of enclosing types (including inherited
//! ones), types of the compilation unit, single-type and single-static
//! imports, types of the same package, on-demand imports, then the
//! implicitly imported platform names. Kotlin adds package-level functions
//! and properties; C# looks types up in the enclosing namespaces from the
//! innermost outwards and treats `using N;` as an on-demand import.
//!
//! Rules are written with a `java.` prefix here; the resolver reports them
//! with the prefix of the referencing file's language (`kotlin.`, `csharp.`).

use crate::builtins;
use crate::golang::local_type;
use crate::model::ProjectConfig;
use crate::{FileIx, Found, Miss, Res, Resolver, SymRef, Target};
use std::collections::HashMap;
use usie_ir::{Confidence, ImportKind, LanguageFamily, SymbolKind};

pub(crate) struct JavaIndex {
    /// Types (top-level and member types) by fully qualified name.
    types: HashMap<String, Vec<SymRef>>,
    /// Top-level types by package.
    packages: HashMap<String, Vec<SymRef>>,
    package_of: HashMap<usize, String>,
    /// Package of each file, and the files of each package, for the
    /// directory a package import points at.
    package_files: HashMap<String, Vec<usize>>,
    /// Every declaration of a type that is split over several declarations
    /// (C# `partial`, Kotlin `expect`/`actual`), keyed by each of them.
    pub(crate) parts: HashMap<SymRef, Vec<SymRef>>,
    /// Extension methods (C#) and functions (Kotlin) by name, with the last
    /// segment of their receiver type.
    extensions: HashMap<String, Vec<(SymRef, String)>>,
    /// .NET project directories (with a project file) and the namespaces
    /// each imports for all its files.
    projects: Vec<(String, Vec<String>)>,
    /// Kotlin package-level functions and properties by qualified name.
    members: HashMap<String, Vec<SymRef>>,
    /// Declared Maven group ids, longest first.
    groups: Vec<String>,
    /// Declared NuGet package ids, longest first.
    nuget: Vec<String>,
}

/// Families resolved by this module.
pub(crate) fn is_classic(family: LanguageFamily) -> bool {
    matches!(
        family,
        LanguageFamily::Java | LanguageFamily::Kotlin | LanguageFamily::Csharp
    )
}

impl JavaIndex {
    pub(crate) fn new(config: &ProjectConfig, files: &[FileIx<'_>]) -> Self {
        let mut index = JavaIndex {
            types: HashMap::new(),
            packages: HashMap::new(),
            package_of: HashMap::new(),
            package_files: HashMap::new(),
            members: HashMap::new(),
            parts: HashMap::new(),
            extensions: HashMap::new(),
            projects: Vec::new(),
            groups: Vec::new(),
            nuget: Vec::new(),
        };
        let mut nuget: Vec<String> = config
            .dependencies
            .iter()
            .filter(|d| d.ecosystem == "nuget")
            .map(|d| d.name.clone())
            .collect();
        nuget.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        nuget.dedup();
        index.nuget = nuget;
        let mut groups: Vec<String> = config
            .dependencies
            .iter()
            .filter(|d| d.ecosystem == "maven")
            .filter_map(|d| d.name.split(':').next())
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .collect();
        groups.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        groups.dedup();
        index.groups = groups;
        for (fi, f) in files.iter().enumerate() {
            if !is_classic(f.family) {
                continue;
            }
            let package =
                f.ir.attrs
                    .get("package")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            index.package_of.insert(fi, package.clone());
            index
                .package_files
                .entry(package.clone())
                .or_default()
                .push(fi);
            for sym in &f.ir.symbols {
                // Namespace blocks (C#) contribute to the package; types
                // nest inside types; anything else (local and anonymous
                // classes) has no qualified name.
                let mut namespaces: Vec<&str> = Vec::new();
                let mut types: Vec<&str> = Vec::new();
                let mut member = true;
                let mut p = sym.parent;
                while let Some(id) = p {
                    let ps = &f.ir.symbols[id as usize];
                    match ps.kind {
                        SymbolKind::Namespace => namespaces.push(&ps.name),
                        SymbolKind::Class | SymbolKind::Interface if namespaces.is_empty() => {
                            types.push(&ps.name)
                        }
                        _ => {
                            member = false;
                            break;
                        }
                    }
                    p = ps.parent;
                }
                if !member {
                    continue;
                }
                namespaces.reverse();
                types.reverse();
                let mut ns: Vec<&str> = Vec::new();
                if !package.is_empty() {
                    ns.push(&package);
                }
                ns.extend(namespaces);
                let ns = ns.join(".");
                if sym.kind == SymbolKind::Namespace {
                    let full = if ns.is_empty() {
                        sym.name.clone()
                    } else {
                        format!("{ns}.{}", sym.name)
                    };
                    index.package_files.entry(full).or_default().push(fi);
                    continue;
                }
                let mut parts: Vec<&str> = Vec::new();
                if !ns.is_empty() {
                    parts.push(&ns);
                }
                parts.extend(types.iter().copied());
                parts.push(&sym.name);
                let fqn = parts.join(".");
                match sym.kind {
                    SymbolKind::Class | SymbolKind::Interface => {
                        index.types.entry(fqn).or_default().push((fi, sym.id));
                        if types.is_empty() {
                            index.packages.entry(ns).or_default().push((fi, sym.id));
                        }
                    }
                    SymbolKind::Function | SymbolKind::Variable | SymbolKind::Type
                        if types.is_empty() && f.family == LanguageFamily::Kotlin =>
                    {
                        index.members.entry(fqn).or_default().push((fi, sym.id));
                    }
                    _ => {}
                }
            }
        }
        // .NET projects: implicit `<Using>` namespaces plus the `global
        // using` directives of their files.
        let mut projects: Vec<(String, Vec<String>)> = config
            .paths
            .iter()
            .filter(|p| p.ends_with(".csproj"))
            .map(|p| (crate::paths::dir_of(p).to_string(), Vec::new()))
            .collect();
        projects.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.cmp(b)));
        projects.dedup();
        for u in &config.dotnet_usings {
            if let Some(p) = projects.iter_mut().find(|p| p.0 == u.dir.trim_matches('/')) {
                p.1.push(u.namespace.clone());
            }
        }
        for f in files.iter().filter(|f| f.family == LanguageFamily::Csharp) {
            let globals =
                f.ir.imports
                    .iter()
                    .filter(|i| i.kind == ImportKind::Wildcard && i.level == 1)
                    .map(|i| i.module.clone());
            let project = projects
                .iter_mut()
                .find(|p| crate::paths::is_under(f.path, &p.0));
            match project {
                Some(p) => p.1.extend(globals),
                None => {
                    let g: Vec<String> = globals.collect();
                    if !g.is_empty() {
                        projects.push((String::new(), g));
                    }
                }
            }
        }
        for p in &mut projects {
            p.1.sort();
            p.1.dedup();
        }
        index.projects = projects;
        for (fi, f) in files.iter().enumerate() {
            if !is_classic(f.family) {
                continue;
            }
            for sym in &f.ir.symbols {
                let receiver = sym
                    .attrs
                    .get("extension_receiver")
                    .and_then(|r| r.as_array())
                    .and_then(|r| r.last())
                    .and_then(|r| r.as_str());
                if let Some(r) = receiver {
                    index
                        .extensions
                        .entry(sym.name.clone())
                        .or_default()
                        .push(((fi, sym.id), r.to_string()));
                }
            }
        }
        // Types declared in several parts.
        let mut parts: Vec<Vec<SymRef>> = Vec::new();
        for decls in index.types.values_mut() {
            if decls.len() < 2 {
                continue;
            }
            let split = decls.iter().all(|(f, id)| {
                let sym = &files[*f].ir.symbols[*id as usize];
                let mods = sym.attrs.get("modifiers").and_then(|m| m.as_array());
                mods.is_some_and(|m| {
                    m.iter()
                        .any(|k| k == "partial" || k == "expect" || k == "actual")
                })
            });
            if split {
                // The `expect` declaration (the common API) represents the type.
                decls.sort_by_key(|(f, id)| {
                    let sym = &files[*f].ir.symbols[*id as usize];
                    let expect = sym
                        .attrs
                        .get("modifiers")
                        .and_then(|m| m.as_array())
                        .is_some_and(|m| m.iter().any(|k| k == "expect"));
                    (!expect, *f, *id)
                });
                parts.push(decls.clone());
                decls.truncate(1);
            }
        }
        for group in parts {
            for r in &group {
                index.parts.insert(*r, group.clone());
            }
        }
        index
    }

    fn is_package(&self, name: &str) -> bool {
        self.package_files.contains_key(name)
    }

    /// The directory holding a package's files; `true` when all of them are
    /// in that one directory.
    pub(crate) fn package_dir<'f>(
        &self,
        name: &str,
        files: &[FileIx<'f>],
    ) -> Option<(String, bool)> {
        let fis = self.package_files.get(name)?;
        let mut dirs: Vec<&str> = fis.iter().map(|f| files[*f].dir).collect();
        dirs.sort_unstable();
        dirs.dedup();
        Some((dirs[0].to_string(), dirs.len() == 1))
    }
}

fn starts_upper(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase)
}

impl<'a> Resolver<'a> {
    fn java_type(&self, fqn: &str, rule: &'static str) -> Option<Res> {
        let decls = self
            .java
            .types
            .get(fqn)
            .or_else(|| self.java.members.get(fqn))?;
        Some(match decls.as_slice() {
            [(f, id)] => Ok(Found {
                target: Target::Symbol(*f, *id),
                confidence: Confidence::Confirmed,
                rule,
            }),
            _ => Err(Miss::new(
                rule,
                format!(
                    "`{fqn}` is declared {} times in the repository; ambiguous",
                    decls.len()
                ),
            )),
        })
    }

    /// An external package node for a package/namespace or type name used
    /// in file `fi`.
    fn java_external(&self, fi: usize, name: &str) -> Found {
        if self.files[fi].family == LanguageFamily::Csharp {
            return self.dotnet_external(name);
        }
        let package: Vec<&str> = name.split('.').take_while(|s| !starts_upper(s)).collect();
        let package = package.join(".");
        if package == "kotlin" || package.starts_with("kotlin.") {
            return self.external(
                "kotlin",
                "stdlib",
                &[("stdlib", true)],
                Confidence::Confirmed,
                "java.import.kotlin-stdlib",
            );
        }
        if builtins::is_jdk_package(&package) {
            let module: Vec<&str> = package.split('.').take(2).collect();
            return self.external(
                "jdk",
                &module.join("."),
                &[("stdlib", true)],
                Confidence::Confirmed,
                "java.import.jdk",
            );
        }
        for group in &self.java.groups {
            if package == *group
                || (package.starts_with(group.as_str())
                    && package.as_bytes().get(group.len()) == Some(&b'.'))
            {
                // Packages conventionally start with the Maven group id.
                return self.external(
                    "maven",
                    group,
                    &[("declared", true)],
                    Confidence::Probable,
                    "java.import.declared-dependency",
                );
            }
        }
        let root: Vec<&str> = package.split('.').take(3).collect();
        self.external(
            "maven",
            &root.join("."),
            &[("declared", false)],
            Confidence::Probable,
            "java.import.undeclared-package",
        )
    }

    /// .NET namespaces: the base class library (`System.*`), declared NuGet
    /// packages (whose ids conventionally prefix their namespaces), or an
    /// undeclared package named after the first two segments.
    fn dotnet_external(&self, name: &str) -> Found {
        if name == "System" || name.starts_with("System.") {
            return self.external(
                "dotnet",
                "System",
                &[("stdlib", true)],
                Confidence::Confirmed,
                "java.import.dotnet-bcl",
            );
        }
        for id in &self.java.nuget {
            if name == id
                || (name.len() > id.len()
                    && name[..id.len()].eq_ignore_ascii_case(id)
                    && name.as_bytes()[id.len()] == b'.')
            {
                return self.external(
                    "nuget",
                    id,
                    &[("declared", true)],
                    Confidence::Probable,
                    "java.import.declared-dependency",
                );
            }
        }
        let root: Vec<&str> = name.split('.').take(2).collect();
        self.external(
            "nuget",
            &root.join("."),
            &[("declared", false)],
            Confidence::Probable,
            "java.import.undeclared-package",
        )
    }

    /// Resolve the import at index `idx` of a Java, Kotlin or C# file `fi`.
    pub(crate) fn java_import(&self, fi: usize, idx: usize) -> Res {
        let imp = &self.files[fi].ir.imports[idx];
        match imp.kind {
            ImportKind::Named => {
                let name = imp.imported.as_deref().unwrap_or("");
                let fqn = if imp.module.is_empty() {
                    name.to_string()
                } else {
                    format!("{}.{name}", imp.module)
                };
                if let Some(res) = self.java_type(&fqn, "java.import.type") {
                    return res;
                }
                // `import static a.b.C.m;`
                if let Some(res) = self.java_type(&imp.module, "java.import.static-member") {
                    let ty = res?;
                    let Target::Symbol(tf, tid) = ty.target else {
                        return Ok(ty);
                    };
                    return match self.member((tf, tid), name, 0) {
                        Some(((mf, mid), inherited)) => Ok(Found {
                            target: Target::Symbol(mf, mid),
                            confidence: if inherited {
                                Confidence::Probable
                            } else {
                                Confidence::Confirmed
                            },
                            rule: "java.import.static-member",
                        }),
                        None => Err(Miss::new(
                            "java.import.static-member",
                            format!("`{}` has no member `{name}`", imp.module),
                        )),
                    };
                }
                if self.java.is_package(&imp.module) {
                    return Err(Miss::new(
                        "java.import.type",
                        format!("package `{}` declares no type `{name}`", imp.module),
                    ));
                }
                Ok(self.java_external(fi, &fqn))
            }
            ImportKind::Wildcard => {
                if self.java.is_package(&imp.module) {
                    let (_, single) = self
                        .java
                        .package_dir(&imp.module, &self.files)
                        .unwrap_or_default();
                    return Ok(Found {
                        target: Target::JavaPackage(imp.module.clone()),
                        confidence: if single {
                            Confidence::Confirmed
                        } else {
                            Confidence::Probable
                        },
                        rule: "java.import.on-demand",
                    });
                }
                if let Some(res) = self.java_type(&imp.module, "java.import.on-demand") {
                    return res;
                }
                Ok(self.java_external(fi, &imp.module))
            }
            _ => Err(Miss::new("java.import", "unsupported import form")),
        }
    }

    /// An extension method or function named `name` for values of the
    /// type `class` (by type name; overloads by receiver namespace are not
    /// distinguished).
    pub(crate) fn java_extension(&self, class: SymRef, name: &str) -> Option<Found> {
        let type_name = &self.sym(class).name;
        let mut names = vec![type_name.clone()];
        for b in self.bases.borrow().get(&class).into_iter().flatten() {
            names.push(self.sym(*b).name.clone());
        }
        let hits: Vec<SymRef> = self
            .java
            .extensions
            .get(name)?
            .iter()
            .filter(|(_, r)| names.contains(r))
            .map(|(s, _)| *s)
            .collect();
        // Overloads declared in one class count as one candidate.
        let first = *hits.first()?;
        let owner = |r: &SymRef| (r.0, self.sym(*r).parent);
        if !hits.iter().all(|h| owner(h) == owner(&first)) {
            return None;
        }
        Some(Found {
            target: Target::Symbol(first.0, first.1),
            confidence: Confidence::Probable,
            rule: "java.extension-method",
        })
    }

    /// The package or namespace that encloses `scope` in file `fi`.
    fn java_namespace(&self, fi: usize, scope: Option<u32>) -> String {
        let ir = self.files[fi].ir;
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = scope;
        while let Some(id) = cur {
            let sym = &ir.symbols[id as usize];
            if sym.kind == SymbolKind::Namespace {
                parts.push(&sym.name);
            }
            cur = sym.parent;
        }
        if let Some(p) = self.java.package_of.get(&fi).filter(|p| !p.is_empty()) {
            parts.push(p);
        }
        parts.reverse();
        parts.join(".")
    }

    /// The enclosing class or interface of a scope.
    fn java_enclosing_type(&self, fi: usize, scope: Option<u32>) -> Option<u32> {
        let ir = self.files[fi].ir;
        let mut cur = scope;
        while let Some(id) = cur {
            let sym = &ir.symbols[id as usize];
            if matches!(sym.kind, SymbolKind::Class | SymbolKind::Interface) {
                return Some(id);
            }
            cur = sym.parent;
        }
        None
    }

    /// Confidence of a member access on a value of a declared type: instance
    /// methods are dispatched on the runtime class, which may be a subclass.
    /// Kotlin and C# members are not overridable unless declared `open`,
    /// `virtual`, `abstract` or `override`, or declared in an interface.
    pub(crate) fn java_member_confidence(&self, member: SymRef, inherited: bool) -> Confidence {
        let sym = self.sym(member);
        let has = |words: &[&str]| {
            sym.attrs
                .get("modifiers")
                .and_then(|m| m.as_array())
                .is_some_and(|m| m.iter().any(|k| words.iter().any(|w| k == w)))
        };
        let in_interface = sym
            .parent
            .is_some_and(|p| self.sym((member.0, p)).kind == SymbolKind::Interface);
        let overridable = match self.files[member.0].family {
            LanguageFamily::Kotlin | LanguageFamily::Csharp => {
                in_interface || has(&["open", "virtual", "abstract", "override"])
            }
            _ => !has(&["private", "final"]),
        };
        let fixed = !sym.kind.is_callable()
            || sym.attrs.contains_key("static")
            || sym.attrs.contains_key("constructor")
            || !overridable;
        if fixed && !inherited {
            Confidence::Confirmed
        } else {
            Confidence::Probable
        }
    }

    /// When a class extends a class outside the repository, an unresolved
    /// member is most likely inherited from it.
    pub(crate) fn java_external_base(&self, class: SymRef, depth: u32) -> Option<Found> {
        if depth > 16 {
            return None;
        }
        let sym = self.sym(class);
        for base in sym.bases.iter().chain(sym.implements.iter()) {
            if base.segments.is_empty() {
                continue;
            }
            match self.java_lookup(class.0, sym.parent, &base.segments) {
                Ok(Found {
                    target: target @ Target::External(_),
                    ..
                }) => {
                    return Some(Found {
                        target,
                        confidence: Confidence::Probable,
                        rule: "java.member.inherited-external",
                    })
                }
                Ok(Found {
                    target: Target::Symbol(bf, bid),
                    ..
                }) if (bf, bid) != class => {
                    if let Some(found) = self.java_external_base((bf, bid), depth + 1) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }

    pub(crate) fn java_lookup(&self, fi: usize, scope: Option<u32>, path: &[String]) -> Res {
        let file = &self.files[fi];
        let ir = file.ir;
        let first = path[0].as_str();

        // `this` and `super` (`base` in C#).
        let csharp = file.family == LanguageFamily::Csharp;
        let first = if csharp && first == "base" {
            "super"
        } else {
            first
        };
        if first == "this" || first == "super" {
            let Some(class) = self.java_enclosing_type(fi, scope) else {
                return Err(Miss::new(
                    "java.receiver",
                    format!("`{first}` outside a class"),
                ));
            };
            let is_super = first == "super";
            if path.len() == 1 {
                if !is_super {
                    return Err(Miss::new(
                        "java.receiver",
                        "calling `this` is not a method call",
                    ));
                }
                let class_sym = &ir.symbols[class as usize];
                let base = self.bases.borrow().get(&(fi, class)).and_then(|b| {
                    b.iter()
                        .copied()
                        .find(|r| self.sym(*r).kind == SymbolKind::Class)
                });
                return match base {
                    Some((bf, bid)) => Ok(Found {
                        target: Target::Symbol(bf, bid),
                        confidence: Confidence::Confirmed,
                        rule: "java.call.super-constructor",
                    }),
                    None => match self.java_external_base((fi, class), 0) {
                        Some(found) => Ok(Found {
                            rule: "java.call.super-constructor",
                            ..found
                        }),
                        None if class_sym.bases.is_empty() => Err(Miss {
                            rule: "builtin",
                            reason: String::new(),
                            builtin: true,
                            unknown_receiver: false,
                        }),
                        None => Err(Miss::new(
                            "java.call.super-constructor",
                            format!(
                                "superclass of `{}` is not resolved",
                                class_sym.qualified_name
                            ),
                        )),
                    },
                };
            }
            let name = path[1].as_str();
            let found = if is_super {
                self.inherited_member((fi, class), name, 0)
            } else {
                self.member((fi, class), name, 0)
            };
            let rule = if is_super {
                "java.call.super-method"
            } else {
                "java.call.this-member"
            };
            return match found {
                Some((m, inherited)) => {
                    // `super.m()` is not virtually dispatched.
                    let confidence = if is_super {
                        Confidence::Confirmed
                    } else {
                        self.java_member_confidence(m, inherited)
                    };
                    let base = Found {
                        target: Target::Symbol(m.0, m.1),
                        confidence,
                        rule,
                    };
                    self.descend(base, &path[2..])
                }
                None => match self.java_external_base((fi, class), 0) {
                    Some(found) => Ok(found),
                    None => Err(Miss::new(
                        rule,
                        format!(
                            "`{name}` is not a member of `{}` or its resolved supertypes",
                            ir.symbols[class as usize].qualified_name
                        ),
                    )),
                },
            };
        }

        // Locals and parameters of enclosing methods and lambdas.
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
                        "java.typed-value",
                    );
                }
                if let Some(res) =
                    self.call_result_value(fi, id, first, &path[1..], "java.call-result")
                {
                    return res;
                }
                return Err(Miss::receiver(
                    "scope.local-binding",
                    format!(
                        "`{first}` is a local binding of `{}` without a declared type; its value is not tracked",
                        sym.qualified_name
                    ),
                ));
            }
            cur = sym.parent;
        }

        // Members of enclosing types, innermost first, including inherited.
        let mut cur = scope;
        let mut innermost_type = None;
        while let Some(id) = cur {
            let sym = &ir.symbols[id as usize];
            if matches!(sym.kind, SymbolKind::Class | SymbolKind::Interface) {
                innermost_type.get_or_insert(id);
                if let Some((m, inherited)) = self.member((fi, id), first, 0) {
                    let base = Found {
                        target: Target::Symbol(m.0, m.1),
                        confidence: self.java_member_confidence(m, inherited),
                        rule: "java.scope.member",
                    };
                    return self.descend(base, &path[1..]);
                }
            }
            cur = sym.parent;
        }

        // Types of this compilation unit.
        if let Some(found) = self.top_symbol(fi, first, "java.scope.file-type") {
            return self.descend(found, &path[1..]);
        }
        // Single-type and single-static imports.
        if let Some(&imp) = file.bindings.get(&(None, first)) {
            let base = self.java_import(fi, imp)?;
            return self.descend(base, &path[1..]);
        }
        // Types of the same package; in C#, of each enclosing namespace from
        // the innermost outwards.
        let mut namespace = self.java_namespace(fi, scope);
        loop {
            let fqn = if namespace.is_empty() {
                first.to_string()
            } else {
                format!("{namespace}.{first}")
            };
            if let Some(res) = self.java_type(&fqn, "java.scope.package-type") {
                return self.descend(res?, &path[1..]);
            }
            if !csharp || namespace.is_empty() {
                break;
            }
            namespace = match namespace.rsplit_once('.') {
                Some((outer, _)) => outer.to_string(),
                None => String::new(),
            };
        }
        // On-demand imports, including a .NET project's implicit usings.
        let mut hits: Vec<Found> = Vec::new();
        let mut external_wildcards: Vec<&str> = Vec::new();
        let mut modules: Vec<&str> = ir
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Wildcard)
            .map(|i| i.module.as_str())
            .collect();
        if csharp {
            if let Some(p) = self
                .java
                .projects
                .iter()
                .find(|p| crate::paths::is_under(file.path, &p.0))
            {
                modules.extend(p.1.iter().map(String::as_str));
            }
            modules.sort_unstable();
            modules.dedup();
        }
        for module in modules {
            let found = if self.java.is_package(module) {
                self.java_type(&format!("{module}.{first}"), "java.import.on-demand")
                    .and_then(Result::ok)
            } else if let Some(Ok(Found {
                target: Target::Symbol(tf, tid),
                ..
            })) = self.java_type(module, "java.import.on-demand")
            {
                // `import static a.b.C.*;`
                self.member((tf, tid), first, 0)
                    .map(|(m, inherited)| Found {
                        target: Target::Symbol(m.0, m.1),
                        confidence: if inherited {
                            Confidence::Probable
                        } else {
                            Confidence::Confirmed
                        },
                        rule: "java.import.static-on-demand",
                    })
            } else {
                external_wildcards.push(module);
                None
            };
            if let Some(f) = found {
                if !hits.iter().any(|h| h.target == f.target) {
                    hits.push(f);
                }
            }
        }
        match hits.len() {
            0 => {}
            1 => return self.descend(hits.remove(0), &path[1..]),
            n => {
                return Err(Miss::new(
                    "java.import.on-demand",
                    format!("`{first}` is provided by {n} on-demand imports; ambiguous"),
                ))
            }
        }
        let builtin = match file.family {
            LanguageFamily::Kotlin => {
                builtins::is_kotlin_builtin(first) || builtins::is_java_lang(first)
            }
            LanguageFamily::Csharp => builtins::is_csharp_builtin(first),
            _ => builtins::is_java_lang(first),
        };
        if builtin {
            return Err(Miss {
                rule: "builtin",
                reason: String::new(),
                builtin: true,
                unknown_receiver: false,
            });
        }
        // Fully qualified names: the longest prefix that names a type.
        if path.len() > 1 && !starts_upper(first) {
            for k in (2..=path.len()).rev() {
                if let Some(res) = self.java_type(&path[..k].join("."), "java.scope.qualified-name")
                {
                    return self.descend(res?, &path[k..]);
                }
            }
            let package: Vec<&str> = path
                .iter()
                .map(String::as_str)
                .take_while(|s| !starts_upper(s))
                .collect();
            if package.len() >= 2 && path.len() > package.len() {
                return Ok(self.java_external(fi, &path.join(".")));
            }
        }
        // A type from on-demand imports of external packages that all map to
        // the same external node (one package, or several `System.*`
        // namespaces).
        if starts_upper(first) && !external_wildcards.is_empty() {
            let nodes: Vec<Found> = external_wildcards
                .iter()
                .map(|m| self.java_external(fi, m))
                .collect();
            if nodes.iter().all(|n| n.target == nodes[0].target) {
                return Ok(Found {
                    confidence: Confidence::Probable,
                    rule: "java.import.on-demand-external",
                    ..nodes[0].clone()
                });
            }
        }
        // Well-known `System` types: SDK-style projects import their
        // namespaces implicitly, and no repository declaration matched.
        if csharp && builtins::is_dotnet_implicit_type(first) {
            return Ok(Found {
                confidence: Confidence::Probable,
                rule: "java.import.dotnet-system-type",
                ..self.dotnet_external("System")
            });
        }
        // An unqualified method inherited from a supertype outside the repository.
        if let Some(class) = innermost_type {
            if let Some(found) = self.java_external_base((fi, class), 0) {
                return Ok(found);
            }
        }
        Err(Miss::new(
            "scope.unbound",
            if external_wildcards.is_empty() {
                format!("no declaration or import of `{first}` is visible here")
            } else {
                format!("no declaration of `{first}` is visible here; it may come from an on-demand import of a package outside the repository")
            },
        ))
    }
}
