//! C# extraction adapter (tree-sitter-c-sharp).
//!
//! Produces the same conventions as the Java adapter (see `java.rs`), with
//! these differences:
//!
//! * A file-scoped namespace (`namespace A.B;`) is stored in
//!   `attrs.package`; a block namespace becomes a `Namespace` symbol whose
//!   name is the dotted namespace name.
//! * `using N;` is a `Wildcard` import of `N`; `using static T;` a
//!   `Wildcard` import of type `T`; `using A = N.T;` a `Named` import of
//!   `T` from `N` bound to `A`.
//! * Classes, structs, records and enums are `Class` symbols; interfaces are
//!   `Interface` symbols. The whole base list goes to `bases`; the resolver
//!   reports interfaces in it as implemented.
//! * Properties and fields are `Variable` members; `base` is the receiver
//!   for base-class members. Attributes are decorator references.
//! * `global using N;` has `level` 1. Extension methods (first parameter
//!   declared `this T`) carry `attrs.extension_receiver`.

use crate::common::{
    children, doc_comment, named_children, span, truncate, Builder, Cx, Extracted, Limits,
    LocalTypes, Target, MAX_CHAIN, MAX_DEPTH,
};
use tree_sitter::Node;
use usie_ir::{Import, ImportKind, ReferenceKind, SymbolKind};

pub(crate) fn extract(root: Node, src: &[u8], limits: Limits) -> Extracted {
    let mut c = Cs {
        b: Builder::new(src, limits),
        types: LocalTypes::default(),
    };
    c.walk(root, Cx::default(), 0);
    let Cs { mut b, types } = c;
    types.apply(&mut b);
    b.finish()
}

struct Cs<'a> {
    b: Builder<'a>,
    types: LocalTypes,
}

const TYPE_DECLS: &[&str] = &[
    "class_declaration",
    "struct_declaration",
    "record_declaration",
    "record_struct_declaration",
    "interface_declaration",
    "enum_declaration",
];

impl<'a> Cs<'a> {
    fn walk_children(&mut self, node: Node, cx: Cx, depth: u32) {
        for child in children(node) {
            self.walk(child, cx, depth + 1);
        }
    }

