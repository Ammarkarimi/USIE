//! Kotlin extraction adapter (tree-sitter-kotlin-ng).
//!
//! Produces the Java adapter's conventions (see `java.rs`), with these
//! differences:
//!
//! * Top-level functions and properties are package members.
//! * `object` declarations are classes (`type_kind: object`) whose members
//!   are static; members of a `companion object` are recorded as static
//!   members of the enclosing class.
//! * `val`/`var` constructor parameters are properties of the class.
//! * Calls of class names are recorded as calls; the resolver reports them
//!   as instantiations. A delegation specifier with arguments is the
//!   superclass (`bases`); one without is an interface (`implements`).
//! * `exported` is true unless the declaration is `private` or `internal`.

use crate::common::{
    children, doc_comment, named_children, span, Builder, Cx, Extracted, Limits, LocalTypes,
    Target, MAX_CHAIN, MAX_DEPTH,
};
use tree_sitter::Node;
use usie_ir::{Import, ImportKind, ReferenceKind, SymbolKind};

pub(crate) fn extract(root: Node, src: &[u8], limits: Limits) -> Extracted {
    let mut k = Kt {
        b: Builder::new(src, limits),
        types: LocalTypes::default(),
    };
    k.walk(root, Cx::default(), 0);
    let Kt { mut b, types } = k;
    types.apply(&mut b);
    b.finish()
}

struct Kt<'a> {
    b: Builder<'a>,
    types: LocalTypes,
}

const TYPE_NODES: &[&str] = &[
    "user_type",
    "nullable_type",
    "parenthesized_type",
    "function_type",
    "non_nullable_type",
];

