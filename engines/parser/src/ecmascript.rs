//! JavaScript / TypeScript / TSX extraction adapter.
//!
//! One adapter serves the three grammars: the TypeScript grammars are
//! supersets of the JavaScript one and differ only in a handful of node kinds
//! (`class_heritage` layout, parameter wrappers, field definitions).

use crate::common::{
    children, named_children, span, truncate, Builder, Cx, Extracted, Limits, Target, MAX_DEPTH,
};
use tree_sitter::Node;
use usie_ir::{Export, ExportKind, Import, ImportKind, Language, ReferenceKind, SymbolKind};

pub(crate) fn extract(root: Node, src: &[u8], _language: Language, limits: Limits) -> Extracted {
    let mut es = Es {
        b: Builder::new(src, limits),
    };
    es.walk(root, Cx::default(), 0);
    es.mark_clause_exports();
    es.b.finish()
}

struct Es<'a> {
    b: Builder<'a>,
}

impl<'a> Es<'a> {
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
            // Types carry no executable references.
            "type_annotation"
            | "type_arguments"
            | "type_parameters"
            | "type_predicate_annotation"
            | "asserts_annotation"
            | "comment" => {}
            "export_statement" => self.export(node, cx, depth),
            "import_statement" => self.import(node, cx),
            "ambient_declaration" => self.walk_children(node, cx, depth),
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                self.function_decl(node, cx, depth, None);
            }
            "class_declaration" | "abstract_class_declaration" => {
                self.class(node, cx, depth, None);
            }
            "interface_declaration" => self.interface(node, cx, depth),
            "type_alias_declaration" => self.type_like(node, cx, "alias"),
            "enum_declaration" => {
                self.type_like(node, cx, "enum");
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(body, cx, depth + 1);
                }
            }
            "internal_module" | "module" => self.namespace(node, cx, depth),
            "method_definition" | "method_signature" | "abstract_method_signature" => {
                self.method(node, cx, depth)
            }
            "public_field_definition" | "field_definition" => self.field(node, cx, depth),
            "lexical_declaration" | "variable_declaration" => self.variables(node, cx, depth),
            "call_expression" => self.call(node, cx, depth),
            "new_expression" => {
                if let Some(ctor) = node.child_by_field_name("constructor") {
                    let target = self.target(ctor);
                    self.b
                        .push_reference(ReferenceKind::New, cx, target, ctor, node);
                }
                self.walk_children(node, cx, depth);
            }
            "jsx_opening_element" | "jsx_self_closing_element" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.jsx(name, cx);
                }
                self.walk_children(node, cx, depth);
            }
            "decorator" => {
                if let Some(expr) = node.named_child(0) {
                    match expr.kind() {
                        "identifier" | "member_expression" => {
                            let target = self.target(expr);
                            self.b
                                .push_reference(ReferenceKind::Decorator, cx, target, expr, expr);
                        }
                        _ => self.walk(expr, cx, depth + 1),
                    }
                }
            }
            "arrow_function" | "function_expression" | "generator_function" => {
                self.anonymous_function(node, cx, depth)
            }
            "class" => self.class_expression(node, cx, depth),
            "catch_clause" => {
                if let Some(p) = node.child_by_field_name("parameter") {
                    self.bind_pattern(p, cx);
                }
                self.walk_children(node, cx, depth);
            }
            "for_in_statement" => {
                if let Some(left) = node.child_by_field_name("left") {
                    let declares = children(node)
                        .iter()
                        .any(|c| matches!(c.kind(), "const" | "let" | "var"));
                    if declares {
                        self.bind_pattern(left, cx);
                    }
                }
                self.walk_children(node, cx, depth);
            }
            "assignment_expression" => {
                if cx.scope.is_none() && !cx.anon {
                    self.commonjs_export(node);
                }
                self.walk_children(node, cx, depth);
            }
            _ => self.walk_children(node, cx, depth),
        }
    }

    // ---- declarations -----------------------------------------------------

    /// Declares a function symbol. `name_override` names anonymous default exports.
    fn function_decl(
        &mut self,
        node: Node,
        cx: Cx,
        depth: u32,
        name_override: Option<&str>,
    ) -> Option<u32> {
        let name_node = node.child_by_field_name("name");
        let name = match (name_node, name_override) {
            (Some(n), _) => self.b.text(n),
            (None, Some(o)) => o.to_string(),
            (None, None) => {
                self.walk_children(node, cx, depth);
                return None;
            }
        };
        let has_body = node.child_by_field_name("body").is_some();
        // Overload signatures and their implementation share one symbol.
        let existing = self
            .b
            .find_child(cx.scope, &name, SymbolKind::Function)
            .filter(|id| self.b.symbols[*id as usize].attrs.contains_key("overloads"));
        let id = match existing {
            Some(id) => {
                if has_body {
                    let sp = span(node);
                    let sym = self.b.sym(id);
                    sym.span = sp;
                    sym.attrs.remove("declaration_only");
                }
                id
            }
            None => {
                self.bind_name(cx, &name);
                let id = self.b.define(
                    SymbolKind::Function,
                    name,
                    cx.scope,
                    node,
                    name_node.unwrap_or(node),
                )?;
                if cx.anon && cx.scope.is_none() {
                    self.b
                        .sym(id)
                        .attrs
                        .insert("in_anonymous_scope".into(), true.into());
                }
                id
            }
        };
        let signature = self.signature(node);
        let doc = self.jsdoc(node);
        let is_async = children(node).iter().any(|c| c.kind() == "async");
        let is_generator = node.kind().starts_with("generator");
        {
            let sym = self.b.sym(id);
            sym.exported |= cx.exported;
            if has_body || sym.signature.is_none() {
                sym.signature = Some(signature);
            }
            if sym.doc.is_none() {
                sym.doc = doc;
            }
            if is_async {
                sym.attrs.insert("async".into(), true.into());
            }
            if is_generator {
                sym.attrs.insert("generator".into(), true.into());
            }
            if !has_body {
                let n = sym
                    .attrs
                    .get("overloads")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                sym.attrs.insert("overloads".into(), (n + 1).into());
                sym.attrs.insert("declaration_only".into(), true.into());
            }
        }
        self.function_body(node, id, cx, depth);
        Some(id)
    }

    /// Walks parameters and body of a function-like node owned by symbol `id`.
    fn function_body(&mut self, node: Node, id: u32, outer: Cx, depth: u32) {
        let inner = Cx {
            scope: Some(id),
            fn_scope: Some(id),
            exported: false,
            anon: false,
        };
        let _ = outer;
        for field in ["parameter", "parameters"] {
            if let Some(params) = node.child_by_field_name(field) {
                self.bind_pattern(params, inner);
                // Default values may call functions; attribute them to this symbol.
                self.walk(params, inner, depth + 1);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, inner, depth + 1);
        }
    }

    fn class(
        &mut self,
        node: Node,
        cx: Cx,
        depth: u32,
        name_override: Option<&str>,
    ) -> Option<u32> {
        let name_node = node.child_by_field_name("name");
        let name = match (name_node, name_override) {
            (Some(n), _) => self.b.text(n),
            (None, Some(o)) => o.to_string(),
            (None, None) => {
                self.walk_children(node, cx, depth);
                return None;
            }
        };
        self.bind_name(cx, &name);
        let id = self.b.define(
            SymbolKind::Class,
            name,
            cx.scope,
            node,
            name_node.unwrap_or(node),
        )?;
        let doc = self.jsdoc(node);
        let is_abstract = node.kind() == "abstract_class_declaration";
        {
            let anon_scope = cx.anon && cx.scope.is_none();
            let sym = self.b.sym(id);
            sym.exported = cx.exported;
            sym.doc = doc;
            if is_abstract {
                sym.attrs.insert("abstract".into(), true.into());
            }
            if anon_scope {
                sym.attrs.insert("in_anonymous_scope".into(), true.into());
            }
        }
        self.class_parts(node, id, cx, depth);
        Some(id)
    }

    /// Heritage, decorators and body of a class node already declared as `id`.
    fn class_parts(&mut self, node: Node, id: u32, cx: Cx, depth: u32) {
        let outer = Cx {
            exported: false,
            ..cx
        };
        let mut decorators = Vec::new();
        for child in children(node) {
            match child.kind() {
                "decorator" => {
                    if let Some(e) = child.named_child(0) {
                        decorators.push(self.b.text_truncated(e));
                    }
                    self.walk(child, outer, depth + 1);
                }
                "class_heritage" => self.heritage(child, id, outer, depth + 1),
                _ => {}
            }
        }
        if !decorators.is_empty() {
            self.b
                .sym(id)
                .attrs
                .insert("decorators".into(), decorators.into());
        }
        if let Some(body) = node.child_by_field_name("body") {
            let inner = Cx {
                scope: Some(id),
                fn_scope: None,
                exported: false,
                anon: false,
            };
            self.walk_children(body, inner, depth + 1);
        }
    }

    fn heritage(&mut self, heritage: Node, id: u32, cx: Cx, depth: u32) {
        let mut plain_js = true;
        for clause in named_children(heritage) {
            match clause.kind() {
                "extends_clause" => {
                    plain_js = false;
                    let mut cursor = clause.walk();
                    let values: Vec<Node> = clause
                        .children_by_field_name("value", &mut cursor)
                        .collect();
                    for value in values {
                        let segments = self.dotted(value).unwrap_or_default();
                        let base = self.b.name_path(value, segments);
                        self.b.sym(id).bases.push(base);
                        self.walk(value, cx, depth + 1);
                    }
                }
                "implements_clause" => {
                    plain_js = false;
                    for ty in named_children(clause) {
                        let segments = self.type_name(ty).unwrap_or_default();
                        let path = self.b.name_path(ty, segments);
                        self.b.sym(id).implements.push(path);
                    }
                }
                _ => {}
            }
        }
        if plain_js {
            // JavaScript grammar: `class_heritage` is `extends <expression>`.
            if let Some(value) = heritage.named_child(0) {
                let segments = self.dotted(value).unwrap_or_default();
                let base = self.b.name_path(value, segments);
                self.b.sym(id).bases.push(base);
                self.walk(value, cx, depth + 1);
            }
        }
    }

    fn interface(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.b.text(name_node);
        // Declaration merging: repeated interfaces extend the first symbol.
        let id = match self.b.find_child(cx.scope, &name, SymbolKind::Interface) {
            Some(id) => id,
            None => {
                let Some(id) =
                    self.b
                        .define(SymbolKind::Interface, name, cx.scope, node, name_node)
                else {
                    return;
                };
                id
            }
        };
        let doc = self.jsdoc(node);
        {
            let sym = self.b.sym(id);
            sym.exported |= cx.exported;
            if sym.doc.is_none() {
                sym.doc = doc;
            }
        }
        for child in named_children(node) {
            if child.kind() == "extends_type_clause" {
                for ty in named_children(child) {
                    let segments = self.type_name(ty).unwrap_or_default();
                    let path = self.b.name_path(ty, segments);
                    self.b.sym(id).bases.push(path);
                }
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            let inner = Cx {
                scope: Some(id),
                fn_scope: None,
                exported: false,
                anon: false,
            };
            for member in named_children(body) {
                if member.kind() == "method_signature" {
                    self.method(member, inner, depth + 1);
                }
            }
        }
    }

    fn type_like(&mut self, node: Node, cx: Cx, type_kind: &str) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.b.text(name_node);
        if type_kind == "enum" {
            // Enums are values as well as types.
            self.bind_name(cx, &name);
        }
        if let Some(id) = self
            .b
            .define(SymbolKind::Type, name, cx.scope, node, name_node)
        {
            let doc = self.jsdoc(node);
            let sym = self.b.sym(id);
            sym.exported = cx.exported;
            sym.doc = doc;
            sym.attrs.insert("type_kind".into(), type_kind.into());
        }
    }

    fn namespace(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let ambient_module = name_node.kind() == "string";
        let name = if ambient_module {
            self.string_value(name_node)
        } else {
            self.b.text(name_node)
        };
        let id = match self.b.find_child(cx.scope, &name, SymbolKind::Namespace) {
            Some(id) => id,
            None => {
                let Some(id) =
                    self.b
                        .define(SymbolKind::Namespace, name, cx.scope, node, name_node)
                else {
                    return;
                };
                id
            }
        };
        {
            let sym = self.b.sym(id);
            sym.exported |= cx.exported;
            if ambient_module {
                sym.attrs.insert("ambient_module".into(), true.into());
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            let inner = Cx {
                scope: Some(id),
                fn_scope: None,
                exported: false,
                anon: false,
            };
            self.walk_children(body, inner, depth + 1);
        }
    }

    fn method(&mut self, node: Node, cx: Cx, depth: u32) {
        let owner_ok = cx
            .scope
            .map(|s| {
                matches!(
                    self.b.symbols[s as usize].kind,
                    SymbolKind::Class | SymbolKind::Interface
                )
            })
            .unwrap_or(false);
        let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
        let in_type_body = matches!(parent_kind, "class_body" | "interface_body" | "object_type");
        let name_node = node.child_by_field_name("name");
        let name = name_node.and_then(|n| self.member_name(n));
        let (Some(name_node), Some(name), true, true) = (name_node, name, owner_ok, in_type_body)
        else {
            // Object-literal methods and computed names: no symbol, but the
            // body is still a function scope of its own.
            self.anonymous_function(node, cx, depth);
            return;
        };
        let has_body = node.child_by_field_name("body").is_some();
        let existing = self
            .b
            .find_child(cx.scope, &name, SymbolKind::Method)
            .filter(|id| {
                self.b.symbols[*id as usize]
                    .attrs
                    .contains_key("declaration_only")
            });
        let id = match existing {
            Some(id) => {
                if has_body {
                    let sp = span(node);
                    let sym = self.b.sym(id);
                    sym.span = sp;
                    sym.attrs.remove("declaration_only");
                }
                id
            }
            None => {
                let Some(id) = self
                    .b
                    .define(SymbolKind::Method, name, cx.scope, node, name_node)
                else {
                    return;
                };
                id
            }
        };
        let signature = self.signature(node);
        let doc = self.jsdoc(node);
        let mut decorators = Vec::new();
        let mut flags: Vec<&'static str> = Vec::new();
        let mut accessibility = None;
        for child in children(node) {
            match child.kind() {
                "static" => flags.push("static"),
                "async" => flags.push("async"),
                "get" => flags.push("getter"),
                "set" => flags.push("setter"),
                "abstract" => flags.push("abstract"),
                "*" => flags.push("generator"),
                "accessibility_modifier" => accessibility = Some(self.b.text(child)),
                "decorator" => {
                    if let Some(e) = child.named_child(0) {
                        decorators.push(self.b.text_truncated(e));
                    }
                    self.walk(child, cx, depth + 1);
                }
                _ => {}
            }
        }
        if node.kind() == "abstract_method_signature" {
            flags.push("abstract");
        }
        // Decorators on methods precede the definition as siblings in the class body.
        let mut prev = node.prev_sibling();
        while let Some(p) = prev {
            if p.kind() != "decorator" {
                break;
            }
            if let Some(e) = p.named_child(0) {
                decorators.insert(0, self.b.text_truncated(e));
            }
            prev = p.prev_sibling();
        }
        {
            let sym = self.b.sym(id);
            if has_body || sym.signature.is_none() {
                sym.signature = Some(signature);
            }
            if sym.doc.is_none() {
                sym.doc = doc;
            }
            for f in flags {
                sym.attrs.insert(f.into(), true.into());
            }
            if let Some(a) = accessibility {
                sym.attrs.insert("accessibility".into(), a.into());
            }
            if !decorators.is_empty() {
                sym.attrs.insert("decorators".into(), decorators.into());
            }
            if !has_body {
                sym.attrs.insert("declaration_only".into(), true.into());
            }
        }
        self.function_body(node, id, cx, depth);
    }

    fn field(&mut self, node: Node, cx: Cx, depth: u32) {
        let in_class = node
            .parent()
            .map(|p| p.kind() == "class_body")
            .unwrap_or(false)
            && cx
                .scope
                .map(|s| self.b.symbols[s as usize].kind == SymbolKind::Class)
                .unwrap_or(false);
        let name_node = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("property"));
        let name = name_node.and_then(|n| self.member_name(n));
        let value = node.child_by_field_name("value");
        let (Some(name_node), Some(name), true) = (name_node, name, in_class) else {
            self.walk_children(node, cx, depth);
            return;
        };
        for child in children(node) {
            if child.kind() == "decorator" {
                self.walk(child, cx, depth + 1);
            }
        }
        let is_static = children(node).iter().any(|c| c.kind() == "static");
        let function_value = value.filter(|v| is_function_value(*v));
        let kind = if function_value.is_some() {
            SymbolKind::Method
        } else {
            SymbolKind::Variable
        };
        let Some(id) = self.b.define(kind, name, cx.scope, node, name_node) else {
            return;
        };
        let doc = self.jsdoc(node);
        let signature = function_value.map(|f| self.signature(f));
        {
            let sym = self.b.sym(id);
            sym.doc = doc;
            sym.signature = signature;
            sym.attrs.insert("member".into(), true.into());
            if is_static {
                sym.attrs.insert("static".into(), true.into());
            }
            if let Some(f) = function_value {
                if f.kind() == "arrow_function" {
                    sym.attrs.insert("arrow".into(), true.into());
                }
            }
        }
        match (function_value, value) {
            (Some(f), _) => self.function_body(f, id, cx, depth + 1),
            (None, Some(v)) => {
                let inner = Cx {
                    scope: Some(id),
                    fn_scope: Some(id),
                    exported: false,
                    anon: false,
                };
                self.walk(v, inner, depth + 1);
            }
            _ => {}
        }
    }

    fn variables(&mut self, node: Node, cx: Cx, depth: u32) {
        let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
        let grand_kind = node
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.kind())
            .unwrap_or("");
        let is_namespace_scope = cx
            .scope
            .map(|s| self.b.symbols[s as usize].kind == SymbolKind::Namespace)
            .unwrap_or(false);
        // Declarations that create module-level (or namespace-level) bindings.
        let top_level = cx.fn_scope.is_none()
            && !cx.anon
            && (cx.scope.is_none() || is_namespace_scope)
            && (matches!(
                parent_kind,
                "program" | "export_statement" | "ambient_declaration"
            ) || (is_namespace_scope && parent_kind == "statement_block"))
            && !(parent_kind == "export_statement"
                && grand_kind == "statement_block"
                && !is_namespace_scope);
        let decl_kind = children(node)
            .iter()
            .find(|c| matches!(c.kind(), "const" | "let" | "var"))
            .map(|c| c.kind())
            .unwrap_or("var");
        let doc = self.jsdoc(node);

        for decl in named_children(node) {
            if decl.kind() != "variable_declarator" {
                continue;
            }
            let Some(name_node) = decl.child_by_field_name("name") else {
                continue;
            };
            let value = decl.child_by_field_name("value");

            // CommonJS `const x = require("m")` / `const {a, b} = require("m")`.
            if let Some(module) = value.and_then(|v| self.require_call(v)) {
                self.require_import(name_node, &module, decl, cx);
                continue;
            }

            if !top_level {
                self.bind_pattern(name_node, cx);
                if let Some(v) = value {
                    self.walk(v, cx, depth + 1);
                }
                continue;
            }

            if name_node.kind() != "identifier" {
                // Destructuring at module level: one variable per bound name.
                let mut names = Vec::new();
                pattern_names(name_node, &mut names, 0);
                let mut first = None;
                for n in names {
                    let name = self.b.text(n);
                    if let Some(id) = self.b.define(SymbolKind::Variable, name, cx.scope, decl, n) {
                        let sym = self.b.sym(id);
                        sym.exported = cx.exported;
                        sym.attrs.insert("declaration".into(), decl_kind.into());
                        sym.attrs.insert("destructured".into(), true.into());
                        first.get_or_insert(id);
                    }
                }
                if let Some(v) = value {
                    let inner = Cx {
                        scope: first.or(cx.scope),
                        fn_scope: first,
                        exported: false,
                        anon: first.is_none(),
                    };
                    self.walk(name_node, inner, depth + 1);
                    self.walk(v, inner, depth + 1);
                }
                continue;
            }

            let name = self.b.text(name_node);
            let function_value = value.filter(|v| is_function_value(*v));
            let class_value = value.filter(|v| v.kind() == "class");
            let kind = if function_value.is_some() {
                SymbolKind::Function
            } else if class_value.is_some() {
                SymbolKind::Class
            } else {
                SymbolKind::Variable
            };
            let Some(id) = self.b.define(kind, name, cx.scope, decl, name_node) else {
                continue;
            };
            let signature = function_value.map(|f| self.signature(f));
            {
                let sym = self.b.sym(id);
                sym.exported = cx.exported;
                sym.doc = doc.clone();
                sym.signature = signature;
                sym.attrs.insert("declaration".into(), decl_kind.into());
                if let Some(f) = function_value {
                    if f.kind() == "arrow_function" {
                        sym.attrs.insert("arrow".into(), true.into());
                    }
                    if children(f).iter().any(|c| c.kind() == "async") {
                        sym.attrs.insert("async".into(), true.into());
                    }
                }
            }
            if let Some(f) = function_value {
                self.function_body(f, id, cx, depth + 1);
            } else if let Some(c) = class_value {
                self.class_parts(
                    c,
                    id,
                    Cx {
                        exported: false,
                        ..cx
                    },
                    depth + 1,
                );
            } else if let Some(v) = value {
                let inner = Cx {
                    scope: Some(id),
                    fn_scope: Some(id),
                    exported: false,
                    anon: false,
                };
                self.walk(v, inner, depth + 1);
            }
        }
    }

    fn anonymous_function(&mut self, node: Node, cx: Cx, depth: u32) {
        // Without an owning function symbol, bindings are tracked file-wide.
        let inner = if cx.fn_scope.is_some() {
            cx
        } else {
            Cx {
                anon: true,
                exported: false,
                ..cx
            }
        };
        if node.kind() == "function_expression" {
            if let Some(name) = node.child_by_field_name("name") {
                self.bind_pattern(name, inner);
            }
        }
        for field in ["parameter", "parameters"] {
            if let Some(params) = node.child_by_field_name(field) {
                self.bind_pattern(params, inner);
                self.walk(params, inner, depth + 1);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, inner, depth + 1);
        }
    }

    fn class_expression(&mut self, node: Node, cx: Cx, depth: u32) {
        // An inline class expression gets no symbol; its members are walked
        // as anonymous code so their calls are still recorded.
        let inner = if cx.fn_scope.is_some() {
            cx
        } else {
            Cx {
                anon: true,
                exported: false,
                ..cx
            }
        };
        if let Some(name) = node.child_by_field_name("name") {
            self.bind_pattern(name, inner);
        }
        for child in children(node) {
            match child.kind() {
                "class_heritage" | "decorator" => self.walk_children(child, inner, depth + 1),
                "class_body" => self.walk_children(child, inner, depth + 1),
                _ => {}
            }
        }
    }

    // ---- imports / exports ------------------------------------------------

    fn import(&mut self, node: Node, cx: Cx) {
        let type_only = children(node)
            .iter()
            .any(|c| c.kind() == "type" || c.kind() == "typeof");
        let scope = cx.fn_scope.or(cx.scope);

        // TypeScript `import x = require("m")`.
        for child in named_children(node) {
            if child.kind() == "import_require_clause" {
                let local = child.named_child(0).map(|n| self.b.text(n));
                let module = child
                    .child_by_field_name("source")
                    .map(|s| self.string_value(s))
                    .unwrap_or_default();
                if module.is_empty() {
                    return;
                }
                if let Some(l) = &local {
                    self.bind_name(cx, l);
                }
                self.b.push_import(Import {
                    module,
                    level: 0,
                    kind: ImportKind::Namespace,
                    imported: None,
                    local,
                    scope,
                    type_only,
                    span: span(node),
                });
                return;
            }
        }

        let Some(source) = node.child_by_field_name("source") else {
            return;
        };
        let module = self.string_value(source);
        if module.is_empty() {
            return;
        }
        let clause = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "import_clause");
        let Some(clause) = clause else {
            self.b.push_import(Import {
                module,
                level: 0,
                kind: ImportKind::SideEffect,
                imported: None,
                local: None,
                scope,
                type_only: false,
                span: span(node),
            });
            return;
        };
        for part in named_children(clause) {
            match part.kind() {
                "identifier" => {
                    let local = self.b.text(part);
                    self.bind_name(cx, &local);
                    self.b.push_import(Import {
                        module: module.clone(),
                        level: 0,
                        kind: ImportKind::Default,
                        imported: Some("default".into()),
                        local: Some(local),
                        scope,
                        type_only,
                        span: span(node),
                    });
                }
                "namespace_import" => {
                    let local = named_children(part)
                        .into_iter()
                        .find(|c| c.kind() == "identifier")
                        .map(|n| self.b.text(n));
                    if let Some(l) = &local {
                        self.bind_name(cx, l);
                    }
                    self.b.push_import(Import {
                        module: module.clone(),
                        level: 0,
                        kind: ImportKind::Namespace,
                        imported: None,
                        local,
                        scope,
                        type_only,
                        span: span(node),
                    });
                }
                "named_imports" => {
                    for spec in named_children(part) {
                        if spec.kind() != "import_specifier" {
                            continue;
                        }
                        let Some(name_node) = spec.child_by_field_name("name") else {
                            continue;
                        };
                        let imported = self.name_or_string(name_node);
                        let local = spec
                            .child_by_field_name("alias")
                            .map(|a| self.b.text(a))
                            .unwrap_or_else(|| imported.clone());
                        let spec_type = children(spec).iter().any(|c| c.kind() == "type");
                        self.bind_name(cx, &local);
                        let kind = if imported == "default" {
                            ImportKind::Default
                        } else {
                            ImportKind::Named
                        };
                        self.b.push_import(Import {
                            module: module.clone(),
                            level: 0,
                            kind,
                            imported: Some(imported),
                            local: Some(local),
                            scope,
                            type_only: type_only || spec_type,
                            span: span(spec),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn export(&mut self, node: Node, cx: Cx, depth: u32) {
        // Only module-level exports form the module's export surface; exports
        // inside a namespace mark the member as exported from the namespace.
        let module_level = cx.scope.is_none() && !cx.anon;
        let source = node
            .child_by_field_name("source")
            .map(|s| self.string_value(s));
        let kids = children(node);
        let is_default = kids.iter().any(|c| c.kind() == "default");

        if let Some(source) = source {
            let mut pushed = false;
            for child in &kids {
                match child.kind() {
                    "export_clause" => {
                        pushed = true;
                        self.export_clause(*child, Some(source.clone()), module_level);
                    }
                    "namespace_export" => {
                        pushed = true;
                        let name = child
                            .named_child(0)
                            .map(|n| self.name_or_string(n))
                            .unwrap_or_default();
                        if module_level && !name.is_empty() {
                            self.b.exports.push(Export {
                                kind: ExportKind::Namespace,
                                name,
                                local: None,
                                source: Some(source.clone()),
                                span: span(node),
                            });
                        }
                    }
                    _ => {}
                }
            }
            if !pushed && module_level && kids.iter().any(|c| c.kind() == "*") {
                self.b.exports.push(Export {
                    kind: ExportKind::Wildcard,
                    name: "*".into(),
                    local: None,
                    source: Some(source),
                    span: span(node),
                });
            }
            return;
        }

        let inner = Cx {
            exported: true,
            ..cx
        };
        if let Some(decl) = node.child_by_field_name("declaration") {
            let before = self.b.symbols.len();
            let override_name = if is_default { Some("default") } else { None };
            let default_local = match decl.kind() {
                "function_declaration"
                | "generator_function_declaration"
                | "function_signature" => self
                    .function_decl(decl, inner, depth + 1, override_name)
                    .map(|id| self.b.symbols[id as usize].name.clone()),
                "class_declaration" | "abstract_class_declaration" => self
                    .class(decl, inner, depth + 1, override_name)
                    .map(|id| self.b.symbols[id as usize].name.clone()),
                _ => {
                    self.walk(decl, inner, depth + 1);
                    None
                }
            };
            if !module_level {
                return;
            }
            if is_default {
                self.b.exports.push(Export {
                    kind: ExportKind::Default,
                    name: "default".into(),
                    local: default_local,
                    source: None,
                    span: span(node),
                });
                return;
            }
            let mut seen = std::collections::BTreeSet::new();
            let mut names: Vec<String> = Vec::new();
            if let Some(n) = default_local {
                seen.insert(n.clone());
                names.push(n);
            }
            for sym in &self.b.symbols[before..] {
                if sym.parent == cx.scope && sym.exported && seen.insert(sym.name.clone()) {
                    names.push(sym.name.clone());
                }
            }
            for name in names {
                // Overloads and merged declarations export one name once.
                if self
                    .b
                    .exports
                    .iter()
                    .any(|e| e.source.is_none() && e.name == name)
                {
                    continue;
                }
                self.b.exports.push(Export {
                    kind: ExportKind::Named,
                    name: name.clone(),
                    local: Some(name),
                    source: None,
                    span: span(decl),
                });
            }
            return;
        }

        if let Some(value) = node.child_by_field_name("value") {
            // `export default <expression>`
            let local = match value.kind() {
                "identifier" => Some(self.b.text(value)),
                "arrow_function" | "function_expression" | "generator_function" if module_level => {
                    match self
                        .b
                        .define(SymbolKind::Function, "default".into(), None, value, value)
                    {
                        Some(id) => {
                            let signature = self.signature(value);
                            let sym = self.b.sym(id);
                            sym.exported = true;
                            sym.signature = Some(signature);
                            self.function_body(value, id, cx, depth + 1);
                            Some("default".to_string())
                        }
                        None => None,
                    }
                }
                "class" if module_level => {
                    match self
                        .b
                        .define(SymbolKind::Class, "default".into(), None, value, value)
                    {
                        Some(id) => {
                            self.b.sym(id).exported = true;
                            self.class_parts(value, id, cx, depth + 1);
                            Some("default".to_string())
                        }
                        None => None,
                    }
                }
                _ => {
                    self.walk(value, cx, depth + 1);
                    None
                }
            };
            if module_level {
                self.b.exports.push(Export {
                    kind: ExportKind::Default,
                    name: "default".into(),
                    local,
                    source: None,
                    span: span(node),
                });
            }
            return;
        }

        let mut handled = false;
        for child in &kids {
            if child.kind() == "export_clause" {
                handled = true;
                self.export_clause(*child, None, module_level);
            }
        }
        if handled {
            return;
        }

        // TypeScript `export = expr` and `export as namespace X`.
        if kids.iter().any(|c| c.kind() == "=") {
            let expr = kids.iter().rev().find(|c| c.is_named()).copied();
            if let Some(expr) = expr {
                let local = (expr.kind() == "identifier").then(|| self.b.text(expr));
                if local.is_none() {
                    self.walk(expr, cx, depth + 1);
                }
                if module_level {
                    self.b.attrs.insert("export_assignment".into(), true.into());
                    self.b.exports.push(Export {
                        kind: ExportKind::Default,
                        name: "default".into(),
                        local,
                        source: None,
                        span: span(node),
                    });
                }
            }
            return;
        }
        self.walk_children(node, inner, depth);
    }

    fn export_clause(&mut self, clause: Node, source: Option<String>, module_level: bool) {
        if !module_level {
            return;
        }
        for spec in named_children(clause) {
            if spec.kind() != "export_specifier" {
                continue;
            }
            let Some(name_node) = spec.child_by_field_name("name") else {
                continue;
            };
            let local = self.name_or_string(name_node);
            let name = spec
                .child_by_field_name("alias")
                .map(|a| self.name_or_string(a))
                .unwrap_or_else(|| local.clone());
            let kind = if name == "default" {
                ExportKind::Default
            } else {
                ExportKind::Named
            };
            self.b.exports.push(Export {
                kind,
                name,
                local: Some(local),
                source: source.clone(),
                span: span(spec),
            });
        }
    }

    /// `export { a }` / `export default a` mark the local symbol as exported.
    fn mark_clause_exports(&mut self) {
        let locals: std::collections::BTreeSet<String> = self
            .b
            .exports
            .iter()
            .filter(|e| e.source.is_none())
            .filter_map(|e| e.local.clone())
            .collect();
        if locals.is_empty() {
            return;
        }
        for sym in self.b.symbols.iter_mut() {
            if sym.parent.is_none() && locals.contains(&sym.name) {
                sym.exported = true;
            }
        }
    }

    /// `module.exports = …`, `module.exports.x = …`, `exports.x = …`.
    fn commonjs_export(&mut self, node: Node) {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return;
        };
        let Some(path) = self.dotted(left) else {
            return;
        };
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        let member = match path.as_slice() {
            ["module", "exports"] => None,
            ["module", "exports", name] | ["exports", name] => Some(name.to_string()),
            _ => return,
        };
        self.b.attrs.insert("commonjs".into(), true.into());
        if let Some(name) = &member {
            if self
                .b
                .exports
                .iter()
                .any(|e| e.source.is_none() && &e.name == name)
            {
                return;
            }
        }
        let value_name = (right.kind() == "identifier").then(|| self.b.text(right));
        match member {
            Some(name) => self.b.exports.push(Export {
                kind: ExportKind::Named,
                name,
                local: value_name,
                source: None,
                span: span(node),
            }),
            None if right.kind() == "object" => {
                for prop in named_children(right) {
                    let (name, local) = match prop.kind() {
                        "shorthand_property_identifier" => {
                            let n = self.b.text(prop);
                            (n.clone(), Some(n))
                        }
                        "pair" => {
                            let Some(key) = prop.child_by_field_name("key") else {
                                continue;
                            };
                            let Some(name) = self.member_name(key) else {
                                continue;
                            };
                            let local = prop
                                .child_by_field_name("value")
                                .filter(|v| v.kind() == "identifier")
                                .map(|v| self.b.text(v));
                            (name, local)
                        }
                        "method_definition" => {
                            let Some(name) = prop
                                .child_by_field_name("name")
                                .and_then(|n| self.member_name(n))
                            else {
                                continue;
                            };
                            (name, None)
                        }
                        _ => continue,
                    };
                    if self
                        .b
                        .exports
                        .iter()
                        .any(|e| e.source.is_none() && e.name == name)
                    {
                        continue;
                    }
                    self.b.exports.push(Export {
                        kind: ExportKind::Named,
                        name,
                        local,
                        source: None,
                        span: span(prop),
                    });
                }
            }
            None => self.b.exports.push(Export {
                kind: ExportKind::Default,
                name: "default".into(),
                local: value_name,
                source: None,
                span: span(node),
            }),
        }
    }

    // ---- references -------------------------------------------------------

    fn call(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(callee) = node.child_by_field_name("function") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let args = node.child_by_field_name("arguments");
        if callee.kind() == "import" {
            // Dynamic `import("m")`: only a literal specifier is resolvable.
            let module = args
                .and_then(|a| a.named_child(0))
                .filter(|a| a.kind() == "string")
                .map(|s| self.string_value(s));
            match module {
                Some(module) if !module.is_empty() => self.b.push_import(Import {
                    module,
                    level: 0,
                    kind: ImportKind::Dynamic,
                    imported: None,
                    local: None,
                    scope: cx.scope,
                    type_only: false,
                    span: span(node),
                }),
                _ => {
                    let n = self
                        .b
                        .attrs
                        .get("dynamic_imports_unresolved")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    self.b
                        .attrs
                        .insert("dynamic_imports_unresolved".into(), (n + 1).into());
                }
            }
            if let Some(a) = args {
                self.walk(a, cx, depth + 1);
            }
            return;
        }
        if let Some(module) = self.require_call(node) {
            // A bare `require("m")` not bound by a declaration.
            self.b.attrs.insert("commonjs".into(), true.into());
            self.b.push_import(Import {
                module,
                level: 0,
                kind: ImportKind::SideEffect,
                imported: None,
                local: None,
                scope: cx.scope,
                type_only: false,
                span: span(node),
            });
            return;
        }
        let target = self.target(callee);
        self.b
            .push_reference(ReferenceKind::Call, cx, target, callee, node);
        self.walk_children(node, cx, depth);
    }

    fn jsx(&mut self, name: Node, cx: Cx) {
        let target = match name.kind() {
            "identifier" => {
                let text = self.b.text(name);
                // Lowercase tags are host elements, not component references.
                if !text
                    .chars()
                    .next()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
                {
                    return;
                }
                Target {
                    path: vec![text],
                    member: None,
                }
            }
            "member_expression" | "nested_identifier" => {
                let text = self.b.text(name);
                let path: Vec<String> = text.split('.').map(|s| s.trim().to_string()).collect();
                if path.iter().any(|s| s.is_empty()) {
                    return;
                }
                Target { path, member: None }
            }
            _ => return,
        };
        self.b
            .push_reference(ReferenceKind::Jsx, cx, target, name, name);
    }

    /// The module of a `require("literal")` call expression.
    fn require_call(&self, node: Node) -> Option<String> {
        if node.kind() != "call_expression" {
            return None;
        }
        let callee = node.child_by_field_name("function")?;
        if callee.kind() != "identifier" || self.b.text(callee) != "require" {
            return None;
        }
        let args = node.child_by_field_name("arguments")?;
        if args.named_child_count() != 1 {
            return None;
        }
        let arg = args.named_child(0)?;
        if arg.kind() != "string" {
            return None;
        }
        let module = self.string_value(arg);
        (!module.is_empty()).then_some(module)
    }

    fn require_import(&mut self, name_node: Node, module: &str, decl: Node, cx: Cx) {
        self.b.attrs.insert("commonjs".into(), true.into());
        let scope = cx.fn_scope.or(cx.scope);
        match name_node.kind() {
            "identifier" => {
                let local = self.b.text(name_node);
                self.bind_name(cx, &local);
                self.b.push_import(Import {
                    module: module.to_string(),
                    level: 0,
                    kind: ImportKind::Namespace,
                    imported: None,
                    local: Some(local),
                    scope,
                    type_only: false,
                    span: span(decl),
                });
            }
            "object_pattern" => {
                for prop in named_children(name_node) {
                    let (imported, local) = match prop.kind() {
                        "shorthand_property_identifier_pattern" => {
                            let n = self.b.text(prop);
                            (n.clone(), n)
                        }
                        "pair_pattern" => {
                            let key = prop
                                .child_by_field_name("key")
                                .and_then(|k| self.member_name(k));
                            let value = prop
                                .child_by_field_name("value")
                                .filter(|v| v.kind() == "identifier");
                            match (key, value) {
                                (Some(k), Some(v)) => (k, self.b.text(v)),
                                _ => {
                                    self.bind_pattern(prop, cx);
                                    continue;
                                }
                            }
                        }
                        _ => {
                            self.bind_pattern(prop, cx);
                            continue;
                        }
                    };
                    self.bind_name(cx, &local);
                    self.b.push_import(Import {
                        module: module.to_string(),
                        level: 0,
                        kind: ImportKind::Named,
                        imported: Some(imported),
                        local: Some(local),
                        scope,
                        type_only: false,
                        span: span(prop),
                    });
                }
            }
            _ => {
                self.bind_pattern(name_node, cx);
                self.b.push_import(Import {
                    module: module.to_string(),
                    level: 0,
                    kind: ImportKind::SideEffect,
                    imported: None,
                    local: None,
                    scope,
                    type_only: false,
                    span: span(decl),
                });
            }
        }
    }

    // ---- helpers ----------------------------------------------------------

    /// Bind a name in the current scope: a function local, or a file-wide
    /// anonymous local. Module-level names are symbols/imports instead.
    fn bind_name(&mut self, cx: Cx, name: &str) {
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, name);
        } else if cx.anon {
            self.b.add_anonymous_local(name);
        }
    }

    fn bind_pattern(&mut self, pattern: Node, cx: Cx) {
        let mut names = Vec::new();
        pattern_names(pattern, &mut names, 0);
        for n in names {
            let name = self.b.text(n);
            if cx.fn_scope.is_some() {
                self.b.add_local(cx.fn_scope, &name);
            } else {
                // Block-scoped or anonymous-function binding outside any symbol.
                self.b.add_anonymous_local(&name);
            }
        }
    }

    /// `a.b.c`, `this.a`, `super.a` as segments; `None` for anything else.
    fn dotted(&self, expr: Node) -> Option<Vec<String>> {
        let mut segments = Vec::new();
        let mut current = expr;
        loop {
            match current.kind() {
                "identifier" | "this" | "super" | "property_identifier" | "type_identifier" => {
                    segments.push(self.b.text(current));
                    break;
                }
                "member_expression" => {
                    let prop = current.child_by_field_name("property")?;
                    if !matches!(
                        prop.kind(),
                        "property_identifier" | "private_property_identifier"
                    ) {
                        return None;
                    }
                    segments.push(self.b.text(prop));
                    current = current.child_by_field_name("object")?;
                }
                "parenthesized_expression" | "non_null_expression" => {
                    current = current.named_child(0)?;
                }
                _ => return None,
            }
            if segments.len() > 64 {
                return None;
            }
        }
        segments.reverse();
        Some(segments)
    }

    /// Segments of a type reference such as `Foo`, `ns.Foo` or `Foo<T>`.
    fn type_name(&self, ty: Node) -> Option<Vec<String>> {
        match ty.kind() {
            "type_identifier" | "identifier" => Some(vec![self.b.text(ty)]),
            "nested_type_identifier" | "nested_identifier" | "member_expression" => {
                let text = self.b.text(ty);
                let segs: Vec<String> = text.split('.').map(|s| s.trim().to_string()).collect();
                (!segs.iter().any(|s| s.is_empty())).then_some(segs)
            }
            "generic_type" => self.type_name(
                ty.child_by_field_name("name")
                    .or_else(|| ty.named_child(0))?,
            ),
            _ => None,
        }
    }

    fn target(&self, expr: Node) -> Target {
        let mut expr = expr;
        while matches!(
            expr.kind(),
            "parenthesized_expression" | "non_null_expression"
        ) {
            match expr.named_child(0) {
                Some(inner) => expr = inner,
                None => break,
            }
        }
        if let Some(path) = self.dotted(expr) {
            return Target { path, member: None };
        }
        if expr.kind() == "member_expression" {
            let member = expr
                .child_by_field_name("property")
                .filter(|p| {
                    matches!(
                        p.kind(),
                        "property_identifier" | "private_property_identifier"
                    )
                })
                .map(|p| self.b.text(p));
            return Target {
                path: Vec::new(),
                member,
            };
        }
        Target::default()
    }

    /// A statically known member name, or `None` for computed names.
    fn member_name(&self, name: Node) -> Option<String> {
        match name.kind() {
            "property_identifier" | "private_property_identifier" | "identifier" | "number" => {
                Some(self.b.text(name))
            }
            "string" => {
                let v = self.string_value(name);
                (!v.is_empty()).then_some(v)
            }
            _ => None,
        }
    }

    fn name_or_string(&self, node: Node) -> String {
        if node.kind() == "string" {
            self.string_value(node)
        } else {
            self.b.text(node)
        }
    }

    fn string_value(&self, string: Node) -> String {
        let mut out = String::new();
        for part in named_children(string) {
            if part.kind() == "string_fragment" {
                out.push_str(&self.b.text(part));
            }
        }
        out
    }

    fn signature(&self, node: Node) -> String {
        let mut out = String::new();
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            out.push_str(&self.b.text_truncated(tp));
        }
        if let Some(p) = node.child_by_field_name("parameters") {
            out.push_str(&self.b.text_truncated(p));
        } else if let Some(p) = node.child_by_field_name("parameter") {
            out.push('(');
            out.push_str(&self.b.text_truncated(p));
            out.push(')');
        } else {
            out.push_str("()");
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            out.push_str(&self.b.text_truncated(ret));
        }
        truncate(&out, 300)
    }

    /// First line of the JSDoc block immediately preceding a declaration.
    fn jsdoc(&self, node: Node) -> Option<String> {
        let mut anchor = node;
        // The comment precedes the outermost statement wrapper.
        while let Some(parent) = anchor.parent() {
            if matches!(parent.kind(), "export_statement" | "ambient_declaration")
                || (matches!(
                    parent.kind(),
                    "lexical_declaration" | "variable_declaration"
                ))
            {
                anchor = parent;
            } else {
                break;
            }
        }
        let mut prev = anchor.prev_sibling()?;
        while prev.kind() == "decorator" {
            prev = prev.prev_sibling()?;
        }
        if prev.kind() != "comment" || prev.end_position().row + 1 < anchor.start_position().row {
            return None;
        }
        let text = self.b.text(prev);
        let body = text.strip_prefix("/**")?.strip_suffix("*/")?;
        let line = body
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .find(|l| !l.is_empty() && !l.starts_with('@'))?;
        Some(truncate(line, 200))
    }
}

fn is_function_value(node: Node) -> bool {
    matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "generator_function"
    )
}

/// Collect the identifier nodes bound by a declaration or parameter pattern.
fn pattern_names<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>, depth: u32) {
    if depth > 64 {
        return;
    }
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => out.push(node),
        "required_parameter" | "optional_parameter" => {
            if let Some(p) = node.child_by_field_name("pattern") {
                pattern_names(p, out, depth + 1);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(left) = node.child_by_field_name("left") {
                pattern_names(left, out, depth + 1);
            }
        }
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                pattern_names(value, out, depth + 1);
            }
        }
        "formal_parameters" | "object_pattern" | "array_pattern" | "rest_pattern" => {
            for child in named_children(node) {
                pattern_names(child, out, depth + 1);
            }
        }
        _ => {}
    }
}
