//! Shared extraction machinery used by every language adapter.

use std::collections::BTreeSet;
use tree_sitter::Node;
use usie_ir::{
    Attrs, Diagnostic, DiagnosticCode, Export, Import, NamePath, Reference, ReferenceKind, Span,
    Symbol, SymbolKind,
};

/// Maximum recursion depth of the extraction walk. Source nested deeper than
/// this is not descended into; the file is flagged `truncated`.
pub(crate) const MAX_DEPTH: u32 = 400;
pub(crate) const MAX_DIAGNOSTICS: usize = 50;
const MAX_TEXT: usize = 200;

/// Per-file hard caps so a pathological input cannot exhaust memory.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_symbols: usize,
    pub max_references: usize,
    pub max_imports: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_symbols: 100_000,
            max_references: 400_000,
            max_imports: 50_000,
        }
    }
}

/// Lexical context threaded through the walk.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Cx {
    /// Innermost enclosing symbol of any kind.
    pub scope: Option<u32>,
    /// Innermost enclosing function-like symbol (owner of local bindings).
    pub fn_scope: Option<u32>,
    /// The current declaration is syntactically exported.
    pub exported: bool,
    /// Inside an anonymous function that has no enclosing symbol. Bindings
    /// made here are tracked file-wide (see `anonymous_locals`).
    pub anon: bool,
}

pub(crate) struct Builder<'a> {
    pub src: &'a [u8],
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub references: Vec<Reference>,
    pub attrs: Attrs,
    pub nodes: u64,
    pub truncated: bool,
    locals: Vec<BTreeSet<String>>,
    /// Names a scope explicitly binds to an outer scope (`global`/`nonlocal`).
    outer_bound: Vec<BTreeSet<String>>,
    /// Names bound inside anonymous functions that have no enclosing symbol.
    anon_locals: BTreeSet<String>,
    limits: Limits,
}

impl<'a> Builder<'a> {
    pub fn new(src: &'a [u8], limits: Limits) -> Self {
        Builder {
            src,
            symbols: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            references: Vec::new(),
            attrs: Attrs::new(),
            nodes: 0,
            truncated: false,
            locals: Vec::new(),
            outer_bound: Vec::new(),
            anon_locals: BTreeSet::new(),
            limits,
        }
    }

    pub fn text(&self, node: Node) -> String {
        String::from_utf8_lossy(&self.src[node.start_byte()..node.end_byte()]).into_owned()
    }

    pub fn text_truncated(&self, node: Node) -> String {
        truncate(&collapse_ws(&self.text(node)), MAX_TEXT)
    }

    /// Declare a symbol. Returns `None` when the per-file cap is reached.
    pub fn define(
        &mut self,
        kind: SymbolKind,
        name: String,
        parent: Option<u32>,
        node: Node,
        name_node: Node,
    ) -> Option<u32> {
        if self.symbols.len() >= self.limits.max_symbols {
            self.truncated = true;
            return None;
        }
        let id = self.symbols.len() as u32;
        let qualified_name = match parent {
            Some(p) => format!("{}.{}", self.symbols[p as usize].qualified_name, name),
            None => name.clone(),
        };
        self.symbols.push(Symbol {
            id,
            parent,
            kind,
            name,
            qualified_name,
            span: span(node),
            name_span: span(name_node),
            signature: None,
            exported: false,
            bases: Vec::new(),
            implements: Vec::new(),
            locals: Vec::new(),
            doc: None,
            attrs: Attrs::new(),
        });
        self.locals.push(BTreeSet::new());
        self.outer_bound.push(BTreeSet::new());
        Some(id)
    }

    pub fn sym(&mut self, id: u32) -> &mut Symbol {
        &mut self.symbols[id as usize]
    }

    /// Whether `parent` already directly contains a symbol with this name and kind.
    pub fn has_child(&self, parent: Option<u32>, name: &str, kind: SymbolKind) -> bool {
        // Scan backwards: siblings are usually declared close together.
        self.symbols
            .iter()
            .rev()
            .any(|s| s.parent == parent && s.kind == kind && s.name == name)
    }

    pub fn add_local(&mut self, fn_scope: Option<u32>, name: &str) {
        if let Some(s) = fn_scope {
            if !name.is_empty() {
                self.locals[s as usize].insert(name.to_string());
            }
        }
    }

    /// Record a binding made in an anonymous function with no owning symbol.
    pub fn add_anonymous_local(&mut self, name: &str) {
        if !name.is_empty() && self.anon_locals.len() < 50_000 {
            self.anon_locals.insert(name.to_string());
        }
    }