impl<'a> Kt<'a> {
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
        match node.kind() {
            "package_header" => {
                if let Some(q) = named_children(node).into_iter().next() {
                    let name = compact(&self.b.text(q));
                    self.b.attrs.insert("package".into(), name.into());
                }
            }
            "import" => self.import(node),
            "class_declaration" | "object_declaration" => self.class(node, cx, depth, false),
            "companion_object" => {
                // Members of a companion object are static members of the class.
                let statics = Cx {
                    anon: cx.anon,
                    ..cx
                };
                for c in named_children(node) {
                    if c.kind() == "class_body" {
                        for m in named_children(c) {
                            self.member(m, statics, depth + 2, true);
                        }
                    }
                }
            }
            "function_declaration" => self.function(node, cx, depth, false),
            "property_declaration" => self.property(node, cx, depth, false),
            "secondary_constructor" => self.constructor(node, cx, depth),
            "for_statement" => {
                for c in named_children(node) {
                    match c.kind() {
                        "variable_declaration" => {
                            let (name, ty) = self.variable(c);
                            if let Some(n) = name {
                                self.bind(cx, n, ty);
                            }
                        }
                        "multi_variable_declaration" => {
                            for v in named_children(c) {
                                if let (Some(n), _) = self.variable(v) {
                                    self.bind(cx, n, None);
                                }
                            }
                        }
                        _ => self.walk(c, cx, depth + 1),
                    }
                }
            }
            "lambda_literal" => {
                let mut explicit = false;
                for c in named_children(node) {
                    if c.kind() == "lambda_parameters" {
                        explicit = true;
                        for v in named_children(c) {
                            for n in self.declared_names(v) {
                                self.bind(cx, n, None);
                            }
                        }
                    } else {
                        self.walk(c, cx, depth + 1);
                    }
                }
                if !explicit && cx.fn_scope.is_some() {
                    self.b.add_local(cx.fn_scope, "it");
                }
            }
            "catch_block" => {
                let ty = named_children(node)
                    .into_iter()
                    .find(|c| TYPE_NODES.contains(&c.kind()))
                    .and_then(|t| self.type_path(t));
                if let Some(n) = named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
                {
                    self.bind(cx, n, ty);
                }
                for c in named_children(node) {
                    if c.kind() == "block" {
                        self.walk(c, cx, depth + 1);
                    }
                }
            }
            "call_expression" => {
                let kids = named_children(node);
                if let Some(callee) = kids.first() {
                    let target = self.target(*callee);
                    self.b
                        .push_reference(ReferenceKind::Call, cx, target, *callee, *callee);
                    self.walk(*callee, cx, depth + 1);
                }
                for c in kids.iter().skip(1) {
                    self.walk(*c, cx, depth + 1);
                }
            }
            "navigation_expression" => {
                if let Some(e) = named_children(node).into_iter().next() {
                    self.walk(e, cx, depth + 1);
                }
            }
            "annotation" => {
                for c in named_children(node) {
                    let ty = if c.kind() == "constructor_invocation" {
                        c.named_child(0).unwrap_or(c)
                    } else {
                        c
                    };
                    if let Some(path) = self.type_path(ty) {
                        let target = Target {
                            path,
                            ..Default::default()
                        };
                        self.b
                            .push_reference(ReferenceKind::Decorator, cx, target, ty, ty);
                    }
                    if c.kind() == "constructor_invocation" {
                        self.walk_children(c, cx, depth + 1);
                    }
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

    fn declared_names<'t>(&self, node: Node<'t>) -> Vec<Node<'t>> {
        match node.kind() {
            "identifier" => vec![node],
            "variable_declaration" => named_children(node)
                .into_iter()
                .filter(|c| c.kind() == "identifier")
                .take(1)
                .collect(),
            "multi_variable_declaration" => named_children(node)
                .into_iter()
                .flat_map(|c| self.declared_names(c))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Name and declared type of a `variable_declaration`.
    fn variable<'t>(&self, decl: Node<'t>) -> (Option<Node<'t>>, Option<Vec<String>>) {
        let kids = named_children(decl);
        let name = kids.iter().copied().find(|c| c.kind() == "identifier");
        let ty = kids
            .iter()
            .copied()
            .find(|c| TYPE_NODES.contains(&c.kind()))
            .and_then(|t| self.type_path(t));
        (name, ty)
    }

    fn import(&mut self, node: Node) {
        let kids = children(node);
        let Some(q) = kids
            .iter()
            .find(|c| matches!(c.kind(), "qualified_identifier" | "identifier"))
        else {
            return;
        };
        let full = compact(&self.b.text(*q));
        let wildcard = kids.iter().any(|c| c.kind() == "*" || c.kind() == ".*");
        let alias = kids
            .iter()
            .rfind(|c| c.kind() == "identifier" && c.id() != q.id())
            .map(|a| self.b.text(*a));
        let import = if wildcard {
            Import {
                module: full,
                level: 0,
                kind: ImportKind::Wildcard,
                imported: None,
                local: None,
                scope: None,
                type_only: false,
                span: span(node),
            }
        } else {
            let (module, last) = match full.rsplit_once('.') {
                Some((m, l)) => (m.to_string(), l.to_string()),
                None => (String::new(), full.clone()),
            };
            Import {
                module,
                level: 0,
                kind: ImportKind::Named,
                local: Some(alias.unwrap_or_else(|| last.clone())),
                imported: Some(last),
                scope: None,
                type_only: false,
                span: span(node),
            }
        };
        self.b.push_import(import);
    }

    /// Modifier keywords; annotations are recorded as references.
    fn modifiers(&mut self, decl: Node, cx: Cx) -> (Vec<String>, Vec<String>) {
        let mut keywords = Vec::new();
        let mut annotations = Vec::new();
        for c in named_children(decl) {
            if c.kind() != "modifiers" {
                continue;
            }
            for m in named_children(c) {
                if m.kind() == "annotation" {
                    annotations.push(compact(&self.b.text(m)).trim_start_matches('@').to_string());
                    self.walk(m, cx, 1);
                } else {
                    keywords.push(self.b.text(m));
                }
            }
        }
        (keywords, annotations)
    }

    fn exported(keywords: &[String]) -> bool {
        !keywords.iter().any(|k| k == "private" || k == "internal")
    }

    fn class(&mut self, node: Node, cx: Cx, depth: u32, _static: bool) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = self.b.text(name_node);
        let (keywords, annotations) = self.modifiers(node, cx);
        let is_object = node.kind() == "object_declaration";
        let is_interface = children(node).iter().any(|c| c.kind() == "interface");
        let enum_body = named_children(node)
            .into_iter()
            .any(|c| c.kind() == "enum_class_body");
        let (kind, type_kind) = if is_interface {
            (SymbolKind::Interface, "interface")
        } else if is_object {
            (SymbolKind::Class, "object")
        } else if enum_body || keywords.iter().any(|k| k == "enum") {
            (SymbolKind::Class, "enum")
        } else if keywords.iter().any(|k| k == "data") {
            (SymbolKind::Class, "data")
        } else {
            (SymbolKind::Class, "class")
        };
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, &name);
        }
        let doc = self.doc(node);
        let Some(id) = self.b.define(kind, name.clone(), cx.scope, node, name_node) else {
            return;
        };
        {
            let sym = self.b.sym(id);
            sym.exported = Self::exported(&keywords);
            sym.doc = doc;
            sym.attrs.insert("type_kind".into(), type_kind.into());
            if is_object {
                sym.attrs.insert("static".into(), true.into());
            }
            if !keywords.is_empty() {
                sym.attrs.insert("modifiers".into(), keywords.into());
            }
            if !annotations.is_empty() {
                sym.attrs.insert("annotations".into(), annotations.into());
            }
        }
        let inner = Cx {
            scope: Some(id),
            fn_scope: None,
            exported: false,
            anon: false,
        };
        for c in named_children(node) {
            match c.kind() {
                "delegation_specifiers" => {
                    for spec in named_children(c) {
                        let first = spec.named_child(0).unwrap_or(spec);
                        let (ty, superclass) = match first.kind() {
                            "constructor_invocation" => (first.named_child(0), true),
                            _ => (Some(first), false),
                        };
                        if let Some(ty) = ty {
                            if let Some(segments) = self.type_path(ty) {
                                let base = self.b.name_path(ty, segments);
                                if superclass || is_interface {
                                    self.b.sym(id).bases.push(base);
                                } else {
                                    self.b.sym(id).implements.push(base);
                                }
                            }
                        }
                        if first.kind() == "constructor_invocation" {
                            self.walk(first, cx, depth + 1);
                        }
                    }
                }
                "primary_constructor" => {
                    for params in named_children(c) {
                        if params.kind() != "class_parameters" {
                            continue;
                        }
                        for p in named_children(params) {
                            self.class_parameter(p, id, cx);
                        }
                    }
                }
                "class_body" | "enum_class_body" => {
                    for m in named_children(c) {
                        if m.kind() == "enum_entry" {
                            if let Some(n) = named_children(m)
                                .into_iter()
                                .find(|x| x.kind() == "identifier")
                            {
                                let ename = self.b.text(n);
                                if let Some(eid) =
                                    self.b.define(SymbolKind::Variable, ename, Some(id), m, n)
                                {
                                    let sym = self.b.sym(eid);
                                    sym.exported = true;
                                    sym.attrs.insert("member".into(), true.into());
                                    sym.attrs.insert("static".into(), true.into());
                                    sym.attrs.insert("enum_constant".into(), true.into());
                                    sym.attrs
                                        .insert("type_path".into(), vec![name.clone()].into());
                                }
                            }
                            self.walk_children(m, inner, depth + 2);
                        } else {
                            self.member(m, inner, depth + 2, is_object);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn member(&mut self, node: Node, cx: Cx, depth: u32, is_static: bool) {
        match node.kind() {
            "function_declaration" => self.function(node, cx, depth, is_static),
            "property_declaration" => self.property(node, cx, depth, is_static),
            _ => self.walk(node, cx, depth),
        }
    }

    fn class_parameter(&mut self, p: Node, owner: u32, cx: Cx) {
        let is_property = children(p)
            .iter()
            .any(|c| c.kind() == "val" || c.kind() == "var");
        if !is_property {
            return;
        }
        let (keywords, _) = self.modifiers(p, cx);
        let kids = named_children(p);
        let Some(n) = kids.iter().copied().find(|c| c.kind() == "identifier") else {
            return;
        };
        let ty = kids
            .iter()
            .copied()
            .find(|c| TYPE_NODES.contains(&c.kind()));
        let path = ty.and_then(|t| self.type_path(t));
        let sig = ty.map(|t| self.b.text_truncated(t));
        let name = self.b.text(n);
        if let Some(fid) = self.b.define(SymbolKind::Variable, name, Some(owner), p, n) {
            let sym = self.b.sym(fid);
            sym.exported = Self::exported(&keywords);
            sym.signature = sig;
            sym.attrs.insert("member".into(), true.into());
            if !keywords.is_empty() {
                sym.attrs.insert("modifiers".into(), keywords.into());
            }
            if let Some(path) = path {
                sym.attrs.insert("type_path".into(), path.into());
            }
        }
    }

    fn property(&mut self, node: Node, cx: Cx, depth: u32, is_static: bool) {
        let kids = named_children(node);
        let value = kids.iter().copied().find(|c| {
            !matches!(
                c.kind(),
                "modifiers"
                    | "variable_declaration"
                    | "multi_variable_declaration"
                    | "getter"
                    | "setter"
                    | "property_delegate"
                    | "type_parameters"
                    | "type_constraints"
            ) && !TYPE_NODES.contains(&c.kind())
        });
        let decl = kids
            .iter()
            .copied()
            .find(|c| c.kind() == "variable_declaration");
        if cx.fn_scope.is_some() {
            // A local `val`/`var`.
            match decl {
                Some(d) => {
                    let (name, ty) = self.variable(d);
                    if let Some(n) = name {
                        match (ty, value) {
                            (Some(ty), _) => self.bind(cx, n, Some(ty)),
                            (None, Some(v)) if v.kind() == "call_expression" => {
                                let callee = v
                                    .named_child(0)
                                    .map(|c| self.target(c).path)
                                    .unwrap_or_default();
                                let local = self.b.text(n);
                                self.b.add_local(cx.fn_scope, &local);
                                self.types.bind_call(cx.fn_scope, &local, callee, 0);
                            }
                            (None, Some(v)) if v.kind() == "as_expression" => {
                                let ty = v
                                    .child_by_field_name("right")
                                    .and_then(|t| self.type_path(t));
                                self.bind(cx, n, ty);
                            }
                            _ => self.bind(cx, n, None),
                        }
                    }
                }
                None => {
                    for m in kids
                        .iter()
                        .filter(|c| c.kind() == "multi_variable_declaration")
                    {
                        for n in self.declared_names(*m) {
                            self.bind(cx, n, None);
                        }
                    }
                }
            }
            if let Some(v) = value {
                self.walk(v, cx, depth + 1);
            }
            return;
        }
        let (keywords, annotations) = self.modifiers(node, cx);
        let Some(d) = decl else {
            if let Some(v) = value {
                self.walk(v, cx, depth + 1);
            }
            return;
        };
        let (name_node, declared) = self.variable(d);
        let Some(n) = name_node else { return };
        let name = self.b.text(n);
        let doc = self.doc(node);
        let sig = named_children(d)
            .into_iter()
            .find(|c| TYPE_NODES.contains(&c.kind()))
            .map(|t| self.b.text_truncated(t));
        // `val x = Foo()`: the type of a call of a class name.
        let init_call = value
            .filter(|v| v.kind() == "call_expression")
            .and_then(|v| v.named_child(0))
            .map(|c| self.target(c).path)
            .filter(|p| !p.is_empty());
        let in_class = cx.scope.is_some();
        if let Some(id) = self.b.define(SymbolKind::Variable, name, cx.scope, node, n) {
            let sym = self.b.sym(id);
            sym.exported = Self::exported(&keywords);
            sym.doc = doc;
            sym.signature = sig;
            if in_class {
                sym.attrs.insert("member".into(), true.into());
            }
            if is_static || keywords.iter().any(|k| k == "const") {
                sym.attrs.insert("static".into(), true.into());
            }
            if !keywords.is_empty() {
                sym.attrs.insert("modifiers".into(), keywords.into());
            }
            if !annotations.is_empty() {
                sym.attrs.insert("annotations".into(), annotations.into());
            }
            match (declared, init_call) {
                (Some(t), _) => {
                    sym.attrs.insert("type_path".into(), t.into());
                }
                (None, Some(callee)) => {
                    sym.attrs.insert(
                        "init_call".into(),
                        serde_json::json!({ "callee": callee, "result": 0 }),
                    );
                }
                _ => {}
            }
        }
        for c in kids {
            if matches!(c.kind(), "getter" | "setter" | "property_delegate")
                || Some(c.id()) == value.map(|v| v.id())
            {
                self.walk(c, cx, depth + 1);
            }
        }
    }

    fn doc(&self, node: Node) -> Option<String> {
        doc_comment(&self.b, node)
    }

    fn function(&mut self, node: Node, cx: Cx, depth: u32, is_static: bool) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let (keywords, annotations) = self.modifiers(node, cx);
        let name = self.b.text(name_node);
        let parent_kind = cx.scope.map(|s| self.b.symbols[s as usize].kind);
        let in_type = matches!(
            parent_kind,
            Some(SymbolKind::Class) | Some(SymbolKind::Interface)
        );
        let kind = if in_type {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, &name);
        }
        let doc = self.doc(node);
        let Some(id) = self.b.define(kind, name, cx.scope, node, name_node) else {
            return;
        };
        let kids = named_children(node);
        let params = kids
            .iter()
            .copied()
            .find(|c| c.kind() == "function_value_parameters");
        // The return type follows the parameters; an extension receiver
        // type precedes the name.
        let ret = params.and_then(|p| {
            kids.iter()
                .copied()
                .find(|c| c.start_byte() > p.end_byte() && TYPE_NODES.contains(&c.kind()))
        });
        let receiver = kids
            .iter()
            .copied()
            .find(|c| c.end_byte() <= name_node.start_byte() && TYPE_NODES.contains(&c.kind()));
        let mut signature = params
            .map(|p| self.b.text_truncated(p))
            .unwrap_or_else(|| "()".into());
        if let Some(r) = ret {
            signature.push_str(": ");
            signature.push_str(&self.b.text_truncated(r));
        }
        let result = ret.and_then(|t| self.type_path(t));
        let receiver_type = receiver.and_then(|t| self.type_path(t));
        let in_interface = parent_kind == Some(SymbolKind::Interface);
        let has_body = kids.iter().any(|c| c.kind() == "function_body");
        {
            let sym = self.b.sym(id);
            sym.signature = Some(signature);
            sym.doc = doc;
            sym.exported = Self::exported(&keywords);
            if is_static {
                sym.attrs.insert("static".into(), true.into());
            }
            if keywords.iter().any(|k| k == "abstract") || (in_interface && !has_body) {
                sym.attrs.insert("abstract".into(), true.into());
            }
            if let Some(r) = result {
                sym.attrs.insert(
                    "result_types".into(),
                    vec![serde_json::Value::from(r)].into(),
                );
            }
            if let Some(r) = receiver_type {
                // Extension functions are called on values of this type.
                sym.attrs.insert("extension_receiver".into(), r.into());
            }
            if !keywords.is_empty() {
                sym.attrs.insert("modifiers".into(), keywords.into());
            }
            if !annotations.is_empty() {
                sym.attrs.insert("annotations".into(), annotations.into());
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
        for c in kids {
            if c.kind() == "function_body" {
                self.walk(c, inner, depth + 1);
            }
        }
    }

    fn constructor(&mut self, node: Node, cx: Cx, depth: u32) {
        let class_name = cx
            .scope
            .map(|s| self.b.symbols[s as usize].name.clone())
            .unwrap_or_else(|| "constructor".into());
        let (keywords, _) = self.modifiers(node, cx);
        let Some(id) = self
            .b
            .define(SymbolKind::Method, class_name, cx.scope, node, node)
        else {
            return;
        };
        {
            let sym = self.b.sym(id);
            sym.exported = Self::exported(&keywords);
            sym.attrs.insert("constructor".into(), true.into());
        }
        let inner = Cx {
            scope: Some(id),
            fn_scope: Some(id),
            exported: false,
            anon: false,
        };
        for c in named_children(node) {
            match c.kind() {
                "function_value_parameters" => self.parameters(c, inner),
                "block" | "constructor_delegation_call" => self.walk(c, inner, depth + 1),
                _ => {}
            }
        }
    }

    fn parameters(&mut self, params: Node, cx: Cx) {
        for p in named_children(params) {
            if p.kind() != "parameter" {
                continue;
            }
            let kids = named_children(p);
            let ty = kids
                .iter()
                .copied()
                .find(|c| TYPE_NODES.contains(&c.kind()))
                .and_then(|t| self.type_path(t));
            if let Some(n) = kids.iter().copied().find(|c| c.kind() == "identifier") {
                self.bind(cx, n, ty);
            }
        }
    }

    /// Segments of a named type without type arguments.
    fn type_path(&self, ty: Node) -> Option<Vec<String>> {
        match ty.kind() {
            "user_type" => {
                let segs: Vec<String> = named_children(ty)
                    .into_iter()
                    .filter(|c| matches!(c.kind(), "identifier" | "type_identifier"))
                    .map(|c| self.b.text(c))
                    .collect();
                (!segs.is_empty()).then_some(segs)
            }
            "nullable_type" | "parenthesized_type" | "non_nullable_type" => named_children(ty)
                .into_iter()
                .find_map(|c| self.type_path(c)),
            "identifier" | "type_identifier" => Some(vec![self.b.text(ty)]),
            _ => None,
        }
    }

    fn dotted(&self, expr: Node) -> Option<Vec<String>> {
        match expr.kind() {
            "identifier" => Some(vec![self.b.text(expr)]),
            "this_expression" => Some(vec!["this".into()]),
            "super_expression" => Some(vec!["super".into()]),
            "navigation_expression" => {
                let kids = named_children(expr);
                let (first, last) = (kids.first()?, kids.last()?);
                if kids.len() != 2 || last.kind() != "identifier" {
                    return None;
                }
                let mut path = self.dotted(*first)?;
                path.push(self.b.text(*last));
                Some(path)
            }
            "parenthesized_expression" => self.dotted(expr.named_child(0)?),
            _ => None,
        }
    }

    fn target(&self, callee: Node) -> Target {
        if let Some(path) = self.dotted(callee) {
            return Target {
                path,
                ..Default::default()
            };
        }
        if callee.kind() == "navigation_expression" {
            let kids = named_children(callee);
            let member = kids
                .last()
                .filter(|l| l.kind() == "identifier")
                .map(|l| self.b.text(*l));
            let chain = match (kids.first(), &member) {
                (Some(e), Some(m)) if kids.len() == 2 => self
                    .chain(*e, 0)
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
            "identifier" | "this_expression" | "super_expression" => self.dotted(expr),
            "navigation_expression" => {
                let kids = named_children(expr);
                if kids.len() != 2 || kids[1].kind() != "identifier" {
                    return None;
                }
                let mut c = self.chain(kids[0], depth + 1)?;
                c.push(self.b.text(kids[1]));
                Some(c)
            }
            "call_expression" => {
                let mut c = self.chain(expr.named_child(0)?, depth + 1)?;
                c.push("()".into());
                Some(c)
            }
            "as_expression" => {
                let ty = self.type_path(expr.child_by_field_name("right")?)?;
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
