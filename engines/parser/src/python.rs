//! Python extraction adapter (tree-sitter-python).

use crate::common::{children, named_children, span, Builder, Cx, Extracted, Limits, Target, MAX_DEPTH};
use tree_sitter::Node;
use usie_ir::{Export, ExportKind, Import, ImportKind, ReferenceKind, SymbolKind};

pub(crate) fn extract(root: Node, src: &[u8], limits: Limits) -> Extracted {
    let mut py = Py {
        b: Builder::new(src, limits),
    };
    py.walk(root, Cx::default(), 0);
    py.b.finish()
}

struct Py<'a> {
    b: Builder<'a>,
}

impl<'a> Py<'a> {
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
            "decorated_definition" => self.decorated(node, cx, depth),
            "function_definition" => self.function(node, cx, depth, Vec::new()),
            "class_definition" => self.class(node, cx, depth, Vec::new()),
            "import_statement" => self.import(node, cx),
            "import_from_statement" => self.import_from(node, cx),
            "future_import_statement" => {}
            "call" => {
                if let Some(callee) = node.child_by_field_name("function") {
                    let target = self.target(callee);
                    self.b
                        .push_reference(ReferenceKind::Call, cx, target, callee, callee);
                }
                self.walk_children(node, cx, depth);
            }
            "assignment" | "augmented_assignment" => {
                let left = node.child_by_field_name("left");
                let right = node.child_by_field_name("right");
                if let Some(left) = left {
                    if node.kind() == "assignment" {
                        self.bind_targets(left, cx, node);
                    }
                    if cx.scope.is_none() && left.kind() == "identifier" && self.b.text(left) == "__all__" {
                        if let Some(right) = right {
                            self.dunder_all(right);
                        }
                    }
                }
                self.walk_children(node, cx, depth);
            }
            "for_statement" | "for_in_clause" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.bind_locals(left, cx);
                }
                self.walk_children(node, cx, depth);
            }
            "as_pattern" => {
                if let Some(alias) = node.child_by_field_name("alias") {
                    self.bind_locals(alias, cx);
                }
                self.walk_children(node, cx, depth);
            }
            "named_expression" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.bind_locals(name, cx);
                }
                self.walk_children(node, cx, depth);
            }
            "lambda" => {
                // Lambda parameters are attributed to the enclosing function
                // scope: over-approximating shadowing can only make a
                // reference unresolved, never mis-resolved.
                if let Some(params) = node.child_by_field_name("parameters") {
                    self.bind_locals(params, cx);
                }
                self.walk_children(node, cx, depth);
            }
            "global_statement" | "nonlocal_statement" => {
                for child in named_children(node) {
                    if child.kind() == "identifier" {
                        let name = self.b.text(child);
                        self.b.bind_outer(cx.fn_scope, &name);
                    }
                }
            }
            _ => self.walk_children(node, cx, depth),
        }
    }

    fn decorated(&mut self, node: Node, cx: Cx, depth: u32) {
        let mut decorators = Vec::new();
        for child in children(node) {
            if child.kind() != "decorator" {
                continue;
            }
            if let Some(expr) = child.named_child(0) {
                decorators.push(self.b.text_truncated(expr));
                match expr.kind() {
                    "identifier" | "attribute" => {
                        let target = self.target(expr);
                        self.b
                            .push_reference(ReferenceKind::Decorator, cx, target, expr, expr);
                    }
                    // `@decorator(args)` is recorded as an ordinary call.
                    _ => self.walk(expr, cx, depth + 1),
                }
            }
        }
        if let Some(def) = node.child_by_field_name("definition") {
            match def.kind() {
                "function_definition" => self.function(def, cx, depth + 1, decorators),
                "class_definition" => self.class(def, cx, depth + 1, decorators),
                _ => self.walk(def, cx, depth + 1),
            }
        }
    }

    fn function(&mut self, node: Node, cx: Cx, depth: u32, decorators: Vec<String>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = self.b.text(name_node);
        let in_class = cx
            .scope
            .map(|s| self.b.symbols[s as usize].kind == SymbolKind::Class)
            .unwrap_or(false);
        let kind = if in_class {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        self.b.add_local(cx.fn_scope, &name);
        let Some(id) = self.b.define(kind, name, cx.scope, node, name_node) else {
            return;
        };

        let params = node.child_by_field_name("parameters");
        let mut signature = params.map(|p| self.b.text_truncated(p)).unwrap_or_default();
        if let Some(ret) = node.child_by_field_name("return_type") {
            signature.push_str(" -> ");
            signature.push_str(&self.b.text_truncated(ret));
        }
        let is_async = children(node).iter().any(|c| c.kind() == "async");
        let doc = node
            .child_by_field_name("body")
            .and_then(|b| self.docstring(b));
        {
            let sym = self.b.sym(id);
            sym.signature = Some(signature);
            sym.doc = doc;
            if is_async {
                sym.attrs.insert("async".into(), true.into());
            }
            apply_decorators(sym, decorators);
        }

        let inner = Cx {
            scope: Some(id),
            fn_scope: Some(id),
            exported: false,
            anon: false,
        };
        if let Some(params) = params {
            self.bind_locals(params, inner);
            // Defaults and annotations are evaluated in the enclosing scope.
            self.walk(params, cx, depth + 1);
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.walk(ret, cx, depth + 1);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, inner, depth + 1);
        }
    }

    fn class(&mut self, node: Node, cx: Cx, depth: u32, decorators: Vec<String>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = self.b.text(name_node);
        self.b.add_local(cx.fn_scope, &name);
        let Some(id) = self.b.define(SymbolKind::Class, name, cx.scope, node, name_node) else {
            return;
        };

        if let Some(supers) = node.child_by_field_name("superclasses") {
            for arg in named_children(supers) {
                match arg.kind() {
                    "keyword_argument" => {
                        let key = arg
                            .child_by_field_name("name")
                            .map(|n| self.b.text(n))
                            .unwrap_or_default();
                        if key == "metaclass" {
                            if let Some(value) = arg.child_by_field_name("value") {
                                let text = self.b.text_truncated(value);
                                self.b.sym(id).attrs.insert("metaclass".into(), text.into());
                            }
                        }
                    }
                    "comment" => {}
                    _ => {
                        // `Base[T]` names the class `Base`.
                        let named = match arg.kind() {
                            "subscript" => arg.child_by_field_name("value").unwrap_or(arg),
                            _ => arg,
                        };
                        let segments = self.dotted(named).unwrap_or_default();
                        let base = self.b.name_path(arg, segments);
                        self.b.sym(id).bases.push(base);
                    }
                }
            }
            self.walk(supers, cx, depth + 1);
        }
        let doc = node
            .child_by_field_name("body")
            .and_then(|b| self.docstring(b));
        {
            let sym = self.b.sym(id);
            sym.doc = doc;
            apply_decorators(sym, decorators);
        }
        if let Some(body) = node.child_by_field_name("body") {
            // A class body is not a function scope: names bound there become
            // members, and are not visible as bare names inside methods.
            let inner = Cx {
                scope: Some(id),
                fn_scope: None,
                exported: false,
                anon: false,
            };
            self.walk(body, inner, depth + 1);
        }
    }

    fn import(&mut self, node: Node, cx: Cx) {
        let mut cursor = node.walk();
        let names: Vec<Node> = node.children_by_field_name("name", &mut cursor).collect();
        for item in names {
            let (module, local) = match item.kind() {
                "aliased_import" => {
                    let module = item
                        .child_by_field_name("name")
                        .map(|n| self.b.text(n))
                        .unwrap_or_default();
                    let alias = item.child_by_field_name("alias").map(|n| self.b.text(n));
                    (module, alias)
                }
                _ => {
                    let module = self.b.text(item);
                    (module.clone(), Some(module))
                }
            };
            if module.is_empty() {
                continue;
            }
            if let Some(local) = &local {
                let first = local.split('.').next().unwrap_or("");
                self.b.add_local(cx.fn_scope, first);
            }
            self.b.push_import(Import {
                module,
                level: 0,
                kind: ImportKind::Namespace,
                imported: None,
                local,
                scope: import_scope(cx),
                type_only: false,
                span: span(node),
            });
        }
    }

    fn import_from(&mut self, node: Node, cx: Cx) {
        let (module, level) = match node.child_by_field_name("module_name") {
            Some(m) if m.kind() == "relative_import" => {
                let mut level = 0u32;
                let mut module = String::new();
                for child in children(m) {
                    match child.kind() {
                        "import_prefix" => {
                            level = self.b.text(child).chars().filter(|c| *c == '.').count() as u32
                        }
                        "dotted_name" => module = self.b.text(child),
                        _ => {}
                    }
                }
                (module, level)
            }
            Some(m) => (self.b.text(m), 0),
            None => (String::new(), 0),
        };
        if module.is_empty() && level == 0 {
            return;
        }

        if children(node).iter().any(|c| c.kind() == "wildcard_import") {
            self.b.push_import(Import {
                module,
                level,
                kind: ImportKind::Wildcard,
                imported: None,
                local: None,
                scope: import_scope(cx),
                type_only: false,
                span: span(node),
            });
            return;
        }

        let mut cursor = node.walk();
        let names: Vec<Node> = node.children_by_field_name("name", &mut cursor).collect();
        for item in names {
            let (imported, local) = match item.kind() {
                "aliased_import" => {
                    let name = item
                        .child_by_field_name("name")
                        .map(|n| self.b.text(n))
                        .unwrap_or_default();
                    let alias = item
                        .child_by_field_name("alias")
                        .map(|n| self.b.text(n))
                        .unwrap_or_else(|| name.clone());
                    (name, alias)
                }
                _ => {
                    let name = self.b.text(item);
                    (name.clone(), name)
                }
            };
            if imported.is_empty() {
                continue;
            }
            self.b.add_local(cx.fn_scope, &local);
            self.b.push_import(Import {
                module: module.clone(),
                level,
                kind: ImportKind::Named,
                imported: Some(imported),
                local: Some(local),
                scope: import_scope(cx),
                type_only: false,
                span: span(node),
            });
        }
    }

    /// Bind assignment targets: locals inside functions, variable symbols at
    /// module and class level.
    fn bind_targets(&mut self, left: Node, cx: Cx, stmt: Node) {
        if cx.fn_scope.is_some() {
            self.bind_locals(left, cx);
            return;
        }
        let mut names = Vec::new();
        pattern_names(left, &mut names, 0);
        let is_member = cx.scope.is_some();
        for name_node in names {
            let name = self.b.text(name_node);
            if self.b.has_child(cx.scope, &name, SymbolKind::Variable) {
                continue;
            }
            if let Some(id) = self
                .b
                .define(SymbolKind::Variable, name, cx.scope, stmt, name_node)
            {
                if is_member {
                    self.b.sym(id).attrs.insert("member".into(), true.into());
                }
            }
        }
    }

    fn bind_locals(&mut self, pattern: Node, cx: Cx) {
        if cx.fn_scope.is_none() {
            return;
        }
        let mut names = Vec::new();
        pattern_names(pattern, &mut names, 0);
        for n in names {
            let name = self.b.text(n);
            self.b.add_local(cx.fn_scope, &name);
        }
    }

    fn dunder_all(&mut self, value: Node) {
        if !matches!(value.kind(), "list" | "tuple") {
            // Computed `__all__`: the export surface is not statically known.
            self.b.attrs.insert("dunder_all_dynamic".into(), true.into());
            return;
        }
        self.b.attrs.insert("dunder_all".into(), true.into());
        for item in named_children(value) {
            if item.kind() != "string" {
                if item.kind() != "comment" {
                    self.b.attrs.insert("dunder_all_dynamic".into(), true.into());
                }
                continue;
            }
            let name = self.string_content(item);
            if name.is_empty() {
                continue;
            }
            self.b.exports.push(Export {
                kind: ExportKind::Named,
                name: name.clone(),
                local: Some(name),
                source: None,
                span: span(item),
            });
        }
    }

    fn string_content(&self, string: Node) -> String {
        let mut out = String::new();
        for part in named_children(string) {
            if part.kind() == "string_content" {
                out.push_str(&self.b.text(part));
            }
        }
        out
    }

    fn docstring(&self, body: Node) -> Option<String> {
        let first = body.named_child(0)?;
        if first.kind() != "expression_statement" {
            return None;
        }
        let string = first.named_child(0)?;
        if string.kind() != "string" {
            return None;
        }
        let content = self.string_content(string);
        let line = content.lines().map(str::trim).find(|l| !l.is_empty())?;
        Some(crate::common::truncate(line, 200))
    }

    /// `a.b.c` as segments, or `None` if the expression is not a plain dotted name.
    fn dotted(&self, expr: Node) -> Option<Vec<String>> {
        let mut segments = Vec::new();
        let mut current = expr;
        loop {
            match current.kind() {
                "identifier" => {
                    segments.push(self.b.text(current));
                    break;
                }
                "attribute" => {
                    segments.push(self.b.text(current.child_by_field_name("attribute")?));
                    current = current.child_by_field_name("object")?;
                }
                _ => return None,
            }
        }
        segments.reverse();
        Some(segments)
    }

    fn target(&self, expr: Node) -> Target {
        if let Some(path) = self.dotted(expr) {
            return Target { path, member: None };
        }
        if expr.kind() == "attribute" {
            let member = expr
                .child_by_field_name("attribute")
                .map(|n| self.b.text(n));
            // `super().method` — keep the receiver, it is statically meaningful.
            if let (Some(obj), Some(member)) = (expr.child_by_field_name("object"), member.clone()) {
                if obj.kind() == "call" {
                    if let Some(f) = obj.child_by_field_name("function") {
                        if f.kind() == "identifier" && self.b.text(f) == "super" {
                            return Target {
                                path: vec!["super".to_string(), member],
                                member: None,
                            };
                        }
                    }
                }
            }
            return Target {
                path: Vec::new(),
                member,
            };
        }
        Target::default()
    }
}

