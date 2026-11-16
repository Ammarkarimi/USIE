//! Rust extraction adapter (tree-sitter-rust).
//!
//! Conventions in the IR produced here:
//!
//! * Inline modules (`mod m { ... }`) are `Namespace` symbols; file modules
//!   are derived from file paths by the resolver.
//! * Structs, enums and unions are `Class` symbols (`attrs.type_kind`);
//!   traits are `Interface` symbols whose supertraits are `bases`; type
//!   aliases are `Type`.
//! * Functions in `impl` blocks are methods at the level of the enclosing
//!   module, qualified `Type.method`, with `attrs.receiver_type` (last
//!   segment), `attrs.impl_type` (the self type's path) and, for trait
//!   impls, `attrs.impl_trait`. Trait methods are children of the trait.
//! * `exported` means declared `pub` (any visibility modifier).
//! * `use` trees become one import per leaf: `Named` (`module` = path with
//!   `::`, `imported` = last segment, `local` = alias or last segment) or
//!   `Wildcard` for globs.
//! * In reference paths, a module-path `self` is written `mod:self`; the
//!   value receiver is `self`. `Self` names the impl's type.
//! * Macro invocations are not recorded.

use crate::common::{
    children, named_children, span, truncate, Builder, Cx, Extracted, Limits, LocalTypes, Target,
    MAX_CHAIN, MAX_DEPTH,
};
use tree_sitter::Node;
use usie_ir::{Import, ImportKind, ReferenceKind, SymbolKind};

pub(crate) fn extract(root: Node, src: &[u8], limits: Limits) -> Extracted {
    let mut r = Rs {
        b: Builder::new(src, limits),
        types: LocalTypes::default(),
    };
    r.walk(root, Cx::default(), 0, None);
    let Rs { mut b, types } = r;
    types.apply(&mut b);
    b.finish()
}

struct Rs<'a> {
    b: Builder<'a>,
    types: LocalTypes,
}

/// The `impl` block being walked: self type path and trait path.
#[derive(Clone)]
struct Impl {
    ty: Vec<String>,
    tr: Option<Vec<String>>,
}

impl<'a> Rs<'a> {
    fn walk_children(&mut self, node: Node, cx: Cx, depth: u32, imp: Option<&Impl>) {
        for child in children(node) {
            self.walk(child, cx, depth + 1, imp);
        }
    }

