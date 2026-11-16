//! Java extraction adapter (tree-sitter-java).
//!
//! Conventions in the IR produced here:
//!
//! * Classes, records and enums are `Class` symbols (`attrs.type_kind`
//!   distinguishes them); interfaces and annotation types are `Interface`.
//!   Members are children of their type; constructors are methods with
//!   `attrs.constructor`.
//! * `exported` means declared `public`. Modifiers are kept in
//!   `attrs.modifiers`, annotations in `attrs.annotations`.
//! * Fields, record components and enum constants are `Variable` members
//!   with `attrs.type_path` (segments of the declared type, without type
//!   arguments) when the type is a class or interface type.
//! * Parameters and typed local variables are recorded in the method's
//!   `attrs.local_types`.
//! * Imports: `import a.b.C;` and `import static a.b.C.m;` are `Named`
//!   imports with `module` = everything before the last segment and
//!   `imported`/`local` = the last segment. `import a.b.*;` and
//!   `import static a.b.C.*;` are `Wildcard` imports of `a.b` / `a.b.C`.
//!   The resolver decides from the repository which segments are packages,
//!   types and members.
//! * The package declaration is stored in `attrs.package`.

use crate::common::{
    children, doc_comment, named_children, span, Builder, Cx, Extracted, Limits, LocalTypes,
    Target, MAX_CHAIN, MAX_DEPTH,
};
use tree_sitter::Node;
use usie_ir::{Import, ImportKind, ReferenceKind, SymbolKind};

pub(crate) fn extract(root: Node, src: &[u8], limits: Limits) -> Extracted {
    let mut j = Java {
        b: Builder::new(src, limits),
        types: LocalTypes::default(),
    };
    j.walk(root, Cx::default(), 0);
    let Java { mut b, types } = j;
    types.apply(&mut b);
    b.finish()
}

struct Java<'a> {
    b: Builder<'a>,
    types: LocalTypes,
}

const TYPE_DECLS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

