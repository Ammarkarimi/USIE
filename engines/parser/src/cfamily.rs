//! C and C++ extraction adapter (tree-sitter-c, tree-sitter-cpp).
//!
//! Conventions in the IR produced here:
//!
//! * `#include "x.h"` is a `SideEffect` import of `x.h` with `level` 0;
//!   `#include <x.h>` has `level` 1.
//! * Functions are `Function` symbols; prototypes carry
//!   `attrs.declaration`, definitions with a body do not. `static`
//!   functions and variables have `attrs.static` and are not exported.
//! * Structs, unions, enums and classes are `Class` symbols; typedefs and
//!   `using X = Y` are `Type` symbols with `attrs.underlying`; C++
//!   namespaces are `Namespace` symbols.
//! * Member functions declared in a class are `Method` children of the
//!   class (`attrs.declaration` when without a body, `attrs.virtual` when
//!   declared `virtual`). Definitions outside the class (`void Cart::add()`)
//!   are methods at namespace level, qualified `Cart.add`, with
//!   `attrs.receiver_type` and `attrs.impl_type` (the qualifying path).
//! * Function-like and object-like macros are `Function` and `Variable`
//!   symbols with `attrs.macro`.
//! * `using namespace N;` is a `Wildcard` import of `N`; `using N::x;` a
//!   `Named` import.

use crate::common::{
    children, doc_comment, named_children, span, Builder, Cx, Extracted, Limits, LocalTypes,
    Target, MAX_CHAIN, MAX_DEPTH,
};
use tree_sitter::Node;
use usie_ir::{Import, ImportKind, Language, ReferenceKind, SymbolKind};

pub(crate) fn extract(root: Node, src: &[u8], _language: Language, limits: Limits) -> Extracted {
    let mut c = Cf {
        b: Builder::new(src, limits),
        types: LocalTypes::default(),
    };
    c.walk(root, Cx::default(), 0, None);
    let Cf { mut b, types } = c;
    types.apply(&mut b);
    b.finish()
}

struct Cf<'a> {
    b: Builder<'a>,
    types: LocalTypes,
}

impl<'a> Cf<'a> {
    fn walk_children(&mut self, node: Node, cx: Cx, depth: u32, class: Option<&[String]>) {
        for child in children(node) {
            self.walk(child, cx, depth + 1, class);
        }
    }

