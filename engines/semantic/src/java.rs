//! Java resolution rules.
//!
//! Types are identified by fully qualified name (package declaration plus
//! the nesting of type declarations), independently of file paths. Simple
//! names are resolved in the order of the Java Language Specification
//! (§6.4, §7.5): locals, members of enclosing types (including inherited
//! ones), types of the compilation unit, single-type and single-static
//! imports, types of the same package, on-demand imports, then `java.lang`.

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
    /// Declared Maven group ids, longest first.
    groups: Vec<String>,
}

impl JavaIndex {
    pub(crate) fn new(config: &ProjectConfig, files: &[FileIx<'_>]) -> Self {
        let mut index = JavaIndex {
            types: HashMap::new(),
            packages: HashMap::new(),
            package_of: HashMap::new(),
            package_files: HashMap::new(),
            groups: Vec::new(),
        };
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
            if f.family != LanguageFamily::Java {
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
                if !matches!(sym.kind, SymbolKind::Class | SymbolKind::Interface) {
                    continue;
                }
                // Member types only: local and anonymous classes have no
                // qualified name.
                let mut p = sym.parent;
                let mut member = true;
                while let Some(id) = p {
                    let ps = &f.ir.symbols[id as usize];
                    if !matches!(ps.kind, SymbolKind::Class | SymbolKind::Interface) {
                        member = false;
                        break;
                    }
                    p = ps.parent;
                }
                if !member {
                    continue;
                }
                let fqn = if package.is_empty() {
                    sym.qualified_name.clone()
                } else {
                    format!("{package}.{}", sym.qualified_name)
                };
                index.types.entry(fqn).or_default().push((fi, sym.id));
                if sym.parent.is_none() {
                    index
                        .packages
                        .entry(package.clone())
                        .or_default()
                        .push((fi, sym.id));
                }
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
        let decls = self.java.types.get(fqn)?;
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

    /// An external package node for a Java package or type name.
    fn java_external(&self, name: &str) -> Found {
        let package: Vec<&str> = name.split('.').take_while(|s| !starts_upper(s)).collect();
        let package = package.join(".");
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

    /// Resolve the import at index `idx` of Java file `fi`.
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
                Ok(self.java_external(&fqn))
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
                Ok(self.java_external(&imp.module))
            }
            _ => Err(Miss::new("java.import", "unsupported import form")),
        }
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
    pub(crate) fn java_member_confidence(&self, member: SymRef, inherited: bool) -> Confidence {
        let sym = self.sym(member);
        let fixed = !sym.kind.is_callable()
            || sym.attrs.contains_key("static")
            || sym.attrs.contains_key("constructor")
            || sym
                .attrs
                .get("modifiers")
                .and_then(|m| m.as_array())
                .is_some_and(|m| m.iter().any(|k| k == "private" || k == "final"));
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

        // `this` and `super`.
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
        // Types of the same package.
        let package = self
            .java
            .package_of
            .get(&fi)
            .map(String::as_str)
            .unwrap_or("");
        let fqn = if package.is_empty() {
            first.to_string()
        } else {
            format!("{package}.{first}")
        };
        if let Some(res) = self.java_type(&fqn, "java.scope.package-type") {
            return self.descend(res?, &path[1..]);
        }
        // On-demand imports.
        let mut hits: Vec<Found> = Vec::new();
        let mut external_wildcards: Vec<usize> = Vec::new();
        for (idx, imp) in ir.imports.iter().enumerate() {
            if imp.kind != ImportKind::Wildcard {
                continue;
            }
            let found = if self.java.is_package(&imp.module) {
                self.java_type(&format!("{}.{first}", imp.module), "java.import.on-demand")
                    .and_then(Result::ok)
            } else if let Some(Ok(Found {
                target: Target::Symbol(tf, tid),
                ..
            })) = self.java_type(&imp.module, "java.import.on-demand")
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
                external_wildcards.push(idx);
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
        if builtins::is_java_lang(first) {
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
                return Ok(self.java_external(&path.join(".")));
            }
        }
        // A type from exactly one on-demand import of an external package.
        if starts_upper(first) && external_wildcards.len() == 1 {
            let module = &ir.imports[external_wildcards[0]].module;
            let found = self.java_external(module);
            return Ok(Found {
                confidence: Confidence::Probable,
                rule: "java.import.on-demand-external",
                ..found
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