    fn walk(&mut self, node: Node, cx: Cx, depth: u32, imp: Option<&Impl>) {
        self.b.nodes += 1;
        if depth > MAX_DEPTH {
            self.b.truncated = true;
            return;
        }
        match node.kind() {
            "mod_item" => {
                let (Some(name_node), Some(body)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("body"),
                ) else {
                    return;
                };
                let name = self.b.text(name_node);
                let doc = self.doc(node);
                let Some(id) =
                    self.b
                        .define(SymbolKind::Namespace, name, cx.scope, node, name_node)
                else {
                    return;
                };
                let sym = self.b.sym(id);
                sym.exported = has_vis(node);
                sym.doc = doc;
                let inner = Cx {
                    scope: Some(id),
                    fn_scope: None,
                    exported: false,
                    anon: false,
                };
                self.walk_children(body, inner, depth + 1, None);
            }
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    self.use_tree(arg, Vec::new(), cx, node);
                }
            }
            "function_item" | "function_signature_item" => self.function(node, cx, depth, imp),
            "struct_item" | "union_item" | "enum_item" => self.adt(node, cx),
            "trait_item" => self.r#trait(node, cx, depth),
            "impl_item" => {
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|t| self.type_path(t));
                let tr = node
                    .child_by_field_name("trait")
                    .and_then(|t| self.type_path(t));
                if let (Some(ty), Some(body)) = (ty, node.child_by_field_name("body")) {
                    let imp = Impl { ty, tr };
                    for item in named_children(body) {
                        self.walk(item, cx, depth + 1, Some(&imp));
                    }
                }
            }
            "type_item" => {
                let Some(n) = node.child_by_field_name("name") else {
                    return;
                };
                let name = self.b.text(n);
                let underlying = node
                    .child_by_field_name("type")
                    .and_then(|t| self.type_path(t));
                if let Some(id) = self.b.define(SymbolKind::Type, name, cx.scope, node, n) {
                    let doc = self.doc(node);
                    let sym = self.b.sym(id);
                    sym.exported = has_vis(node);
                    sym.doc = doc;
                    sym.attrs.insert("type_kind".into(), "alias".into());
                    if let Some(u) = underlying {
                        sym.attrs.insert("underlying".into(), u.into());
                    }
                }
            }
            "const_item" | "static_item" => {
                let Some(n) = node.child_by_field_name("name") else {
                    return;
                };
                let name = self.b.text(n);
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|t| self.type_path(t));
                if cx.fn_scope.is_some() {
                    self.bind(cx, n, ty);
                } else if let Some(id) =
                    self.b.define(SymbolKind::Variable, name, cx.scope, node, n)
                {
                    let doc = self.doc(node);
                    let sym = self.b.sym(id);
                    sym.exported = has_vis(node);
                    sym.doc = doc;
                    if node.kind() == "const_item" {
                        sym.attrs.insert("const".into(), true.into());
                    }
                    if let Some(t) = ty {
                        sym.attrs.insert("type_path".into(), t.into());
                    }
                }
                if let Some(v) = node.child_by_field_name("value") {
                    self.walk(v, cx, depth + 1, imp);
                }
            }
            "let_declaration" => {
                let pattern = node.child_by_field_name("pattern");
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|t| self.type_path(t))
                    .map(|t| self.self_type(t, imp));
                let value = node.child_by_field_name("value");
                if let Some(p) = pattern {
                    let p = if p.kind() == "mut_pattern" {
                        p.named_child(0).unwrap_or(p)
                    } else {
                        p
                    };
                    if p.kind() == "identifier" {
                        match (ty, value) {
                            (Some(t), _) => self.bind(cx, p, Some(t)),
                            (None, Some(v)) if v.kind() == "struct_expression" => {
                                let t = v
                                    .child_by_field_name("name")
                                    .and_then(|n| self.type_path(n))
                                    .map(|t| self.self_type(t, imp));
                                self.bind(cx, p, t);
                            }
                            (None, Some(v)) if v.kind() == "call_expression" => {
                                let callee = v
                                    .child_by_field_name("function")
                                    .map(|f| self.target(f, imp).path)
                                    .unwrap_or_default();
                                let name = self.b.text(p);
                                if cx.fn_scope.is_some() && !callee.is_empty() {
                                    self.b.add_local(cx.fn_scope, &name);
                                    self.types.bind_call(cx.fn_scope, &name, callee, 0);
                                } else {
                                    self.bind(cx, p, None);
                                }
                            }
                            _ => self.bind(cx, p, None),
                        }
                    } else {
                        self.bind_pattern(p, cx);
                    }
                }
                if let Some(v) = value {
                    self.walk(v, cx, depth + 1, imp);
                }
                if let Some(a) = node.child_by_field_name("alternative") {
                    self.walk(a, cx, depth + 1, imp);
                }
            }
            "let_condition" | "match_arm" | "for_expression" => {
                if let Some(p) = node.child_by_field_name("pattern") {
                    self.bind_pattern(p, cx);
                }
                for c in children(node) {
                    if Some(c.id()) != node.child_by_field_name("pattern").map(|p| p.id()) {
                        self.walk(c, cx, depth + 1, imp);
                    }
                }
            }
            "closure_expression" => {
                if let Some(params) = node.child_by_field_name("parameters") {
                    for p in named_children(params) {
                        let pat = p.child_by_field_name("pattern").unwrap_or(p);
                        self.bind_pattern(pat, cx);
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(body, cx, depth + 1, imp);
                }
            }
            "call_expression" => {
                if let Some(function) = node.child_by_field_name("function") {
                    let target = self.target(function, imp);
                    self.b
                        .push_reference(ReferenceKind::Call, cx, target, function, function);
                    self.walk(function, cx, depth + 1, imp);
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    self.walk(args, cx, depth + 1, imp);
                }
            }
            "struct_expression" => {
                if let Some(name) = node.child_by_field_name("name") {
                    if let Some(path) = self.type_path(name) {
                        let path = self.self_type(path, imp);
                        let target = Target {
                            path,
                            ..Default::default()
                        };
                        self.b
                            .push_reference(ReferenceKind::New, cx, target, name, name);
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(body, cx, depth + 1, imp);
                }
            }
            "field_expression" => {
                if let Some(v) = node.child_by_field_name("value") {
                    self.walk(v, cx, depth + 1, imp);
                }
            }
            // Attributes and macros carry token trees, not syntax.
            "attribute_item" | "inner_attribute_item" | "macro_invocation" | "macro_definition" => {
                if node.kind() == "macro_definition" {
                    if let Some(n) = node.child_by_field_name("name") {
                        let name = self.b.text(n);
                        if let Some(id) =
                            self.b.define(SymbolKind::Function, name, cx.scope, node, n)
                        {
                            self.b.sym(id).attrs.insert("macro".into(), true.into());
                        }
                    }
                }
            }
            _ => self.walk_children(node, cx, depth, imp),
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

    /// Bind the identifiers of a pattern (types unknown). Path heads of
    /// tuple-struct and struct patterns are not bindings.
    fn bind_pattern(&mut self, p: Node, cx: Cx) {
        let mut stack = vec![(p, 0u32)];
        while let Some((n, d)) = stack.pop() {
            if d > 64 {
                continue;
            }
            match n.kind() {
                "identifier" => {
                    let text = self.b.text(n);
                    // `None`, `Kind::A`-style constants start upper-case.
                    if !text.starts_with(char::is_uppercase) {
                        self.bind(cx, n, None);
                    }
                }
                "tuple_struct_pattern" | "struct_pattern" => {
                    for (i, c) in named_children(n).into_iter().enumerate() {
                        if i > 0 || n.kind() == "struct_pattern" {
                            stack.push((c, d + 1));
                        }
                    }
                }
                "field_pattern" => match n.child_by_field_name("pattern") {
                    Some(pat) => stack.push((pat, d + 1)),
                    None => {
                        if let Some(name) = n.child_by_field_name("name") {
                            self.bind(cx, name, None);
                        }
                    }
                },
                "scoped_identifier" | "scoped_type_identifier" | "type_identifier" => {}
                _ => {
                    for c in named_children(n) {
                        stack.push((c, d + 1));
                    }
                }
            }
        }
    }

    fn use_tree(&mut self, node: Node, prefix: Vec<String>, cx: Cx, stmt: Node) {
        let push = |s: &mut Self,
                    module: Vec<String>,
                    kind: ImportKind,
                    imported: Option<String>,
                    local: Option<String>| {
            s.b.push_import(Import {
                module: module.join("::"),
                level: 0,
                kind,
                imported,
                local,
                scope: cx.scope,
                type_only: false,
                span: span(stmt),
            });
        };
        match node.kind() {
            "identifier" | "crate" | "self" | "super" | "scoped_identifier" => {
                let mut path = prefix;
                path.extend(self.path_segments(node));
                let Some(last) = path.pop() else { return };
                let (imported, local) = if last == "self" {
                    // `use a::b::{self}` binds `b`.
                    match path.last().cloned() {
                        Some(b) => (b.clone(), b),
                        None => return,
                    }
                } else {
                    (last.clone(), last)
                };
                let module = if imported == local && path.last() == Some(&imported) {
                    path[..path.len() - 1].to_vec()
                } else {
                    path
                };
                push(self, module, ImportKind::Named, Some(imported), Some(local));
            }
            "use_as_clause" => {
                let (Some(p), Some(alias)) = (
                    node.child_by_field_name("path"),
                    node.child_by_field_name("alias"),
                ) else {
                    return;
                };
                let mut path = prefix;
                path.extend(self.path_segments(p));
                let Some(last) = path.pop() else { return };
                let alias = self.b.text(alias);
                if alias == "_" {
                    return;
                }
                push(self, path, ImportKind::Named, Some(last), Some(alias));
            }
            "use_wildcard" => {
                let mut path = prefix;
                if let Some(p) = named_children(node).into_iter().next() {
                    path.extend(self.path_segments(p));
                }
                push(self, path, ImportKind::Wildcard, None, None);
            }
            "scoped_use_list" => {
                let mut path = prefix;
                if let Some(p) = node.child_by_field_name("path") {
                    path.extend(self.path_segments(p));
                }
                if let Some(list) = node.child_by_field_name("list") {
                    for item in named_children(list) {
                        self.use_tree(item, path.clone(), cx, stmt);
                    }
                }
            }
            "use_list" => {
                for item in named_children(node) {
                    self.use_tree(item, prefix.clone(), cx, stmt);
                }
            }
            _ => {}
        }
    }

    /// `a::b::c` as segments; `self` at the head of a module path is kept
    /// as `self` here (imports are module paths).
    fn path_segments(&self, node: Node) -> Vec<String> {
        match node.kind() {
            "scoped_identifier" | "scoped_type_identifier" => {
                let mut out = node
                    .child_by_field_name("path")
                    .map(|p| self.path_segments(p))
                    .unwrap_or_else(|| vec!["crate".into()]);
                if let Some(n) = node.child_by_field_name("name") {
                    out.push(self.b.text(n));
                }
                out
            }
            "generic_type" | "generic_function" => node
                .child_by_field_name("type")
                .or_else(|| node.child_by_field_name("function"))
                .map(|t| self.path_segments(t))
                .unwrap_or_default(),
            _ => vec![self.b.text(node)],
        }
    }

    fn doc(&self, node: Node) -> Option<String> {
        // `///` comments are `line_comment` siblings with a doc child;
        // attributes may sit between them and the item.
        let mut cur = node.prev_sibling();
        let mut first: Option<String> = None;
        while let Some(prev) = cur {
            match prev.kind() {
                "attribute_item" => {}
                "line_comment" | "block_comment" => {
                    let doc = named_children(prev)
                        .into_iter()
                        .find(|c| c.kind() == "doc_comment")
                        .map(|d| self.b.text(d));
                    let is_outer = named_children(prev)
                        .iter()
                        .any(|c| c.kind() == "outer_doc_comment_marker");
                    match doc {
                        Some(text) if is_outer => {
                            let line = text.trim().to_string();
                            if !line.is_empty() {
                                first = Some(line);
                            }
                        }
                        _ => break,
                    }
                }
                _ => break,
            }
            cur = prev.prev_sibling();
        }
        first.map(|l| truncate(&l, 200))
    }

    fn function(&mut self, node: Node, cx: Cx, depth: u32, imp: Option<&Impl>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.b.text(name_node);
        let in_trait = cx
            .scope
            .is_some_and(|s| self.b.symbols[s as usize].kind == SymbolKind::Interface);
        let kind = if imp.is_some() || in_trait {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, &name);
        }
        let doc = self.doc(node);
        let Some(id) = self.b.define(kind, name.clone(), cx.scope, node, name_node) else {
            return;
        };
        let params = node.child_by_field_name("parameters");
        let ret = node.child_by_field_name("return_type");
        let mut signature = String::new();
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            signature.push_str(&self.b.text_truncated(tp));
        }
        signature.push_str(&params.map(|p| self.b.text_truncated(p)).unwrap_or_default());
        if let Some(r) = ret {
            signature.push_str(" -> ");
            signature.push_str(&self.b.text_truncated(r));
        }
        let result = ret
            .and_then(|r| self.type_path(r))
            .map(|t| self.self_type(t, imp));
        let has_self = params.is_some_and(|p| {
            named_children(p)
                .iter()
                .any(|c| c.kind() == "self_parameter")
        });
        {
            let exported = has_vis(node) || in_trait || imp.is_some_and(|i| i.tr.is_some());
            let sym = self.b.sym(id);
            sym.signature = Some(signature);
            sym.doc = doc;
            sym.exported = exported;
            if let Some(r) = result {
                sym.attrs.insert(
                    "result_types".into(),
                    vec![serde_json::Value::from(r)].into(),
                );
            }
            if !has_self && (imp.is_some() || in_trait) {
                sym.attrs.insert("static".into(), true.into());
            }
            if node.kind() == "function_signature_item" {
                sym.attrs.insert("abstract".into(), true.into());
            }
            if let Some(imp) = imp {
                let receiver = imp.ty.last().cloned().unwrap_or_default();
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
                sym.attrs.insert("impl_type".into(), imp.ty.clone().into());
                if let Some(tr) = &imp.tr {
                    sym.attrs.insert("impl_trait".into(), tr.clone().into());
                }
            }
        }
        let inner = Cx {
            scope: Some(id),
            fn_scope: Some(id),
            exported: false,
            anon: false,
        };
        if let Some(p) = params {
            for param in named_children(p) {
                match param.kind() {
                    "self_parameter" => {
                        self.b.add_local(Some(id), "self");
                        if let Some(imp) = imp {
                            self.types.bind(Some(id), "self", Some(imp.ty.clone()));
                        }
                    }
                    "parameter" => {
                        let ty = param
                            .child_by_field_name("type")
                            .and_then(|t| self.type_path(t))
                            .map(|t| self.self_type(t, imp));
                        if let Some(pat) = param.child_by_field_name("pattern") {
                            let pat = if pat.kind() == "mut_pattern" {
                                pat.named_child(0).unwrap_or(pat)
                            } else {
                                pat
                            };
                            if pat.kind() == "identifier" {
                                self.bind(inner, pat, ty);
                            } else {
                                self.bind_pattern(pat, inner);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, inner, depth + 1, imp);
        }
    }

    fn adt(&mut self, node: Node, cx: Cx) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.b.text(name_node);
        let type_kind = match node.kind() {
            "enum_item" => "enum",
            "union_item" => "union",
            _ => "struct",
        };
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, &name);
        }
        let doc = self.doc(node);
        let Some(id) = self
            .b
            .define(SymbolKind::Class, name.clone(), cx.scope, node, name_node)
        else {
            return;
        };
        {
            let sym = self.b.sym(id);
            sym.exported = has_vis(node);
            sym.doc = doc;
            sym.attrs.insert("type_kind".into(), type_kind.into());
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        for item in named_children(body) {
            match item.kind() {
                "field_declaration" => {
                    let Some(n) = item.child_by_field_name("name") else {
                        continue;
                    };
                    let fname = self.b.text(n);
                    let ty = item.child_by_field_name("type");
                    let path = ty.and_then(|t| self.type_path(t));
                    let sig = ty.map(|t| self.b.text_truncated(t));
                    if let Some(fid) = self
                        .b
                        .define(SymbolKind::Variable, fname, Some(id), item, n)
                    {
                        let sym = self.b.sym(fid);
                        sym.exported = has_vis(item);
                        sym.signature = sig;
                        sym.attrs.insert("member".into(), true.into());
                        if let Some(p) = path {
                            sym.attrs.insert("type_path".into(), p.into());
                        }
                    }
                }
                "enum_variant" => {
                    let Some(n) = item.child_by_field_name("name") else {
                        continue;
                    };
                    let vname = self.b.text(n);
                    if let Some(vid) = self
                        .b
                        .define(SymbolKind::Variable, vname, Some(id), item, n)
                    {
                        let sym = self.b.sym(vid);
                        sym.exported = true;
                        sym.attrs.insert("member".into(), true.into());
                        sym.attrs.insert("static".into(), true.into());
                        sym.attrs.insert("enum_variant".into(), true.into());
                        sym.attrs
                            .insert("type_path".into(), vec![name.clone()].into());
                    }
                }
                _ => {}
            }
        }
    }

    fn r#trait(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.b.text(name_node);
        let doc = self.doc(node);
        let Some(id) = self
            .b
            .define(SymbolKind::Interface, name, cx.scope, node, name_node)
        else {
            return;
        };
        {
            let sym = self.b.sym(id);
            sym.exported = has_vis(node);
            sym.doc = doc;
            sym.attrs.insert("type_kind".into(), "trait".into());
        }
        if let Some(bounds) = node.child_by_field_name("bounds") {
            for b in named_children(bounds) {
                if let Some(segments) = self.type_path(b) {
                    let base = self.b.name_path(b, segments);
                    self.b.sym(id).bases.push(base);
                }
            }
        }
        let inner = Cx {
            scope: Some(id),
            fn_scope: None,
            exported: false,
            anon: false,
        };
        if let Some(body) = node.child_by_field_name("body") {
            for item in named_children(body) {
                if matches!(item.kind(), "function_item" | "function_signature_item") {
                    // Inside a trait, `Self` is the implementing type; it is
                    // left unresolved rather than taken to be the trait.
                    self.function(item, inner, depth + 1, None);
                }
            }
        }
    }

    /// `Self` → the impl's self type.
    fn self_type(&self, t: Vec<String>, imp: Option<&Impl>) -> Vec<String> {
        match (t.first().map(String::as_str), imp) {
            (Some("Self"), Some(imp)) => {
                let mut out = imp.ty.clone();
                out.extend(t.into_iter().skip(1));
                out
            }
            _ => t,
        }
    }

    /// Segments of a named type, without references, pointers and type
    /// arguments; `None` for tuples, arrays, slices and function types.
    fn type_path(&self, ty: Node) -> Option<Vec<String>> {
        match ty.kind() {
            "type_identifier" | "identifier" | "primitive_type" => Some(vec![self.b.text(ty)]),
            "scoped_type_identifier" | "scoped_identifier" => Some(
                self.path_segments(ty)
                    .into_iter()
                    .map(|s| if s == "self" { "mod:self".into() } else { s })
                    .collect(),
            ),
            "generic_type" => self.type_path(ty.child_by_field_name("type")?),
            "reference_type" | "pointer_type" => self.type_path(ty.child_by_field_name("type")?),
            "abstract_type" | "dynamic_type" => {
                let t = ty.child_by_field_name("trait").or(ty.named_child(0))?;
                self.type_path(t)
            }
            _ => None,
        }
    }

    /// A static path: `f`, `a::b::f`, `self.x.y`, `Self::new`.
    fn dotted(&self, expr: Node, imp: Option<&Impl>) -> Option<Vec<String>> {
        match expr.kind() {
            "identifier" => Some(vec![self.b.text(expr)]),
            "self" => Some(vec!["self".into()]),
            "scoped_identifier" | "generic_function" => {
                let segs = self.type_path(if expr.kind() == "generic_function" {
                    expr.child_by_field_name("function")?
                } else {
                    expr
                })?;
                Some(self.self_type(segs, imp))
            }
            "field_expression" => {
                let mut path = self.dotted(expr.child_by_field_name("value")?, imp)?;
                let field = expr.child_by_field_name("field")?;
                if field.kind() != "field_identifier" {
                    return None;
                }
                path.push(self.b.text(field));
                Some(path)
            }
            "parenthesized_expression" => self.dotted(expr.named_child(0)?, imp),
            _ => None,
        }
    }

    fn target(&self, function: Node, imp: Option<&Impl>) -> Target {
        if let Some(path) = self.dotted(function, imp) {
            return Target {
                path,
                ..Default::default()
            };
        }
        if function.kind() == "field_expression" {
            let member = function
                .child_by_field_name("field")
                .map(|f| self.b.text(f));
            let chain = match (function.child_by_field_name("value"), &member) {
                (Some(v), Some(m)) => self
                    .chain(v, 0, imp)
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

    fn chain(&self, expr: Node, depth: u32, imp: Option<&Impl>) -> Option<Vec<String>> {
        if depth > MAX_CHAIN as u32 {
            return None;
        }
        match expr.kind() {
            "identifier" | "self" | "scoped_identifier" => self.dotted(expr, imp),
            "field_expression" => {
                let mut c = self.chain(expr.child_by_field_name("value")?, depth + 1, imp)?;
                c.push(self.b.text(expr.child_by_field_name("field")?));
                Some(c)
            }
            "call_expression" => {
                let mut c = self.chain(expr.child_by_field_name("function")?, depth + 1, imp)?;
                c.push("()".into());
                Some(c)
            }
            "struct_expression" => {
                let ty = self.type_path(expr.child_by_field_name("name")?)?;
                let ty = self.self_type(ty, imp);
                Some(vec![format!("type:{}", ty.join("."))])
            }
            "reference_expression" => {
                self.chain(expr.child_by_field_name("value")?, depth + 1, imp)
            }
            "parenthesized_expression" => self.chain(expr.named_child(0)?, depth + 1, imp),
            // `?` and `.await` change the type (Result<T> → T); not followed.
            _ => None,
        }
    }
}

fn has_vis(node: Node) -> bool {
    named_children(node)
        .iter()
        .any(|c| c.kind() == "visibility_modifier")
}