impl<'a> Java<'a> {
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
            "package_declaration" => {
                if let Some(name) = named_children(node)
                    .into_iter()
                    .find(|c| matches!(c.kind(), "scoped_identifier" | "identifier"))
                {
                    let name = compact(&self.b.text(name));
                    self.b.attrs.insert("package".into(), name.into());
                }
            }
            "import_declaration" => self.import(node, cx),
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => self.method(node, cx, depth),
            "field_declaration" | "constant_declaration" => self.field(node, cx, depth),
            "local_variable_declaration" => {
                let declared = node.child_by_field_name("type");
                let var = declared.is_some_and(|t| self.b.text(t) == "var");
                let declared_path = if var {
                    None
                } else {
                    declared.and_then(|t| self.type_path(t))
                };
                let mut cursor = node.walk();
                let decls: Vec<Node> = node
                    .children_by_field_name("declarator", &mut cursor)
                    .collect();
                for d in decls {
                    let value = d.child_by_field_name("value");
                    let ty = if var {
                        value.and_then(|v| self.creation_type(v))
                    } else {
                        declared_path.clone()
                    };
                    let callee = value
                        .filter(|v| var && v.kind() == "method_invocation")
                        .and_then(|v| {
                            Some(self.invocation_target(v, v.child_by_field_name("name")?))
                        })
                        .map(|t| t.path)
                        .filter(|p| !p.is_empty());
                    if let Some(n) = d.child_by_field_name("name") {
                        match (ty, callee) {
                            (None, Some(callee)) => {
                                let name = self.b.text(n);
                                if cx.fn_scope.is_some() {
                                    self.b.add_local(cx.fn_scope, &name);
                                    self.types.bind_call(cx.fn_scope, &name, callee, 0);
                                } else {
                                    self.b.add_anonymous_local(&name);
                                }
                            }
                            (ty, _) => self.bind(cx, n, ty),
                        }
                    }
                    if let Some(v) = d.child_by_field_name("value") {
                        self.walk(v, cx, depth + 1);
                    }
                }
                if let Some(t) = declared {
                    self.annotations_in(t, cx);
                }
            }
            "enhanced_for_statement" => {
                let ty = node
                    .child_by_field_name("type")
                    .filter(|t| self.b.text(*t) != "var")
                    .and_then(|t| self.type_path(t));
                if let Some(n) = node.child_by_field_name("name") {
                    self.bind(cx, n, ty);
                }
                if let Some(v) = node.child_by_field_name("value") {
                    self.walk(v, cx, depth + 1);
                }
                if let Some(b) = node.child_by_field_name("body") {
                    self.walk(b, cx, depth + 1);
                }
            }
            "resource" => {
                let ty = node
                    .child_by_field_name("type")
                    .filter(|t| self.b.text(*t) != "var")
                    .and_then(|t| self.type_path(t));
                if let Some(n) = node.child_by_field_name("name") {
                    self.bind(cx, n, ty);
                }
                if let Some(v) = node.child_by_field_name("value") {
                    self.walk(v, cx, depth + 1);
                }
            }
            "catch_formal_parameter" => {
                let types: Vec<Node> = named_children(node)
                    .into_iter()
                    .filter(|c| c.kind() == "catch_type")
                    .flat_map(named_children)
                    .collect();
                let ty = if types.len() == 1 {
                    self.type_path(types[0])
                } else {
                    None
                };
                if let Some(n) = node.child_by_field_name("name") {
                    self.bind(cx, n, ty);
                }
            }
            "instanceof_expression" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let ty = node
                        .child_by_field_name("right")
                        .and_then(|t| self.type_path(t));
                    self.bind(cx, n, ty);
                }
                if let Some(left) = node.child_by_field_name("left") {
                    self.walk(left, cx, depth + 1);
                }
            }
            "lambda_expression" => {
                if let Some(params) = node.child_by_field_name("parameters") {
                    match params.kind() {
                        "identifier" => self.bind(cx, params, None),
                        "inferred_parameters" => {
                            for p in named_children(params) {
                                if p.kind() == "identifier" {
                                    self.bind(cx, p, None);
                                }
                            }
                        }
                        _ => self.parameters(params, cx),
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(body, cx, depth + 1);
                }
            }
            "method_invocation" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let target = self.invocation_target(node, name);
                    self.b
                        .push_reference(ReferenceKind::Call, cx, target, node, name);
                }
                if let Some(obj) = node.child_by_field_name("object") {
                    self.walk(obj, cx, depth + 1);
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    self.walk(args, cx, depth + 1);
                }
            }
            "explicit_constructor_invocation" => {
                // `super(...)` runs the base class constructor.
                if let Some(c) = node.child_by_field_name("constructor") {
                    if c.kind() == "super" && node.child_by_field_name("object").is_none() {
                        let target = Target {
                            path: vec!["super".into()],
                            member: None,
                            ..Default::default()
                        };
                        self.b.push_reference(ReferenceKind::Call, cx, target, c, c);
                    }
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    self.walk(args, cx, depth + 1);
                }
            }
            "object_creation_expression" => {
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
                for child in named_children(node) {
                    match child.kind() {
                        "argument_list" => self.walk(child, cx, depth + 1),
                        // Anonymous class body: its methods belong to the
                        // enclosing scope's anonymous class.
                        "class_body" => {
                            let anon = Cx { anon: true, ..cx };
                            self.walk_children(child, anon, depth + 1);
                        }
                        k if k != "type_identifier"
                            && k != "scoped_type_identifier"
                            && k != "generic_type" =>
                        {
                            self.walk(child, cx, depth + 1)
                        }
                        _ => {}
                    }
                }
            }
            "marker_annotation" | "annotation" => {
                self.annotation(node, cx);
                if let Some(args) = node.child_by_field_name("arguments") {
                    self.walk(args, cx, depth + 1);
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

    fn import(&mut self, node: Node, cx: Cx) {
        let kids = children(node);
        let wildcard = kids.iter().any(|c| c.kind() == "asterisk");
        let Some(name) = kids
            .iter()
            .find(|c| matches!(c.kind(), "scoped_identifier" | "identifier"))
        else {
            return;
        };
        let full = compact(&self.b.text(*name));
        let import = if wildcard {
            Import {
                module: full,
                level: 0,
                kind: ImportKind::Wildcard,
                imported: None,
                local: None,
                scope: cx.fn_scope,
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
                imported: Some(last.clone()),
                local: Some(last),
                scope: cx.fn_scope,
                type_only: false,
                span: span(node),
            }
        };
        self.b.push_import(import);
    }

    /// Modifier keywords and annotations of a declaration; annotation uses
    /// are recorded as references in the enclosing scope.
    fn modifiers(&mut self, decl: Node, cx: Cx) -> (Vec<String>, Vec<String>) {
        let mut keywords = Vec::new();
        let mut annotations = Vec::new();
        let Some(mods) = children(decl).into_iter().find(|c| c.kind() == "modifiers") else {
            return (keywords, annotations);
        };
        for m in children(mods) {
            match m.kind() {
                "marker_annotation" | "annotation" => {
                    if let Some(n) = m.child_by_field_name("name") {
                        annotations.push(compact(&self.b.text(n)));
                    }
                    self.annotation(m, cx);
                    if let Some(args) = m.child_by_field_name("arguments") {
                        self.walk(args, cx, 1);
                    }
                }
                k if !m.is_named() || k.ends_with("_modifier") => {
                    keywords.push(self.b.text(m));
                }
                _ => {}
            }
        }
        (keywords, annotations)
    }

    fn annotation(&mut self, node: Node, cx: Cx) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let path: Vec<String> = compact(&self.b.text(name))
            .split('.')
            .map(str::to_string)
            .collect();
        self.b.push_reference(
            ReferenceKind::Decorator,
            cx,
            Target {
                path,
                member: None,
                ..Default::default()
            },
            name,
            name,
        );
    }

    fn annotations_in(&mut self, node: Node, cx: Cx) {
        for child in named_children(node) {
            if matches!(child.kind(), "marker_annotation" | "annotation") {
                self.annotation(child, cx);
            }
        }
    }

    fn type_decl(&mut self, node: Node, cx: Cx, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.walk_children(node, cx, depth);
            return;
        };
        let name = self.b.text(name_node);
        let (kind, type_kind) = match node.kind() {
            "interface_declaration" => (SymbolKind::Interface, "interface"),
            "annotation_type_declaration" => (SymbolKind::Interface, "annotation"),
            "enum_declaration" => (SymbolKind::Class, "enum"),
            "record_declaration" => (SymbolKind::Class, "record"),
            _ => (SymbolKind::Class, "class"),
        };
        let (keywords, annotations) = self.modifiers(node, cx);
        if cx.fn_scope.is_some() {
            self.b.add_local(cx.fn_scope, &name);
        }
        let doc = doc_comment(&self.b, node);
        let Some(id) = self.b.define(kind, name, cx.scope, node, name_node) else {
            return;
        };
        {
            let sym = self.b.sym(id);
            sym.exported = keywords.iter().any(|k| k == "public");
            sym.doc = doc;
            sym.attrs.insert("type_kind".into(), type_kind.into());
            if cx.anon {
                sym.attrs.insert("in_anonymous_scope".into(), true.into());
            }
            if !keywords.is_empty() {
                sym.attrs.insert("modifiers".into(), keywords.into());
            }
            if !annotations.is_empty() {
                sym.attrs.insert("annotations".into(), annotations.into());
            }
        }
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            let text = self.b.text_truncated(tp);
            self.b.sym(id).signature = Some(text);
        }
        // Heritage.
        if let Some(sup) = node.child_by_field_name("superclass") {
            for t in named_children(sup) {
                if let Some(segments) = self.type_path(t) {
                    let base = self.b.name_path(t, segments);
                    self.b.sym(id).bases.push(base);
                }
            }
        }
        if let Some(ifaces) = node.child_by_field_name("interfaces") {
            for t in self.type_list(ifaces) {
                if let Some(segments) = self.type_path(t) {
                    let base = self.b.name_path(t, segments);
                    self.b.sym(id).implements.push(base);
                }
            }
        }
        if let Some(ext) = children(node)
            .into_iter()
            .find(|c| c.kind() == "extends_interfaces")
        {
            for t in self.type_list(ext) {
                if let Some(segments) = self.type_path(t) {
                    let base = self.b.name_path(t, segments);
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
        // Record components are fields.
        if let Some(params) = node.child_by_field_name("parameters") {
            for p in named_children(params) {
                if p.kind() != "formal_parameter" {
                    continue;
                }
                let (Some(n), ty) = (p.child_by_field_name("name"), p.child_by_field_name("type"))
                else {
                    continue;
                };
                let fname = self.b.text(n);
                if let Some(fid) = self.b.define(SymbolKind::Variable, fname, Some(id), p, n) {
                    let path = ty.and_then(|t| self.type_path(t));
                    let sig = ty.map(|t| self.b.text_truncated(t));
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
        if let Some(body) = node.child_by_field_name("body") {
            for member in named_children(body) {
                match member.kind() {
                    "enum_constant" => self.enum_constant(member, id, inner, depth + 2),
                    "enum_body_declarations" => self.walk_children(member, inner, depth + 2),
                    _ => self.walk(member, inner, depth + 2),
                }
            }
        }
    }

    fn type_list<'t>(&self, node: Node<'t>) -> Vec<Node<'t>> {
        named_children(node)
            .into_iter()
            .flat_map(|c| {
                if c.kind() == "type_list" {
                    named_children(c)
                } else {
                    vec![c]
                }
            })
            .collect()
    }

    fn enum_constant(&mut self, node: Node, owner: u32, cx: Cx, depth: u32) {
        let Some(n) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.b.text(n);
        let owner_name = self.b.symbols[owner as usize].name.clone();
        if let Some(id) = self
            .b
            .define(SymbolKind::Variable, name, Some(owner), node, n)
        {
            let sym = self.b.sym(id);
            sym.exported = true;
            sym.attrs.insert("member".into(), true.into());
            sym.attrs.insert("enum_constant".into(), true.into());
            sym.attrs
                .insert("type_path".into(), vec![owner_name].into());
        }
        if let Some(args) = node.child_by_field_name("arguments") {
            self.walk(args, cx, depth + 1);
        }
        if let Some(body) = node.child_by_field_name("body") {
            let anon = Cx { anon: true, ..cx };
            self.walk_children(body, anon, depth + 1);
        }
    }

    fn field(&mut self, node: Node, cx: Cx, depth: u32) {
        let (keywords, annotations) = self.modifiers(node, cx);
        let ty = node.child_by_field_name("type");
        let path = ty.and_then(|t| self.type_path(t));
        let sig = ty.map(|t| self.b.text_truncated(t));
        let doc = doc_comment(&self.b, node);
        let in_interface = cx
            .scope
            .is_some_and(|s| self.b.symbols[s as usize].kind == SymbolKind::Interface);
        let mut cursor = node.walk();
        let decls: Vec<Node> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        for d in decls {
            let Some(n) = d.child_by_field_name("name") else {
                continue;
            };
            let name = self.b.text(n);
            if cx.fn_scope.is_some() {
                self.bind(cx, n, path.clone());
            } else if let Some(id) = self.b.define(SymbolKind::Variable, name, cx.scope, d, n) {
                let sym = self.b.sym(id);
                sym.exported = in_interface || keywords.iter().any(|k| k == "public");
                sym.signature = sig.clone();
                sym.doc = doc.clone();
                sym.attrs.insert("member".into(), true.into());
                if in_interface || keywords.iter().any(|k| k == "static") {
                    sym.attrs.insert("static".into(), true.into());
                }
                if !keywords.is_empty() {
                    sym.attrs
                        .insert("modifiers".into(), keywords.clone().into());
                }
                if !annotations.is_empty() {
                    sym.attrs
                        .insert("annotations".into(), annotations.clone().into());
                }
                if let Some(p) = &path {
                    sym.attrs.insert("type_path".into(), p.clone().into());
                }
            }
            if let Some(v) = d.child_by_field_name("value") {
                self.walk(v, cx, depth + 1);
            }
        }
    }

    fn method(&mut self, node: Node, cx: Cx, depth: u32) {
        let (keywords, annotations) = self.modifiers(node, cx);
        let constructor = node.kind() != "method_declaration";
        let name_node = node.child_by_field_name("name");
        let Some(name_node) = name_node else {
            self.walk_children(node, cx, depth);
            return;
        };
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
        let doc = doc_comment(&self.b, node);
        let Some(id) = self.b.define(kind, name, cx.scope, node, name_node) else {
            return;
        };
        let mut signature = String::new();
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            signature.push_str(&self.b.text_truncated(tp));
            signature.push(' ');
        }
        let params = node.child_by_field_name("parameters");
        match params {
            Some(p) => signature.push_str(&self.b.text_truncated(p)),
            None => signature.push_str("()"),
        }
        if let Some(ret) = node.child_by_field_name("type") {
            signature.push_str(": ");
            signature.push_str(&self.b.text_truncated(ret));
        }
        let in_interface = parent_kind == Some(SymbolKind::Interface);
        let result = node
            .child_by_field_name("type")
            .and_then(|t| self.type_path(t));
        {
            let sym = self.b.sym(id);
            sym.signature = Some(signature);
            if let Some(result) = result {
                sym.attrs.insert(
                    "result_types".into(),
                    vec![serde_json::Value::from(result)].into(),
                );
            }
            sym.doc = doc;
            sym.exported = in_interface || keywords.iter().any(|k| k == "public");
            if constructor {
                sym.attrs.insert("constructor".into(), true.into());
            }
            if keywords.iter().any(|k| k == "static") {
                sym.attrs.insert("static".into(), true.into());
            }
            if keywords.iter().any(|k| k == "abstract")
                || (in_interface
                    && node.child_by_field_name("body").is_none()
                    && !keywords.iter().any(|k| k == "default" || k == "static"))
            {
                sym.attrs.insert("abstract".into(), true.into());
            }
            if cx.anon {
                sym.attrs.insert("in_anonymous_scope".into(), true.into());
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
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, inner, depth + 1);
        }
    }

    fn parameters(&mut self, params: Node, cx: Cx) {
        for p in named_children(params) {
            match p.kind() {
                "formal_parameter" => {
                    let ty = p
                        .child_by_field_name("type")
                        .and_then(|t| self.type_path(t));
                    if let Some(n) = p.child_by_field_name("name") {
                        self.bind(cx, n, ty);
                    }
                }
                "spread_parameter" => {
                    // Varargs: the local is an array, whose type is not tracked.
                    for d in named_children(p) {
                        if d.kind() == "variable_declarator" {
                            if let Some(n) = d.child_by_field_name("name") {
                                self.bind(cx, n, None);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The class instantiated by `new T(...)`, for `var x = new T()`.
    fn creation_type(&self, value: Node) -> Option<Vec<String>> {
        if value.kind() != "object_creation_expression" {
            return None;
        }
        // An anonymous subclass is not `T` itself, but has `T`'s members.
        self.type_path(value.child_by_field_name("type")?)
    }

    /// Segments of a class or interface type, without type arguments.
    /// `None` for primitives, arrays and `var`.
    fn type_path(&self, ty: Node) -> Option<Vec<String>> {
        match ty.kind() {
            "type_identifier" | "identifier" => {
                let t = self.b.text(ty);
                (t != "var").then(|| vec![t])
            }
            "scoped_type_identifier" | "scoped_identifier" => Some(
                named_children(ty)
                    .into_iter()
                    .filter(|c| {
                        matches!(
                            c.kind(),
                            "type_identifier"
                                | "identifier"
                                | "scoped_type_identifier"
                                | "scoped_identifier"
                                | "generic_type"
                        )
                    })
                    .map(|c| self.type_path(c))
                    .collect::<Option<Vec<Vec<String>>>>()?
                    .concat(),
            ),
            "generic_type" => self.type_path(ty.named_child(0)?),
            _ => None,
        }
    }

    /// `a.b.c` as segments for receivers: identifiers, `this`, `super` and
    /// field accesses; `None` for any other expression.
    fn dotted(&self, expr: Node) -> Option<Vec<String>> {
        match expr.kind() {
            "identifier" => Some(vec![self.b.text(expr)]),
            "this" => Some(vec!["this".into()]),
            "super" => Some(vec!["super".into()]),
            "field_access" => {
                let mut path = self.dotted(expr.child_by_field_name("object")?)?;
                path.push(self.b.text(expr.child_by_field_name("field")?));
                Some(path)
            }
            "parenthesized_expression" => self.dotted(expr.named_child(0)?),
            _ => None,
        }
    }

    fn invocation_target(&self, node: Node, name: Node) -> Target {
        let method = self.b.text(name);
        match node.child_by_field_name("object") {
            None => Target {
                path: vec![method],
                member: None,
                ..Default::default()
            },
            Some(obj) => match self.dotted(obj) {
                Some(mut path) => {
                    path.push(method);
                    Target {
                        path,
                        member: None,
                        ..Default::default()
                    }
                }
                None => {
                    let chain = self
                        .chain(obj, 0)
                        .map(|mut c| {
                            c.push(method.clone());
                            c
                        })
                        .filter(|c| c.len() <= MAX_CHAIN)
                        .unwrap_or_default();
                    Target {
                        path: Vec::new(),
                        member: Some(method),
                        chain,
                    }
                }
            },
        }
    }

    /// Steps that produce the value of `expr` (see `Reference::chain`), or
    /// `None` when they are not statically describable.
    fn chain(&self, expr: Node, depth: u32) -> Option<Vec<String>> {
        if depth > MAX_CHAIN as u32 {
            return None;
        }
        match expr.kind() {
            "identifier" | "this" | "super" => self.dotted(expr),
            "field_access" => {
                let mut c = self.chain(expr.child_by_field_name("object")?, depth + 1)?;
                c.push(self.b.text(expr.child_by_field_name("field")?));
                Some(c)
            }
            "method_invocation" => {
                let mut c = match expr.child_by_field_name("object") {
                    Some(obj) => self.chain(obj, depth + 1)?,
                    None => Vec::new(),
                };
                c.push(self.b.text(expr.child_by_field_name("name")?));
                c.push("()".into());
                Some(c)
            }
            "object_creation_expression" => {
                let ty = self.type_path(expr.child_by_field_name("type")?)?;
                Some(vec![format!("type:{}", ty.join("."))])
            }
            "cast_expression" => {
                let ty = self.type_path(expr.child_by_field_name("type")?)?;
                Some(vec![format!("type:{}", ty.join("."))])
            }
            "parenthesized_expression" => self.chain(expr.named_child(0)?, depth + 1),
            _ => None,
        }
    }
}

/// Remove whitespace from a dotted name (`a . b` → `a.b`).
fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}