fn import_scope(cx: Cx) -> Option<u32> {
    // Function-level imports bind in that function; class-level imports bind
    // a class attribute and are recorded against the class.
    cx.fn_scope.or(cx.scope)
}

fn apply_decorators(sym: &mut usie_ir::Symbol, decorators: Vec<String>) {
    if decorators.is_empty() {
        return;
    }
    for d in &decorators {
        match d.as_str() {
            "staticmethod" => {
                sym.attrs.insert("static".into(), true.into());
            }
            "classmethod" => {
                sym.attrs.insert("classmethod".into(), true.into());
            }
            "property" => {
                sym.attrs.insert("property".into(), true.into());
            }
            _ => {}
        }
    }
    sym.attrs.insert("decorators".into(), decorators.into());
}

/// Collect the identifier nodes bound by a target/parameter pattern.
fn pattern_names<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>, depth: u32) {
    if depth > 64 {
        return;
    }
    match node.kind() {
        "identifier" => out.push(node),
        "default_parameter" | "typed_default_parameter" => {
            if let Some(name) = node.child_by_field_name("name") {
                pattern_names(name, out, depth + 1);
            }
        }
        "typed_parameter" => {
            if let Some(first) = node.named_child(0) {
                pattern_names(first, out, depth + 1);
            }
        }
        "parameters" | "lambda_parameters" | "pattern_list" | "tuple_pattern" | "list_pattern"
        | "list_splat_pattern" | "dictionary_splat_pattern" | "as_pattern_target"
        | "parenthesized_expression" | "tuple" | "list" | "expression_list" => {
            for child in named_children(node) {
                pattern_names(child, out, depth + 1);
            }
        }
        // Attribute and subscript targets (`self.x = …`, `a[i] = …`) bind no name.
        _ => {}
    }
}