    /// `class` is the qualifying class of the method being walked (for
    /// `this`), as written.
    fn walk(&mut self, node: Node, cx: Cx, depth: u32, class: Option<&[String]>) {
        self.b.nodes += 1;
        if depth > MAX_DEPTH {
            self.b.truncated = true;
            return;
        }
        match node.kind() {
            "preproc_include" => {
                if let Some(p) = node.child_by_field_name("path") {
                    let raw = self.b.text(p);
                    let system = p.kind() == "system_lib_string";
                    let path = raw
                        .trim_matches(|c| c == '"' || c == '<' || c == '>')
                        .to_string();
                    if !path.is_empty() {
                        self.b.push_import(Import {
                            module: path,
                            level: u32::from(system),
                            kind: ImportKind::SideEffect,
                            imported: None,
                            local: None,
                            scope: None,
                            type_only: false,
                            span: span(node),
                        });
                    }
                }
            }
            "preproc_def" | "preproc_function_def" => {
                if cx.fn_scope.is_some() {
                    return;
                }
                if let Some(n) = node.child_by_field_name("name") {
                    let name = self.b.text(n);
                    let kind = if node.kind() == "preproc_function_def" {
                        SymbolKind::Function
                    } else {
                        SymbolKind::Variable
                    };
                    if !self.b.has_child(cx.scope, &name, kind) {
                        let sig = node
                            .child_by_field_name("parameters")
                            .map(|p| self.b.text_truncated(p));
                        if let Some(id) = self.b.define(kind, name, cx.scope, node, n) {
                            let sym = self.b.sym(id);
                            sym.exported = true;
                            sym.signature = sig;
                            sym.attrs.insert("macro".into(), true.into());
                        }
                    }
                }
            }
            "namespace_definition" => {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| self.b.text(n).replace("::", "."))
                    .unwrap_or_default();
                let Some(body) = node.child_by_field_name("body") else {
                    return;
                };
                if name.is_empty() {
                    // Anonymous namespace: file-local declarations.
                    self.walk_children(body, cx, depth, class);
                    return;
                }
                let name_node = node.child_by_field_name("name").unwrap_or(node);
                let Some(id) =
                    self.b
                        .define(SymbolKind::Namespace, name, cx.scope, node, name_node)
                else {
                    return;
                };
                self.b.sym(id).exported = true;
                let inner = Cx {
                    scope: Some(id),
                    ..cx
                };
                self.walk_children(body, inner, depth + 1, None);
            }
            "using_declaration" => {
                let is_namespace = children(node).iter().any(|c| c.kind() == "namespace");
                if let Some(target) = named_children(node).into_iter().last() {
                    let segs = self.qualified(target);
                    if segs.is_empty() {
                        return;
                    }
                    let import = if is_namespace {
                        Import {
                            module: segs.join("::"),
                            level: 0,
                            kind: ImportKind::Wildcard,
                            imported: None,
                            local: None,
                            scope: cx.fn_scope,
                            type_only: false,
                            span: span(node),
                        }
                    } else {
                        let last = segs.last().cloned().unwrap_or_default();
                        Import {
                            module: segs[..segs.len() - 1].join("::"),
                            level: 0,
                            kind: ImportKind::Named,
                            imported: Some(last.clone()),
                            local: Some(last),
                            scope: cx.fn_scope,
                            type_only: false,
                            span: span(node),
                        }
                    };
                    self.b.push_import(import);
                }
            }
            "alias_declaration" => {
                if let (Some(n), Some(t)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("type"),
                ) {
                    let name = self.b.text(n);
                    let underlying = self.type_path(t);
                    if let Some(id) = self.b.define(SymbolKind::Type, name, cx.scope, node, n) {
                        let sym = self.b.sym(id);
                        sym.exported = true;
                        sym.attrs.insert("type_kind".into(), "alias".into());
                        if let Some(u) = underlying {
                            sym.attrs.insert("underlying".into(), u.into());
                        }
                    }
                }
            }
            "template_declaration"
            | "linkage_specification"
            | "declaration_list"
            | "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif" => self.walk_children(node, cx, depth, class),
            "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => {
                self.record(node, cx, depth, None);
            }
            "type_definition" => self.typedef(node, cx, depth),
            "function_definition" => self.function(node, cx, depth),
            "declaration" | "field_declaration" => self.declaration(node, cx, depth, class),
            "for_range_loop" => {
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|t| self.type_path(t));
                if let Some(d) = node.child_by_field_name("declarator") {
                    if let Some(n) = declarator_name(d) {
                        self.bind(cx, n, ty);
                    }
                }
                for f in ["right", "body"] {
                    if let Some(n) = node.child_by_field_name(f) {
                        self.walk(n, cx, depth + 1, class);
                    }
                }
            }
            "lambda_expression" => {
                if let Some(d) = node.child_by_field_name("declarator") {
                    if let Some(params) = d.child_by_field_name("parameters") {
                        self.parameters(params, cx);
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(body, cx, depth + 1, class);
                }
            }
            "call_expression" => {
                if let Some(function) = node.child_by_field_name("function") {
                    let target = self.target(function);
                    self.b
                        .push_reference(ReferenceKind::Call, cx, target, function, function);
                    self.walk(function, cx, depth + 1, class);
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    self.walk(args, cx, depth + 1, class);
                }
            }
            "new_expression" | "compound_literal_expression" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    if let Some(path) = self.type_path(ty) {
                        let target = Target {
                            path,
                            ..Default::default()
                        };
                        self.b
                            .push_reference(ReferenceKind::New, cx, target, ty, ty);
                    }
                }
                for f in ["arguments", "value"] {
                    if let Some(n) = node.child_by_field_name(f) {
                        self.walk(n, cx, depth + 1, class);
                    }
                }
            }
            "field_expression" => {
                if let Some(a) = node.child_by_field_name("argument") {
                    self.walk(a, cx, depth + 1, class);
                }
            }
            _ => self.walk_children(node, cx, depth, class),
        }
    }

    fn bind(&mut self, cx: Cx, name: Node, ty: Option<Vec<String>>) {
        let name = self.b.text(name);
        match cx.fn_scope {
            Some(_) => {
                self.b.add_local(cx.fn_scope, &name);
                self.types.bind(cx.fn_scope, &name, ty);
            }
            None => self.b.add_anonymous_local(&name),
        }
    }

    fn doc(&self, node: Node) -> Option<String> {
        let d = doc_comment(&self.b, node)?;
        let d = d.trim_start_matches(['/', '!', '<']).trim();
        (!d.is_empty()).then(|| d.to_string())
    }

    /// A struct/union/enum/class with a body; returns its symbol.
    fn record(
        &mut self,
        node: Node,
        cx: Cx,
        depth: u32,
        typedef_name: Option<(String, Node)>,
    ) -> Option<u32> {
        let body = node.child_by_field_name("body")?;
        let name_node = node.child_by_field_name("name");
        let (name, name_node) = match (name_node, &typedef_name) {
            (Some(n), _) => (self.qualified(n).last().cloned().unwrap_or_default(), n),
            (None, Some((t, n))) => (t.clone(), *n),
            (None, None) => {
                // Anonymous struct/enum: walk members for references only.
                self.walk_children(body, cx, depth, None);
                return None;
            }
        };
        let type_kind = match node.kind() {
            "union_specifier" => "union",
            "enum_specifier" => "enum",
            "class_specifier" => "class",
            _ => "struct",
        };
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, &name);
        }
        let doc = self
            .doc(
                node.parent()
                    .filter(|p| p.kind() != "translation_unit" && p.kind() != "declaration_list")
                    .unwrap_or(node),
            )
            .or_else(|| self.doc(node));
        let id = self
            .b
            .define(SymbolKind::Class, name.clone(), cx.scope, node, name_node)?;
        {
            let sym = self.b.sym(id);
            sym.exported = true;
            sym.doc = doc;
            sym.attrs.insert("type_kind".into(), type_kind.into());
        }
        for c in named_children(node) {
            if c.kind() == "base_class_clause" {
                for t in named_children(c) {
                    if let Some(segments) = self.type_path(t) {
                        let base = self.b.name_path(t, segments);
                        self.b.sym(id).bases.push(base);
                    }
                }
            }
        }
        let inner = Cx {
            scope: Some(id),
            fn_scope: None,
            exported: false,
            anon: false,
        };
        let class_path = vec![name.clone()];
        for member in named_children(body) {
            match member.kind() {
                "enumerator" => {
                    if let Some(n) = member.child_by_field_name("name") {
                        let ename = self.b.text(n);
                        if let Some(eid) =
                            self.b
                                .define(SymbolKind::Variable, ename, Some(id), member, n)
                        {
                            let sym = self.b.sym(eid);
                            sym.exported = true;
                            sym.attrs.insert("member".into(), true.into());
                            sym.attrs.insert("static".into(), true.into());
                            sym.attrs.insert("enum_constant".into(), true.into());
                        }
                    }
                }
                _ => self.walk(member, inner, depth + 2, Some(&class_path)),
            }
        }
        Some(id)
    }

    fn typedef(&mut self, node: Node, cx: Cx, depth: u32) {
        let ty = node.child_by_field_name("type");
        let mut cursor = node.walk();
        let declarators: Vec<Node> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        let first_name = declarators
            .first()
            .and_then(|d| declarator_name(*d))
            .map(|n| (self.b.text(n), n));
        // `typedef struct { ... } name;` declares the struct under `name`.
        let mut underlying = ty.and_then(|t| self.type_path(t));
        if let Some(t) = ty {
            if t.child_by_field_name("body").is_some() {
                let anonymous = t.child_by_field_name("name").is_none();
                let rec = self.record(
                    t,
                    cx,
                    depth + 1,
                    if anonymous { first_name.clone() } else { None },
                );
                if anonymous && rec.is_some() {
                    return;
                }
                if rec.is_some() {
                    underlying = t.child_by_field_name("name").map(|n| vec![self.b.text(n)]);
                }
            }
        }
        for d in declarators {
            let Some(n) = declarator_name(d) else {
                continue;
            };
            let name = self.b.text(n);
            // A pointer or function typedef has no members of its own.
            let plain = n.id() == d.id() || d.kind() == "type_identifier";
            if let Some(id) = self.b.define(SymbolKind::Type, name, cx.scope, node, n) {
                let doc = self.doc(node);
                let sym = self.b.sym(id);
                sym.exported = true;
                sym.doc = doc;
                sym.attrs.insert("type_kind".into(), "alias".into());
                if plain || d.kind() == "pointer_declarator" {
                    if let Some(u) = &underlying {
                        sym.attrs.insert("underlying".into(), u.clone().into());
                    }
                }
            }
        }
    }

    fn function(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(decl) = node.child_by_field_name("declarator") else {
            return;
        };
        let Some(fdecl) = function_declarator(decl) else {
            self.walk_children(node, cx, depth, None);
            return;
        };
        let Some(name_node) = fdecl.child_by_field_name("declarator") else {
            return;
        };
        let segs = self.qualified(name_node);
        let Some(name) = segs.last().cloned() else {
            return;
        };
        let qualifier: Vec<String> = segs[..segs.len() - 1].to_vec();
        let in_class = cx
            .scope
            .is_some_and(|s| self.b.symbols[s as usize].kind == SymbolKind::Class);
        let is_static = has_storage(node, &self.b, "static");
        let kind = if in_class || !qualifier.is_empty() {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        let doc = self.doc(node);
        let Some(id) = self.b.define(kind, name.clone(), cx.scope, node, name_node) else {
            return;
        };
        let ret = node.child_by_field_name("type");
        let result = ret
            .and_then(|t| self.type_path(t))
            .filter(|t| !(t.len() == 1 && t[0] == "void"));
        let params = fdecl.child_by_field_name("parameters");
        {
            let mut signature = String::new();
            if let Some(r) = ret {
                signature.push_str(&self.b.text_truncated(r));
                signature.push(' ');
            }
            signature.push_str(&params.map(|p| self.b.text_truncated(p)).unwrap_or_default());
            let sym = self.b.sym(id);
            sym.signature = Some(signature);
            sym.doc = doc;
            sym.exported = !is_static;
            if is_static {
                sym.attrs.insert("static".into(), true.into());
            }
            if let Some(r) = result {
                sym.attrs.insert(
                    "result_types".into(),
                    vec![serde_json::Value::from(r)].into(),
                );
            }
            if !qualifier.is_empty() && !in_class {
                let receiver = qualifier.last().cloned().unwrap_or_default();
                sym.qualified_name = match sym.parent {
                    Some(_) => {
                        let prefix = sym
                            .qualified_name
                            .strip_suffix(&name)
                            .unwrap_or("")
                            .to_string();
                        format!("{prefix}{receiver}.{name}")
                    }
                    None => format!("{receiver}.{name}"),
                };
                sym.attrs.insert("receiver_type".into(), receiver.into());
                sym.attrs
                    .insert("impl_type".into(), qualifier.clone().into());
            }
        }
        let inner = Cx {
            scope: Some(id),
            fn_scope: Some(id),
            exported: false,
            anon: false,
        };
        if let Some(p) = params {
            self.parameters(p, inner);
        }
        let class: Option<Vec<String>> = if !qualifier.is_empty() {
            Some(qualifier)
        } else if in_class {
            cx.scope
                .map(|s| vec![self.b.symbols[s as usize].name.clone()])
        } else {
            None
        };
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, inner, depth + 1, class.as_deref());
        }
        // Constructor initializer lists.
        for c in named_children(node) {
            if c.kind() == "field_initializer_list" {
                self.walk(c, inner, depth + 1, class.as_deref());
            }
        }
    }

    fn parameters(&mut self, params: Node, cx: Cx) {
        for p in named_children(params) {
            if !matches!(
                p.kind(),
                "parameter_declaration" | "optional_parameter_declaration"
            ) {
                continue;
            }
            let ty = p
                .child_by_field_name("type")
                .and_then(|t| self.type_path(t));
            if let Some(d) = p.child_by_field_name("declarator") {
                if let Some(n) = declarator_name(d) {
                    self.bind(cx, n, ty);
                }
            }
        }
    }

    /// Prototypes, variables and fields.
    fn declaration(&mut self, node: Node, cx: Cx, depth: u32, class: Option<&[String]>) {
        let ty = node.child_by_field_name("type");
        if let Some(t) = ty {
            if t.child_by_field_name("body").is_some() {
                self.record(t, cx, depth + 1, None);
            }
        }
        let is_auto = ty.is_some_and(|t| t.kind() == "placeholder_type_specifier");
        let type_path = if is_auto {
            None
        } else {
            ty.and_then(|t| self.type_path(t))
        };
        let in_class = cx
            .scope
            .is_some_and(|s| self.b.symbols[s as usize].kind == SymbolKind::Class);
        let is_static = has_storage(node, &self.b, "static");
        let is_virtual = children(node)
            .iter()
            .any(|c| c.kind() == "virtual" || (c.kind() == "virtual_function_specifier"));
        let mut cursor = node.walk();
        let declarators: Vec<Node> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        for d in declarators {
            // Prototype or member function declaration.
            if let Some(fdecl) = function_declarator(d) {
                if cx.fn_scope.is_some() {
                    continue;
                }
                let Some(name_node) = fdecl.child_by_field_name("declarator") else {
                    continue;
                };
                let segs = self.qualified(name_node);
                let Some(name) = segs.last().cloned() else {
                    continue;
                };
                if segs.len() > 1 {
                    continue;
                }
                let kind = if in_class {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                let doc = self.doc(node);
                let result = ty
                    .and_then(|t| self.type_path(t))
                    .filter(|t| !(t.len() == 1 && t[0] == "void"));
                let sig = fdecl
                    .child_by_field_name("parameters")
                    .map(|p| self.b.text_truncated(p));
                // `override` also makes a member function virtual; `final`
                // forbids further overriding.
                let specifiers: Vec<String> = named_children(fdecl)
                    .into_iter()
                    .filter(|c| c.kind() == "virtual_specifier")
                    .map(|c| self.b.text(c))
                    .collect();
                let is_virtual = (is_virtual || specifiers.iter().any(|s| s == "override"))
                    && !specifiers.iter().any(|s| s == "final");
                if let Some(id) = self.b.define(kind, name, cx.scope, node, name_node) {
                    let sym = self.b.sym(id);
                    sym.exported = !is_static;
                    sym.doc = doc;
                    sym.signature = sig;
                    sym.attrs.insert("declaration".into(), true.into());
                    if is_static {
                        sym.attrs.insert("static".into(), true.into());
                    }
                    if is_virtual {
                        sym.attrs.insert("virtual".into(), true.into());
                    }
                    if let Some(r) = result {
                        sym.attrs.insert(
                            "result_types".into(),
                            vec![serde_json::Value::from(r)].into(),
                        );
                    }
                }
                continue;
            }
            let (name_node, value) = if d.kind() == "init_declarator" {
                (
                    d.child_by_field_name("declarator")
                        .and_then(declarator_name),
                    d.child_by_field_name("value"),
                )
            } else {
                (declarator_name(d), None)
            };
            let Some(n) = name_node else { continue };
            if cx.fn_scope.is_some() {
                match (&type_path, value) {
                    (Some(t), _) => self.bind(cx, n, Some(t.clone())),
                    (None, Some(v)) if v.kind() == "call_expression" => {
                        let callee = v
                            .child_by_field_name("function")
                            .map(|f| self.target(f).path)
                            .unwrap_or_default();
                        let name = self.b.text(n);
                        if !callee.is_empty() {
                            self.b.add_local(cx.fn_scope, &name);
                            self.types.bind_call(cx.fn_scope, &name, callee, 0);
                        } else {
                            self.bind(cx, n, None);
                        }
                    }
                    (None, Some(v)) if v.kind() == "new_expression" => {
                        let t = v
                            .child_by_field_name("type")
                            .and_then(|t| self.type_path(t));
                        self.bind(cx, n, t);
                    }
                    _ => self.bind(cx, n, None),
                }
            } else {
                let name = self.b.text(n);
                let doc = self.doc(node);
                let sig = ty.map(|t| self.b.text_truncated(t));
                let is_extern = has_storage(node, &self.b, "extern");
                if let Some(id) = self.b.define(SymbolKind::Variable, name, cx.scope, node, n) {
                    let sym = self.b.sym(id);
                    sym.exported = !is_static;
                    sym.doc = doc;
                    sym.signature = sig;
                    if in_class {
                        sym.attrs.insert("member".into(), true.into());
                    }
                    if is_static {
                        sym.attrs.insert("static".into(), true.into());
                    }
                    if is_extern {
                        sym.attrs.insert("declaration".into(), true.into());
                    }
                    if let Some(t) = &type_path {
                        sym.attrs.insert("type_path".into(), t.clone().into());
                    }
                }
            }
            if let Some(v) = value {
                self.walk(v, cx, depth + 1, class);
            }
        }
    }

    /// Segments of a possibly qualified name (`a::b::c`), without template
    /// arguments; `~X` and `operator+` are kept as written.
    fn qualified(&self, node: Node) -> Vec<String> {
        match node.kind() {
            "qualified_identifier" | "nested_namespace_specifier" => {
                let mut out = Vec::new();
                if let Some(s) = node.child_by_field_name("scope") {
                    out.extend(self.qualified(s));
                } else if node.kind() == "nested_namespace_specifier" {
                    for c in named_children(node) {
                        out.extend(self.qualified(c));
                    }
                    return out;
                }
                if let Some(n) = node.child_by_field_name("name") {
                    out.extend(self.qualified(n));
                }
                out
            }
            "template_type" | "template_function" | "template_method" => node
                .child_by_field_name("name")
                .map(|n| self.qualified(n))
                .unwrap_or_default(),
            _ => vec![self.b.text(node)],
        }
    }

    /// Segments of a named type; `None` for function pointers and arrays.
    fn type_path(&self, ty: Node) -> Option<Vec<String>> {
        match ty.kind() {
            "type_identifier" | "primitive_type" | "sized_type_specifier" => {
                Some(vec![self.b.text(ty)])
            }
            "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => {
                ty.child_by_field_name("name").map(|n| self.qualified(n))
            }
            "qualified_identifier" | "template_type" => {
                let q = self.qualified(ty);
                (!q.is_empty()).then_some(q)
            }
            "type_descriptor" => self.type_path(ty.child_by_field_name("type")?),
            _ => None,
        }
    }

    fn dotted(&self, expr: Node) -> Option<Vec<String>> {
        match expr.kind() {
            "identifier" | "field_identifier" => Some(vec![self.b.text(expr)]),
            "this" => Some(vec!["this".into()]),
            "qualified_identifier" | "template_function" => {
                let q = self.qualified(expr);
                (!q.is_empty()).then_some(q)
            }
            "field_expression" => {
                let mut path = self.dotted(expr.child_by_field_name("argument")?)?;
                let field = expr.child_by_field_name("field")?;
                path.extend(self.qualified(field));
                Some(path)
            }
            "parenthesized_expression" => self.dotted(expr.named_child(0)?),
            "pointer_expression" => {
                // `(*p).f`
                self.dotted(expr.child_by_field_name("argument")?)
            }
            _ => None,
        }
    }

    fn target(&self, function: Node) -> Target {
        if let Some(path) = self.dotted(function) {
            return Target {
                path,
                ..Default::default()
            };
        }
        if function.kind() == "field_expression" {
            let member = function
                .child_by_field_name("field")
                .map(|f| self.qualified(f))
                .and_then(|q| q.last().cloned());
            let chain = match (function.child_by_field_name("argument"), &member) {
                (Some(a), Some(m)) => self
                    .chain(a, 0)
                    .map(|mut c| {
                        c.push(m.clone());
                        c
                    })
                    .filter(|c| c.len() <= MAX_CHAIN)
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            return Target {
                path: Vec::new(),
                member,
                chain,
            };
        }
        Target::default()
    }

    fn chain(&self, expr: Node, depth: u32) -> Option<Vec<String>> {
        if depth > MAX_CHAIN as u32 {
            return None;
        }
        match expr.kind() {
            "identifier" | "this" | "qualified_identifier" => self.dotted(expr),
            "field_expression" => {
                let mut c = self.chain(expr.child_by_field_name("argument")?, depth + 1)?;
                c.push(self.b.text(expr.child_by_field_name("field")?));
                Some(c)
            }
            "call_expression" => {
                let mut c = self.chain(expr.child_by_field_name("function")?, depth + 1)?;
                c.push("()".into());
                Some(c)
            }
            "new_expression" | "compound_literal_expression" | "cast_expression" => {
                let ty = self.type_path(expr.child_by_field_name("type")?)?;
                Some(vec![format!("type:{}", ty.join("."))])
            }
            "parenthesized_expression" => self.chain(expr.named_child(0)?, depth + 1),
            _ => None,
        }
    }
}

/// The function declarator inside pointer/reference declarators.
fn function_declarator(d: Node) -> Option<Node> {
    let mut cur = d;
    for _ in 0..16 {
        match cur.kind() {
            "function_declarator" => return Some(cur),
            "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" => {
                cur = cur
                    .child_by_field_name("declarator")
                    .or(cur.named_child(0))?;
            }
            _ => return None,
        }
    }
    None
}

/// The identifier a declarator declares.
fn declarator_name(d: Node) -> Option<Node> {
    let mut cur = d;
    for _ in 0..16 {
        match cur.kind() {
            "identifier" | "field_identifier" | "type_identifier" => return Some(cur),
            "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "init_declarator"
            | "parenthesized_declarator"
            | "attributed_declarator" => {
                cur = cur.child_by_field_name("declarator").or_else(|| {
                    named_children(cur)
                        .into_iter()
                        .find(|c| c.kind() != "type_qualifier")
                })?;
            }
            _ => return None,
        }
    }
    None
}

fn has_storage(node: Node, b: &Builder, word: &str) -> bool {
    named_children(node)
        .iter()
        .any(|c| c.kind() == "storage_class_specifier" && b.text(*c) == word)
}