    fn walk(&mut self, node: Node, cx: Cx, depth: u32) {
        self.b.nodes += 1;
        if depth > MAX_DEPTH {
            self.b.truncated = true;
            return;
        }
        let kind = node.kind();
        if TYPE_DECLS.contains(&kind) {
            self.type_decl(node, cx, depth);
            return;
        }
        match kind {
            "file_scoped_namespace_declaration" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = compact(&self.b.text(n));
                    self.b.attrs.insert("package".into(), name.into());
                }
                self.walk_children(node, cx, depth);
            }
            "namespace_declaration" => self.namespace(node, cx, depth),
            "using_directive" => self.using(node),
            "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "operator_declaration"
            | "local_function_statement" => self.method(node, cx, depth),
            "field_declaration" | "event_field_declaration" => self.field(node, cx, depth),
            "property_declaration" => self.property(node, cx, depth),
            "local_declaration_statement" | "using_statement" | "fixed_statement" => {
                for decl in named_children(node) {
                    if decl.kind() == "variable_declaration" {
                        self.variable_declaration(decl, cx, depth);
                    } else {
                        self.walk(decl, cx, depth + 1);
                    }
                }
            }
            "foreach_statement" => {
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|t| self.type_path(t));
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "identifier" {
                        self.bind(cx, left, ty);
                    }
                }
                for f in ["right", "body"] {
                    if let Some(n) = node.child_by_field_name(f) {
                        self.walk(n, cx, depth + 1);
                    }
                }
            }
            "catch_declaration" | "declaration_pattern" | "declaration_expression" => {
                let ty = node
                    .child_by_field_name("type")
                    .and_then(|t| self.type_path(t));
                if let Some(n) = node.child_by_field_name("name") {
                    self.bind(cx, n, ty);
                }
            }
            "lambda_expression" | "anonymous_method_expression" => {
                if let Some(params) = node.child_by_field_name("parameters") {
                    if params.kind() == "parameter_list" {
                        self.parameters(params, cx);
                    } else {
                        self.bind(cx, params, None);
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(body, cx, depth + 1);
                }
            }
            "invocation_expression" => {
                if let Some(function) = node.child_by_field_name("function") {
                    let target = self.target(function);
                    self.b
                        .push_reference(ReferenceKind::Call, cx, target, function, function);
                    self.walk(function, cx, depth + 1);
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    self.walk(args, cx, depth + 1);
                }
            }
            "constructor_initializer" => {
                // `: base(...)` runs the base class constructor.
                if children(node).iter().any(|c| c.kind() == "base") {
                    let target = Target {
                        path: vec!["base".into()],
                        ..Default::default()
                    };
                    self.b
                        .push_reference(ReferenceKind::Call, cx, target, node, node);
                }
                self.walk_children(node, cx, depth);
            }
            "object_creation_expression" => {
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
                for f in ["arguments", "initializer"] {
                    if let Some(n) = node.child_by_field_name(f) {
                        self.walk(n, cx, depth + 1);
                    }
                }
            }
            "attribute" => {
                if let Some(name) = node.child_by_field_name("name") {
                    if let Some(path) = self.type_path(name) {
                        let target = Target {
                            path,
                            ..Default::default()
                        };
                        self.b
                            .push_reference(ReferenceKind::Decorator, cx, target, name, name);
                    }
                }
                self.walk_children(node, cx, depth);
            }
            // Names in member access are walked through `target`.
            "member_access_expression" => {
                if let Some(e) = node.child_by_field_name("expression") {
                    self.walk(e, cx, depth + 1);
                }
            }
            _ => self.walk_children(node, cx, depth),
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

    fn namespace(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = compact(&self.b.text(name_node));
        let Some(id) = self
            .b
            .define(SymbolKind::Namespace, name, cx.scope, node, name_node)
        else {
            return;
        };
        self.b.sym(id).exported = true;
        let inner = Cx {
            scope: Some(id),
            ..cx
        };
        if let Some(body) = node.child_by_field_name("body") {
            self.walk_children(body, inner, depth + 1);
        }
    }

    fn using(&mut self, node: Node) {
        let kids = children(node);
        let is_static = kids.iter().any(|c| c.kind() == "static");
        // `global using N;` applies to every file of the project.
        let is_global = kids.iter().any(|c| c.kind() == "global");
        let alias = node.child_by_field_name("name");
        let Some(target) = kids
            .iter()
            .rev()
            .find(|c| c.is_named() && Some(c.id()) != alias.map(|a| a.id()))
        else {
            return;
        };
        let full = compact(&self.b.text(*target));
        let full = full
            .split('<')
            .next()
            .unwrap_or("")
            .trim_start_matches("global::")
            .to_string();
        if full.is_empty() {
            return;
        }
        let import = match alias {
            Some(a) => {
                let (module, last) = match full.rsplit_once('.') {
                    Some((m, l)) => (m.to_string(), l.to_string()),
                    None => (String::new(), full.clone()),
                };
                Import {
                    module,
                    level: 0,
                    kind: ImportKind::Named,
                    imported: Some(last),
                    local: Some(self.b.text(a)),
                    scope: None,
                    type_only: false,
                    span: span(node),
                }
            }
            None => Import {
                module: full,
                level: u32::from(is_global),
                kind: ImportKind::Wildcard,
                imported: None,
                local: None,
                scope: None,
                type_only: is_static,
                span: span(node),
            },
        };
        self.b.push_import(import);
    }

    /// Modifier keywords and attribute names of a declaration. Attribute
    /// uses are walked (recorded as references) by the caller's walk.
    fn modifiers(&mut self, decl: Node, cx: Cx) -> (Vec<String>, Vec<String>) {
        let mut keywords = Vec::new();
        let mut attributes = Vec::new();
        for c in children(decl) {
            match c.kind() {
                "modifier" => keywords.push(self.b.text(c)),
                "attribute_list" => {
                    for a in named_children(c) {
                        if a.kind() == "attribute" {
                            if let Some(n) = a.child_by_field_name("name") {
                                attributes.push(compact(&self.b.text(n)));
                            }
                        }
                    }
                    self.walk(c, cx, 1);
                }
                _ => {}
            }
        }
        (keywords, attributes)
    }

    fn doc(&self, node: Node) -> Option<String> {
        let raw = doc_comment(&self.b, node)?;
        // `/// <summary>Text</summary>`: drop the third slash and XML tags.
        let mut out = String::new();
        let mut in_tag = false;
        for ch in raw.trim_start_matches('/').chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => out.push(c),
                _ => {}
            }
        }
        let out = out.trim();
        (!out.is_empty()).then(|| truncate(out, 200))
    }

    fn type_decl(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = self.b.text(name_node);
        let (kind, type_kind) = match node.kind() {
            "interface_declaration" => (SymbolKind::Interface, "interface"),
            "struct_declaration" => (SymbolKind::Class, "struct"),
            "record_declaration" | "record_struct_declaration" => (SymbolKind::Class, "record"),
            "enum_declaration" => (SymbolKind::Class, "enum"),
            _ => (SymbolKind::Class, "class"),
        };
        let (keywords, attributes) = self.modifiers(node, cx);
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, &name);
        }
        let doc = self.doc(node);
        let Some(id) = self.b.define(kind, name.clone(), cx.scope, node, name_node) else {
            return;
        };
        {
            let sym = self.b.sym(id);
            sym.exported = keywords.iter().any(|k| k == "public");
            sym.doc = doc;
            sym.attrs.insert("type_kind".into(), type_kind.into());
            if keywords.iter().any(|k| k == "static") {
                sym.attrs.insert("static".into(), true.into());
            }
            if !keywords.is_empty() {
                sym.attrs.insert("modifiers".into(), keywords.into());
            }
            if !attributes.is_empty() {
                sym.attrs.insert("annotations".into(), attributes.into());
            }
        }
        for c in named_children(node) {
            if c.kind() == "base_list" {
                for t in named_children(c) {
                    let ty = if t.kind() == "primary_constructor_base_type" {
                        t.named_child(0).unwrap_or(t)
                    } else {
                        t
                    };
                    if let Some(segments) = self.type_path(ty) {
                        let base = self.b.name_path(ty, segments);
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
        // Positional record parameters are properties.
        for c in named_children(node) {
            if c.kind() == "parameter_list" {
                for p in named_children(c) {
                    if p.kind() != "parameter" {
                        continue;
                    }
                    let (Some(n), ty) =
                        (p.child_by_field_name("name"), p.child_by_field_name("type"))
                    else {
                        continue;
                    };
                    let pname = self.b.text(n);
                    let path = ty.and_then(|t| self.type_path(t));
                    let sig = ty.map(|t| self.b.text_truncated(t));
                    if let Some(fid) = self.b.define(SymbolKind::Variable, pname, Some(id), p, n) {
                        let sym = self.b.sym(fid);
                        sym.exported = true;
                        sym.signature = sig;
                        sym.attrs.insert("member".into(), true.into());
                        if let Some(path) = path {
                            sym.attrs.insert("type_path".into(), path.into());
                        }
                    }
                }
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            for member in named_children(body) {
                if member.kind() == "enum_member_declaration" {
                    if let Some(n) = member.child_by_field_name("name") {
                        let mname = self.b.text(n);
                        if let Some(mid) =
                            self.b
                                .define(SymbolKind::Variable, mname, Some(id), member, n)
                        {
                            let sym = self.b.sym(mid);
                            sym.exported = true;
                            sym.attrs.insert("member".into(), true.into());
                            sym.attrs.insert("static".into(), true.into());
                            sym.attrs.insert("enum_constant".into(), true.into());
                            sym.attrs
                                .insert("type_path".into(), vec![name.clone()].into());
                        }
                    }
                } else {
                    self.walk(member, inner, depth + 2);
                }
            }
        }
    }

    fn field(&mut self, node: Node, cx: Cx, depth: u32) {
        let (keywords, attributes) = self.modifiers(node, cx);
        let doc = self.doc(node);
        let Some(decl) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "variable_declaration")
        else {
            return;
        };
        let ty = decl.child_by_field_name("type");
        let path = ty.and_then(|t| self.type_path(t));
        let sig = ty.map(|t| self.b.text_truncated(t));
        for d in named_children(decl) {
            if d.kind() != "variable_declarator" {
                continue;
            }
            let Some(n) = d.child_by_field_name("name") else {
                continue;
            };
            let name = self.b.text(n);
            if let Some(id) = self.b.define(SymbolKind::Variable, name, cx.scope, d, n) {
                let sym = self.b.sym(id);
                sym.exported = keywords.iter().any(|k| k == "public");
                sym.signature = sig.clone();
                sym.doc = doc.clone();
                sym.attrs.insert("member".into(), true.into());
                if keywords.iter().any(|k| k == "static" || k == "const") {
                    sym.attrs.insert("static".into(), true.into());
                }
                if !keywords.is_empty() {
                    sym.attrs
                        .insert("modifiers".into(), keywords.clone().into());
                }
                if !attributes.is_empty() {
                    sym.attrs
                        .insert("annotations".into(), attributes.clone().into());
                }
                if let Some(p) = &path {
                    sym.attrs.insert("type_path".into(), p.clone().into());
                }
            }
            for v in named_children(d) {
                if Some(v.id()) != Some(n.id()) {
                    self.walk(v, cx, depth + 1);
                }
            }
        }
    }

    fn property(&mut self, node: Node, cx: Cx, depth: u32) {
        let (keywords, attributes) = self.modifiers(node, cx);
        let Some(n) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.b.text(n);
        let ty = node.child_by_field_name("type");
        let path = ty.and_then(|t| self.type_path(t));
        let sig = ty.map(|t| self.b.text_truncated(t));
        let doc = self.doc(node);
        let Some(id) = self.b.define(SymbolKind::Variable, name, cx.scope, node, n) else {
            return;
        };
        {
            let sym = self.b.sym(id);
            sym.exported = keywords.iter().any(|k| k == "public");
            sym.signature = sig;
            sym.doc = doc;
            sym.attrs.insert("member".into(), true.into());
            sym.attrs.insert("property".into(), true.into());
            if keywords.iter().any(|k| k == "static") {
                sym.attrs.insert("static".into(), true.into());
            }
            if !keywords.is_empty() {
                sym.attrs.insert("modifiers".into(), keywords.into());
            }
            if !attributes.is_empty() {
                sym.attrs.insert("annotations".into(), attributes.into());
            }
            if let Some(p) = path {
                sym.attrs.insert("type_path".into(), p.into());
            }
        }
        // Accessor bodies and initializers run with the property as scope.
        let inner = Cx {
            scope: Some(id),
            fn_scope: Some(id),
            exported: false,
            anon: false,
        };
        for c in named_children(node) {
            let is_type = Some(c.id()) == ty.map(|t| t.id());
            if !is_type && c.id() != n.id() && !matches!(c.kind(), "modifier" | "attribute_list") {
                self.walk(c, inner, depth + 1);
            }
        }
    }

    fn variable_declaration(&mut self, decl: Node, cx: Cx, depth: u32) {
        let ty = decl.child_by_field_name("type");
        let implicit = ty.is_some_and(|t| t.kind() == "implicit_type");
        let declared = if implicit {
            None
        } else {
            ty.and_then(|t| self.type_path(t))
        };
        for d in named_children(decl) {
            if d.kind() != "variable_declarator" {
                continue;
            }
            let Some(n) = d.child_by_field_name("name") else {
                continue;
            };
            let value = named_children(d)
                .into_iter()
                .find(|v| v.id() != n.id() && v.kind() != "bracketed_argument_list");
            let value = value.map(|v| {
                if v.kind() == "equals_value_clause" {
                    v.named_child(0).unwrap_or(v)
                } else {
                    v
                }
            });
            if implicit {
                match value {
                    Some(v) if v.kind() == "object_creation_expression" => {
                        let ty = v
                            .child_by_field_name("type")
                            .and_then(|t| self.type_path(t));
                        self.bind(cx, n, ty);
                    }
                    Some(v) if v.kind() == "invocation_expression" => {
                        let callee = v
                            .child_by_field_name("function")
                            .map(|f| self.target(f).path)
                            .unwrap_or_default();
                        let name = self.b.text(n);
                        if cx.fn_scope.is_some() && !callee.is_empty() {
                            self.b.add_local(cx.fn_scope, &name);
                            self.types.bind_call(cx.fn_scope, &name, callee, 0);
                        } else {
                            self.bind(cx, n, None);
                        }
                    }
                    _ => self.bind(cx, n, None),
                }
            } else {
                self.bind(cx, n, declared.clone());
            }
            if let Some(v) = value {
                self.walk(v, cx, depth + 1);
            }
        }
    }

    fn method(&mut self, node: Node, cx: Cx, depth: u32) {
        let (keywords, attributes) = self.modifiers(node, cx);
        let constructor = node.kind() == "constructor_declaration";
        let Some(name_node) = node.child_by_field_name("name").or_else(|| {
            // Operators have no name field.
            node.child_by_field_name("operator")
        }) else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = self.b.text(name_node);
        let parent_kind = cx.scope.map(|s| self.b.symbols[s as usize].kind);
        let in_type = matches!(
            parent_kind,
            Some(SymbolKind::Class) | Some(SymbolKind::Interface)
        );
        let kind = if in_type && node.kind() != "local_function_statement" {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        if node.kind() == "local_function_statement" {
            self.b.add_local(cx.fn_scope, &name);
        }
        let doc = self.doc(node);
        let Some(id) = self.b.define(kind, name, cx.scope, node, name_node) else {
            return;
        };
        let params = node.child_by_field_name("parameters");
        let ret = node
            .child_by_field_name("returns")
            .or_else(|| node.child_by_field_name("type"));
        let mut signature = String::new();
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            signature.push_str(&self.b.text_truncated(tp));
        }
        signature.push_str(
            &params
                .map(|p| self.b.text_truncated(p))
                .unwrap_or_else(|| "()".into()),
        );
        if let Some(r) = ret {
            signature.push_str(": ");
            signature.push_str(&self.b.text_truncated(r));
        }
        let in_interface = parent_kind == Some(SymbolKind::Interface);
        let result = ret.and_then(|t| self.type_path(t));
        {
            let sym = self.b.sym(id);
            sym.signature = Some(signature);
            sym.doc = doc;
            sym.exported = in_interface || keywords.iter().any(|k| k == "public");
            if constructor {
                sym.attrs.insert("constructor".into(), true.into());
            }
            if keywords.iter().any(|k| k == "static") {
                sym.attrs.insert("static".into(), true.into());
            }
            if keywords.iter().any(|k| k == "abstract")
                || (in_interface && node.child_by_field_name("body").is_none())
            {
                sym.attrs.insert("abstract".into(), true.into());
            }
            if let Some(r) = result {
                sym.attrs.insert(
                    "result_types".into(),
                    vec![serde_json::Value::from(r)].into(),
                );
            }
            if !keywords.is_empty() {
                sym.attrs.insert("modifiers".into(), keywords.into());
            }
            if !attributes.is_empty() {
                sym.attrs.insert("annotations".into(), attributes.into());
            }
        }
        let inner = Cx {
            scope: Some(id),
            fn_scope: Some(id),
            exported: false,
            anon: false,
        };
        if let Some(p) = params {
            let receiver = named_children(p)
                .into_iter()
                .find(|c| c.kind() == "parameter")
                .filter(|first| {
                    children(*first)
                        .iter()
                        .any(|m| m.kind() == "modifier" && self.b.text(*m) == "this")
                })
                .and_then(|first| first.child_by_field_name("type"))
                .and_then(|t| self.type_path(t));
            if let Some(r) = receiver {
                self.b
                    .sym(id)
                    .attrs
                    .insert("extension_receiver".into(), r.into());
            }
            self.parameters(p, inner);
        }
        for c in named_children(node) {
            if matches!(
                c.kind(),
                "block" | "arrow_expression_clause" | "constructor_initializer"
            ) {
                self.walk(c, inner, depth + 1);
            }
        }
    }

    fn parameters(&mut self, params: Node, cx: Cx) {
        for p in named_children(params) {
            if p.kind() != "parameter" {
                continue;
            }
            let ty = p
                .child_by_field_name("type")
                .and_then(|t| self.type_path(t));
            if let Some(n) = p.child_by_field_name("name") {
                self.bind(cx, n, ty);
            }
        }
    }

    /// Segments of a named type without type arguments; `None` for arrays,
    /// tuples, pointers and `var`.
    fn type_path(&self, ty: Node) -> Option<Vec<String>> {
        match ty.kind() {
            "identifier" => Some(vec![self.b.text(ty)]),
            "predefined_type" => Some(vec![self.b.text(ty)]),
            "qualified_name" => {
                let mut out = self.type_path(ty.child_by_field_name("qualifier")?)?;
                out.extend(self.type_path(ty.child_by_field_name("name")?)?);
                Some(out)
            }
            "alias_qualified_name" => self.type_path(ty.child_by_field_name("name")?),
            "generic_name" => {
                let id = named_children(ty)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")?;
                Some(vec![self.b.text(id)])
            }
            "nullable_type" => {
                self.type_path(ty.child_by_field_name("type").or(ty.named_child(0))?)
            }
            _ => None,
        }
    }

    /// `a.b.c` for member accesses on names, `this` and `base`.
    fn dotted(&self, expr: Node) -> Option<Vec<String>> {
        match expr.kind() {
            "identifier" | "predefined_type" => Some(vec![self.b.text(expr)]),
            "this_expression" | "this" => Some(vec!["this".into()]),
            "base_expression" | "base" => Some(vec!["base".into()]),
            "generic_name" => self.type_path(expr),
            "qualified_name" => self.type_path(expr),
            "member_access_expression" => {
                let name = expr.child_by_field_name("name")?;
                let mut path = match expr.child_by_field_name("expression") {
                    Some(e) => self.dotted(e)?,
                    // `this.X` / `base.X`: the receiver keyword is unnamed.
                    None => {
                        let first = children(expr).into_iter().next()?;
                        match first.kind() {
                            "this" => vec!["this".into()],
                            "base" => vec!["base".into()],
                            _ => return None,
                        }
                    }
                };
                path.extend(self.type_path(name)?);
                Some(path)
            }
            "parenthesized_expression" => self.dotted(expr.named_child(0)?),
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
        if function.kind() == "member_access_expression" {
            let member = function
                .child_by_field_name("name")
                .and_then(|n| self.type_path(n))
                .and_then(|p| p.last().cloned());
            let chain = match (function.child_by_field_name("expression"), &member) {
                (Some(e), Some(m)) => self
                    .chain(e, 0)
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
            "identifier" | "this_expression" | "base_expression" => self.dotted(expr),
            "member_access_expression" => {
                let mut c = match expr.child_by_field_name("expression") {
                    Some(e) => self.chain(e, depth + 1)?,
                    None => self.dotted(expr)?[..1].to_vec(),
                };
                c.push(self.type_path(expr.child_by_field_name("name")?)?.pop()?);
                Some(c)
            }
            "invocation_expression" => {
                let mut c = self.chain(expr.child_by_field_name("function")?, depth + 1)?;
                c.push("()".into());
                Some(c)
            }
            "object_creation_expression" | "cast_expression" => {
                let ty = self.type_path(expr.child_by_field_name("type")?)?;
                Some(vec![format!("type:{}", ty.join("."))])
            }
            "parenthesized_expression" => self.chain(expr.named_child(0)?, depth + 1),
            _ => None,
        }
    }
}

fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}
