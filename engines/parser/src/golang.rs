//! Go extraction adapter (tree-sitter-go).
//!
//! Conventions in the IR produced here:
//!
//! * Methods are module-level symbols (Go declares them outside their type,
//!   possibly in another file of the package) named after the method, with
//!   `qualified_name` `Type.Method` and `attrs.receiver_type`.
//! * Struct fields and interface methods are children of their type.
//!   Embedded fields and embedded interfaces are recorded as `bases`.
//! * Struct fields and package-level variables carry `attrs.type_path`
//!   (segments of the declared named type) when the type is a named type.
//! * Parameters, receivers and typed local variables are recorded in the
//!   function's `attrs.local_types`.
//! * Imports without an alias have `local: None`: the binding name is the
//!   imported package's `package` clause, which the resolver determines.
//! * The file's package clause is stored in `attrs.package`.

use crate::common::{
    children, doc_comment, named_children, span, Builder, Cx, Extracted, Limits, LocalTypes,
    Target, MAX_CHAIN, MAX_DEPTH,
};
use tree_sitter::Node;
use usie_ir::{Import, ImportKind, ReferenceKind, SymbolKind};

pub(crate) fn extract(root: Node, src: &[u8], limits: Limits) -> Extracted {
    let mut go = Go {
        b: Builder::new(src, limits),
        types: LocalTypes::default(),
    };
    go.walk(root, Cx::default(), 0);
    let Go { mut b, types } = go;
    types.apply(&mut b);
    b.finish()
}

struct Go<'a> {
    b: Builder<'a>,
    types: LocalTypes,
}