    /// The most recently declared direct child of `parent` with this name and kind.
    pub fn find_child(&self, parent: Option<u32>, name: &str, kind: SymbolKind) -> Option<u32> {
        self.symbols
            .iter()
            .rev()
            .find(|s| s.parent == parent && s.kind == kind && s.name == name)
            .map(|s| s.id)
    }

    pub fn bind_outer(&mut self, fn_scope: Option<u32>, name: &str) {
        if let Some(s) = fn_scope {
            self.outer_bound[s as usize].insert(name.to_string());
        }
    }

    pub fn push_import(&mut self, import: Import) {
        if self.imports.len() >= self.limits.max_imports {
            self.truncated = true;
            return;
        }
        self.imports.push(import);
    }

    pub fn push_reference(
        &mut self,
        kind: ReferenceKind,
        cx: Cx,
        target: Target,
        text_node: Node,
        span_node: Node,
    ) {
        if self.references.len() >= self.limits.max_references {
            self.truncated = true;
            return;
        }
        self.references.push(Reference {
            kind,
            scope: cx.scope,
            path: target.path,
            member: target.member,
            text: self.text_truncated(text_node),
            span: span(span_node),
        });
    }

    pub fn name_path(&self, node: Node, segments: Vec<String>) -> NamePath {
        NamePath {
            segments,
            text: self.text_truncated(node),
            span: span(node),
        }
    }

    /// Finalize: move the collected local-binding sets into the symbols.
    pub fn finish(mut self) -> Extracted {
        for (i, sym) in self.symbols.iter_mut().enumerate() {
            let outer = &self.outer_bound[i];
            sym.locals = self.locals[i]
                .iter()
                .filter(|n| !outer.contains(*n))
                .cloned()
                .collect();
        }
        if self.truncated {
            self.attrs.insert("truncated".into(), true.into());
        }
        if !self.anon_locals.is_empty() {
            let names: Vec<String> = self.anon_locals.iter().cloned().collect();
            self.attrs.insert("anonymous_locals".into(), names.into());
        }
        Extracted {
            symbols: self.symbols,
            imports: self.imports,
            exports: self.exports,
            references: self.references,
            attrs: self.attrs,
            nodes: self.nodes,
        }
    }
}

pub(crate) struct Extracted {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub references: Vec<Reference>,
    pub attrs: Attrs,
    pub nodes: u64,
}

/// Target of a reference before resolution.
#[derive(Debug, Default)]
pub(crate) struct Target {
    pub path: Vec<String>,
    pub member: Option<String>,
}

pub(crate) fn span(node: Node) -> Span {
    let s = node.start_position();
    let e = node.end_position();
    Span {
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
        start_line: s.row as u32 + 1,
        start_col: s.column as u32,
        end_line: e.row as u32 + 1,
        end_col: e.column as u32,
    }
}

pub(crate) fn children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

pub(crate) fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

pub(crate) fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws && !out.is_empty() {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Collect syntax diagnostics from tree-sitter error and missing nodes.
/// Returns the bounded list and the total count.
pub(crate) fn collect_diagnostics(root: Node, src: &[u8]) -> (Vec<Diagnostic>, u64) {
    let mut out = Vec::new();
    let mut total = 0u64;
    if !root.has_error() {
        return (out, 0);
    }
    // Iterative walk: error recovery can produce very deep trees.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            total += 1;
            if out.len() < MAX_DIAGNOSTICS {
                out.push(Diagnostic {
                    code: DiagnosticCode::MissingToken,
                    message: format!("missing {}", describe_kind(node.kind())),
                    span: span(node),
                });
            }
            continue;
        }
        if node.is_error() {
            total += 1;
            if out.len() < MAX_DIAGNOSTICS {
                let text = String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]);
                let first_line = text.lines().next().unwrap_or("").trim();
                out.push(Diagnostic {
                    code: DiagnosticCode::SyntaxError,
                    message: if first_line.is_empty() {
                        "syntax error".to_string()
                    } else {
                        format!("syntax error near `{}`", truncate(first_line, 60))
                    },
                    span: span(node),
                });
            }
            // Do not report errors nested inside an error region.
            continue;
        }
        if node.has_error() {
            let mut kids = children(node);
            kids.reverse();
            stack.extend(kids);
        }
    }
    out.sort_by_key(|d| (d.span.start_byte, d.span.end_byte));
    (out, total)
}

fn describe_kind(kind: &str) -> String {
    if kind.chars().all(|c| c.is_alphanumeric() || c == '_') {
        kind.replace('_', " ")
    } else {
        format!("`{kind}`")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_ws_normalizes_runs() {
        assert_eq!(collapse_ws("  a \n\t b  "), "a b");
        assert_eq!(collapse_ws(""), "");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "héllo";
        assert_eq!(truncate(s, 2), "h…");
        assert_eq!(truncate(s, 10), "héllo");
    }
}