fn exported(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

impl<'a> Go<'a> {
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
            "package_clause" => {
                if let Some(name) = named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "package_identifier")
                {
                    let name = self.b.text(name);
                    self.b.attrs.insert("package".into(), name.into());
                }
            }
            "import_declaration" => self.imports(node, cx),
            "function_declaration" => self.function(node, cx, depth),
            "method_declaration" => self.method(node, cx, depth),
            "type_declaration" => {
                for spec in named_children(node) {
                    if matches!(spec.kind(), "type_spec" | "type_alias") {
                        self.type_spec(spec, cx);
                    }
                }
            }
            "var_declaration" | "const_declaration" => {
                let constant = node.kind() == "const_declaration";
                for spec in named_children(node) {
                    match spec.kind() {
                        "var_spec" | "const_spec" => self.value_spec(spec, cx, depth + 1, constant),
                        // `var ( ... )` groups specs in a var_spec_list.
                        "var_spec_list" => {
                            for inner in named_children(spec) {
                                if inner.kind() == "var_spec" {
                                    self.value_spec(inner, cx, depth + 2, constant);
                                }
                            }
                        }
                        _ => self.walk(spec, cx, depth + 1),
                    }
                }
            }
            "short_var_declaration" => {
                let left = node.child_by_field_name("left");
                let right = node.child_by_field_name("right");
                if let Some(left) = left {
                    let names: Vec<Node> = named_children(left)
                        .into_iter()
                        .filter(|n| n.kind() == "identifier")
                        .collect();
                    let values: Vec<Node> = right.map(named_children).unwrap_or_default();
                    self.bind_values(cx, &names, &values);
                }
                if let Some(right) = right {
                    self.walk(right, cx, depth + 1);
                }
            }
            "range_clause" | "receive_statement" => {
                // `for k, v := range xs` / `case v := <-ch`: bound, type unknown.
                if let Some(left) = node.child_by_field_name("left") {
                    if children(node).iter().any(|c| c.kind() == ":=") {
                        for n in named_children(left) {
                            if n.kind() == "identifier" {
                                self.bind(cx, n, None);
                            }
                        }
                    }
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.walk(right, cx, depth + 1);
                }
            }
            "type_switch_statement" => {
                if let Some(alias) = node.child_by_field_name("alias") {
                    for n in named_children(alias) {
                        if n.kind() == "identifier" {
                            self.bind(cx, n, None);
                        }
                    }
                }
                self.walk_children(node, cx, depth);
            }
            "func_literal" => {
                // Parameters of a closure are attributed to the enclosing
                // function: over-approximating shadowing can only make a
                // reference unresolved, never mis-resolved.
                if let Some(params) = node.child_by_field_name("parameters") {
                    self.parameters(params, cx, false);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(body, cx, depth + 1);
                }
            }
            "call_expression" => {
                if let Some(callee) = node.child_by_field_name("function") {
                    let target = self.target(callee);
                    self.b
                        .push_reference(ReferenceKind::Call, cx, target, callee, callee);
                }
                self.walk_children(node, cx, depth);
            }
            "type_conversion_expression" => {
                // `T(x)` and `F[int](x)` parse alike; record a call of the name.
                if let Some(ty) = node.child_by_field_name("type") {
                    if let Some(path) = self.type_path(ty) {
                        let target = Target {
                            path,
                            member: None,
                            ..Default::default()
                        };
                        self.b
                            .push_reference(ReferenceKind::Call, cx, target, ty, ty);
                    }
                }
                if let Some(operand) = node.child_by_field_name("operand") {
                    self.walk(operand, cx, depth + 1);
                }
            }
            "composite_literal" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    if let Some(path) = self.type_path(ty) {
                        let target = Target {
                            path,
                            member: None,
                            ..Default::default()
                        };
                        self.b
                            .push_reference(ReferenceKind::New, cx, target, ty, ty);
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(body, cx, depth + 1);
                }
            }
            // Keys of keyed struct literals are field names, not references.
            "keyed_element" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.walk(value, cx, depth + 1);
                } else {
                    self.walk_children(node, cx, depth);
                }
            }
            _ => self.walk_children(node, cx, depth),
        }
    }

    /// Bind `names := values`: composite literals give the type; a single
    /// call gives result types by position.
    fn bind_values(&mut self, cx: Cx, names: &[Node], values: &[Node]) {
        let call = match values {
            [v] if v.kind() == "call_expression" => v
                .child_by_field_name("function")
                .map(|f| self.target(f))
                .filter(|t| !t.path.is_empty())
                .map(|t| t.path),
            _ => None,
        };
        for (i, name) in names.iter().enumerate() {
            if names.len() == values.len() {
                if let Some(ty) = self.literal_type(values[i]) {
                    self.bind(cx, *name, Some(ty));
                    continue;
                }
                if values[i].kind() == "call_expression" {
                    let callee = values[i]
                        .child_by_field_name("function")
                        .map(|f| self.target(f).path)
                        .unwrap_or_default();
                    self.bind_call(cx, *name, callee, 0);
                    continue;
                }
            } else if let Some(callee) = &call {
                self.bind_call(cx, *name, callee.clone(), i);
                continue;
            }
            self.bind(cx, *name, None);
        }
    }

    fn bind_call(&mut self, cx: Cx, name: Node, callee: Vec<String>, index: usize) {
        let name = self.b.text(name);
        if name == "_" {
            return;
        }
        match cx.fn_scope {
            Some(_) => {
                self.b.add_local(cx.fn_scope, &name);
                self.types.bind_call(cx.fn_scope, &name, callee, index);
            }
            None => self.b.add_anonymous_local(&name),
        }
    }

    fn bind(&mut self, cx: Cx, name: Node, ty: Option<Vec<String>>) {
        let name = self.b.text(name);
        if name == "_" {
            return;
        }
        match cx.fn_scope {
            Some(_) => {
                self.b.add_local(cx.fn_scope, &name);
                self.types.bind(cx.fn_scope, &name, ty);
            }
            None => self.b.add_anonymous_local(&name),
        }
    }

    fn imports(&mut self, node: Node, cx: Cx) {
        let mut specs = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "import_spec" => specs.push(child),
                "import_spec_list" => specs.extend(
                    named_children(child)
                        .into_iter()
                        .filter(|c| c.kind() == "import_spec"),
                ),
                _ => {}
            }
        }
        for spec in specs {
            let Some(path_node) = spec.child_by_field_name("path") else {
                continue;
            };
            let module = self.string_content(path_node);
            if module.is_empty() {
                continue;
            }
            let (kind, local) = match spec.child_by_field_name("name") {
                Some(n) if n.kind() == "dot" => (ImportKind::Wildcard, None),
                Some(n) if n.kind() == "blank_identifier" => (ImportKind::SideEffect, None),
                Some(n) => (ImportKind::Namespace, Some(self.b.text(n))),
                None => (ImportKind::Namespace, None),
            };
            self.b.push_import(Import {
                module,
                level: 0,
                kind,
                imported: None,
                local,
                scope: cx.fn_scope,
                type_only: false,
                span: span(spec),
            });
        }
    }

    fn function(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = self.b.text(name_node);
        let is_exported = exported(&name);
        let Some(id) = self
            .b
            .define(SymbolKind::Function, name, cx.scope, node, name_node)
        else {
            return;
        };
        self.callable(id, node, depth, is_exported);
    }

    fn method(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = self.b.text(name_node);
        let receiver = node.child_by_field_name("receiver").and_then(|r| {
            named_children(r)
                .into_iter()
                .find(|c| c.kind() == "parameter_declaration")
        });
        let receiver_type = receiver
            .and_then(|r| r.child_by_field_name("type"))
            .and_then(|t| self.type_path(t))
            .and_then(|p| p.last().cloned());
        let receiver_name = receiver
            .and_then(|r| r.child_by_field_name("name"))
            .map(|n| self.b.text(n));
        let is_exported = exported(&name);
        let Some(id) = self
            .b
            .define(SymbolKind::Method, name.clone(), cx.scope, node, name_node)
        else {
            return;
        };
        if let Some(ty) = &receiver_type {
            let sym = self.b.sym(id);
            sym.qualified_name = format!("{ty}.{name}");
            sym.attrs.insert("receiver_type".into(), ty.clone().into());
        }
        if let (Some(rn), Some(ty)) = (&receiver_name, &receiver_type) {
            if rn != "_" {
                self.b.add_local(Some(id), rn);
                self.types.bind(Some(id), rn, Some(vec![ty.clone()]));
                self.b
                    .sym(id)
                    .attrs
                    .insert("receiver_name".into(), rn.clone().into());
            }
        }
        self.callable(id, node, depth, is_exported);
    }

    /// Signature, doc, parameters and body of a function or method.
    fn callable(&mut self, id: u32, node: Node, depth: u32, is_exported: bool) {
        let mut signature = String::new();
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            signature.push_str(&self.b.text_truncated(tp));
        }
        let params = node.child_by_field_name("parameters");
        if let Some(p) = params {
            signature.push_str(&self.b.text_truncated(p));
        }
        if let Some(result) = node.child_by_field_name("result") {
            signature.push(' ');
            signature.push_str(&self.b.text_truncated(result));
        }
        let doc = doc_comment(&self.b, node);
        let results = node
            .child_by_field_name("result")
            .map(|r| self.result_types(r));
        {
            let sym = self.b.sym(id);
            sym.signature = Some(signature);
            sym.doc = doc;
            sym.exported = is_exported;
            if let Some(results) = results {
                if results.iter().any(|r| !r.is_null()) {
                    sym.attrs.insert("result_types".into(), results.into());
                }
            }
        }
        let inner = Cx {
            scope: Some(id),
            fn_scope: Some(id),
            exported: false,
            anon: false,
        };
        if let Some(params) = params {
            self.parameters(params, inner, true);
        }
        if let Some(result) = node.child_by_field_name("result") {
            // Named results are locals too.
            if result.kind() == "parameter_list" {
                self.parameters(result, inner, true);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, inner, depth + 1);
        }
    }

    /// Declared result types by position (`null` where the type is not a
    /// named type), as `attrs.result_types`.
    fn result_types(&self, result: Node) -> Vec<serde_json::Value> {
        if result.kind() != "parameter_list" {
            return vec![self
                .type_path(result)
                .map_or(serde_json::Value::Null, Into::into)];
        }
        let mut out = Vec::new();
        for param in named_children(result) {
            if param.kind() != "parameter_declaration" {
                continue;
            }
            let ty: serde_json::Value = param
                .child_by_field_name("type")
                .and_then(|t| self.type_path(t))
                .map_or(serde_json::Value::Null, Into::into);
            let mut cursor = param.walk();
            let n = param
                .children_by_field_name("name", &mut cursor)
                .filter(|n| n.is_named())
                .count();
            for _ in 0..n.max(1) {
                out.push(ty.clone());
            }
        }
        out
    }

    fn parameters(&mut self, list: Node, cx: Cx, typed: bool) {
        for param in named_children(list) {
            if !matches!(
                param.kind(),
                "parameter_declaration" | "variadic_parameter_declaration"
            ) {
                continue;
            }
            let ty = if typed && param.kind() == "parameter_declaration" {
                param
                    .child_by_field_name("type")
                    .and_then(|t| self.type_path(t))
            } else {
                None
            };
            let mut cursor = param.walk();
            let names: Vec<Node> = param
                .children_by_field_name("name", &mut cursor)
                .filter(|n| n.is_named())
                .collect();
            for n in names {
                self.bind(cx, n, ty.clone());
            }
        }
    }

    fn type_spec(&mut self, spec: Node, cx: Cx) {
        let Some(name_node) = spec.child_by_field_name("name") else {
            return;
        };
        let name = self.b.text(name_node);
        let ty = spec.child_by_field_name("type");
        let (kind, type_kind) = match ty.map(|t| t.kind()) {
            Some("struct_type") => (SymbolKind::Class, "struct"),
            Some("interface_type") => (SymbolKind::Interface, "interface"),
            _ if spec.kind() == "type_alias" => (SymbolKind::Type, "alias"),
            _ => (SymbolKind::Type, "defined"),
        };
        let is_exported = exported(&name);
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, &name);
        }
        // Group-declared types (`type ( A int; B string )`) document the spec.
        let doc_anchor = match spec.parent() {
            Some(p) if p.kind() == "type_declaration" && p.named_child_count() == 1 => p,
            _ => spec,
        };
        let doc = doc_comment(&self.b, doc_anchor);
        let decl = if doc_anchor.kind() == "type_declaration" {
            doc_anchor
        } else {
            spec
        };
        let Some(id) = self.b.define(kind, name, cx.scope, decl, name_node) else {
            return;
        };
        {
            let sym = self.b.sym(id);
            sym.exported = is_exported;
            sym.doc = doc;
            sym.attrs.insert("type_kind".into(), type_kind.into());
        }
        let Some(ty) = ty else { return };
        match ty.kind() {
            "struct_type" => self.struct_fields(id, ty),
            "interface_type" => self.interface_elems(id, ty),
            _ => {
                if let Some(path) = self.type_path(ty) {
                    // `type A = pkg.B` / `type A B`: what the name stands for.
                    self.b
                        .sym(id)
                        .attrs
                        .insert("underlying".into(), path.into());
                }
            }
        }
    }

    fn struct_fields(&mut self, owner: u32, ty: Node) {
        let Some(list) = named_children(ty)
            .into_iter()
            .find(|c| c.kind() == "field_declaration_list")
        else {
            return;
        };
        for field in named_children(list) {
            if field.kind() != "field_declaration" {
                continue;
            }
            let field_type = field.child_by_field_name("type");
            let path = field_type.and_then(|t| self.type_path(t));
            let mut cursor = field.walk();
            let names: Vec<Node> = field
                .children_by_field_name("name", &mut cursor)
                .filter(|n| n.is_named())
                .collect();
            if names.is_empty() {
                // Embedded field: its methods and fields are promoted.
                if let (Some(t), Some(segments)) = (field_type, path.clone()) {
                    let base = self.b.name_path(t, segments);
                    self.b.sym(owner).bases.push(base);
                }
                continue;
            }
            for n in names {
                let name = self.b.text(n);
                let is_exported = exported(&name);
                if let Some(fid) = self
                    .b
                    .define(SymbolKind::Variable, name, Some(owner), field, n)
                {
                    let sig = field_type.map(|t| self.b.text_truncated(t));
                    let sym = self.b.sym(fid);
                    sym.exported = is_exported;
                    sym.signature = sig;
                    sym.attrs.insert("member".into(), true.into());
                    if let Some(p) = &path {
                        sym.attrs.insert("type_path".into(), p.clone().into());
                    }
                }
            }
        }
    }

    fn interface_elems(&mut self, owner: u32, ty: Node) {
        for elem in named_children(ty) {
            match elem.kind() {
                "method_elem" => {
                    let Some(name_node) = elem.child_by_field_name("name") else {
                        continue;
                    };
                    let name = self.b.text(name_node);
                    let is_exported = exported(&name);
                    let mut signature = elem
                        .child_by_field_name("parameters")
                        .map(|p| self.b.text_truncated(p))
                        .unwrap_or_default();
                    if let Some(r) = elem.child_by_field_name("result") {
                        signature.push(' ');
                        signature.push_str(&self.b.text_truncated(r));
                    }
                    let doc = doc_comment(&self.b, elem);
                    if let Some(mid) =
                        self.b
                            .define(SymbolKind::Method, name, Some(owner), elem, name_node)
                    {
                        let sym = self.b.sym(mid);
                        sym.exported = is_exported;
                        sym.signature = Some(signature);
                        sym.doc = doc;
                        sym.attrs.insert("abstract".into(), true.into());
                    }
                }
                "type_elem" => {
                    // Embedded interfaces; unions and `~T` constraints are not.
                    let inner = named_children(elem);
                    if inner.len() == 1 {
                        if let Some(segments) = self.type_path(inner[0]) {
                            let base = self.b.name_path(inner[0], segments);
                            self.b.sym(owner).bases.push(base);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn value_spec(&mut self, spec: Node, cx: Cx, depth: u32, constant: bool) {
        let declared = spec
            .child_by_field_name("type")
            .and_then(|t| self.type_path(t));
        let values: Vec<Node> = spec
            .child_by_field_name("value")
            .map(named_children)
            .unwrap_or_default();
        let mut cursor = spec.walk();
        let names: Vec<Node> = spec
            .children_by_field_name("name", &mut cursor)
            .filter(|n| n.is_named())
            .collect();
        if cx.fn_scope.is_some() {
            if declared.is_some() {
                for n in &names {
                    self.bind(cx, *n, declared.clone());
                }
            } else {
                self.bind_values(cx, &names, &values);
            }
            if let Some(v) = spec.child_by_field_name("value") {
                self.walk(v, cx, depth + 1);
            }
            return;
        }
        for (i, n) in names.iter().enumerate() {
            let ty = declared.clone().or_else(|| {
                if values.len() == names.len() {
                    self.literal_type(values[i])
                } else {
                    None
                }
            });
            let name = self.b.text(*n);
            if name == "_" {
                continue;
            }
            let is_exported = exported(&name);
            let doc = doc_comment(&self.b, spec.parent().unwrap_or(spec));
            if let Some(id) = self
                .b
                .define(SymbolKind::Variable, name, cx.scope, spec, *n)
            {
                let sym = self.b.sym(id);
                sym.exported = is_exported;
                sym.doc = doc;
                if constant {
                    sym.attrs.insert("const".into(), true.into());
                }
                if let Some(t) = ty {
                    sym.attrs.insert("type_path".into(), t.into());
                }
            }
        }
        if let Some(v) = spec.child_by_field_name("value") {
            self.walk(v, cx, depth + 1);
        }
    }

    /// The named type of `T{...}` or `&T{...}`.
    fn literal_type(&self, value: Node) -> Option<Vec<String>> {
        let lit = match value.kind() {
            "composite_literal" => value,
            "unary_expression" => {
                let operand = value.child_by_field_name("operand")?;
                if operand.kind() != "composite_literal" {
                    return None;
                }
                operand
            }
            _ => return None,
        };
        self.type_path(lit.child_by_field_name("type")?)
    }

    /// Segments of a named type (`T`, `pkg.T`, `*T`, `T[int]`), or `None` for
    /// slices, maps, channels, function and literal types.
    fn type_path(&self, ty: Node) -> Option<Vec<String>> {
        match ty.kind() {
            "type_identifier" | "identifier" => Some(vec![self.b.text(ty)]),
            "qualified_type" => Some(vec![
                self.b.text(ty.child_by_field_name("package")?),
                self.b.text(ty.child_by_field_name("name")?),
            ]),
            "pointer_type" | "parenthesized_type" => self.type_path(ty.named_child(0)?),
            "generic_type" => self.type_path(ty.child_by_field_name("type")?),
            _ => None,
        }
    }

    fn string_content(&self, lit: Node) -> String {
        let text = self.b.text(lit);
        text.trim_matches(|c| c == '"' || c == '`').to_string()
    }

    /// `a.b.c` as segments, or `None` if not a plain selector chain.
    fn dotted(&self, expr: Node) -> Option<Vec<String>> {
        let mut segments = Vec::new();
        let mut current = expr;
        loop {
            match current.kind() {
                "identifier" | "type_identifier" | "package_identifier" => {
                    segments.push(self.b.text(current));
                    break;
                }
                "selector_expression" => {
                    segments.push(self.b.text(current.child_by_field_name("field")?));
                    current = current.child_by_field_name("operand")?;
                }
                "qualified_type" => {
                    segments.push(self.b.text(current.child_by_field_name("name")?));
                    current = current.child_by_field_name("package")?;
                }
                "parenthesized_expression" => current = current.named_child(0)?,
                _ => return None,
            }
        }
        segments.reverse();
        Some(segments)
    }

    fn target(&self, callee: Node) -> Target {
        // Explicit instantiation `F[int](x)` calls `F`.
        let callee = match callee.kind() {
            "index_expression" | "generic_type" => callee
                .child_by_field_name("operand")
                .or_else(|| callee.child_by_field_name("type"))
                .unwrap_or(callee),
            "parenthesized_expression" => callee.named_child(0).unwrap_or(callee),
            _ => callee,
        };
        if let Some(path) = self.dotted(callee) {
            return Target {
                path,
                member: None,
                ..Default::default()
            };
        }
        if callee.kind() == "selector_expression" {
            let member = callee.child_by_field_name("field").map(|f| self.b.text(f));
            let chain = match (callee.child_by_field_name("operand"), &member) {
                (Some(operand), Some(m)) => self
                    .chain(operand, 0)
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

    /// Steps that produce the value of `expr` (see `Reference::chain`), or
    /// `None` when they are not statically describable.
    fn chain(&self, expr: Node, depth: u32) -> Option<Vec<String>> {
        if depth > MAX_CHAIN as u32 {
            return None;
        }
        match expr.kind() {
            "identifier" => Some(vec![self.b.text(expr)]),
            "selector_expression" => {
                let mut c = self.chain(expr.child_by_field_name("operand")?, depth + 1)?;
                c.push(self.b.text(expr.child_by_field_name("field")?));
                Some(c)
            }
            "call_expression" => {
                let mut function = expr.child_by_field_name("function")?;
                if function.kind() == "index_expression" {
                    function = function.child_by_field_name("operand")?;
                }
                let mut c = self.chain(function, depth + 1)?;
                c.push("()".into());
                Some(c)
            }
            "type_conversion_expression" => {
                let ty = self.type_path(expr.child_by_field_name("type")?)?;
                Some(vec![format!("type:{}", ty.join("."))])
            }
            "composite_literal" => {
                let ty = self.type_path(expr.child_by_field_name("type")?)?;
                Some(vec![format!("type:{}", ty.join("."))])
            }
            "unary_expression" => {
                // `&T{}` has the members of `T`; other operators do not.
                let op = expr.child_by_field_name("operator")?;
                if self.b.text(op) != "&" {
                    return None;
                }
                self.chain(expr.child_by_field_name("operand")?, depth + 1)
            }
            "parenthesized_expression" => self.chain(expr.named_child(0)?, depth + 1),
            _ => None,
        }
    }
}
