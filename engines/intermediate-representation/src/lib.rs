//! USIE normalized intermediate representation (IR).
//!
//! This crate is the contract between the parsing engine, the semantic engine
//! and the Go services. Every type here is serialized as JSON; changing a
//! field is a contract change and requires bumping [`IR_SCHEMA_VERSION`].
//!
//! Design rules:
//!
//! * The IR is *structural*, not a shared AST. It captures the facts common
//!   analyses need (declarations, bindings, call sites, diagnostics) and keeps
//!   language-specific detail in the open `attrs` maps instead of forcing
//!   every language into one tree shape.
//! * The IR never contains resolved cross-file facts. Resolution is the job
//!   of `usie-semantic`, which records provenance and confidence for every
//!   relationship it derives.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Bump on any breaking change to the serialized IR. The parse cache is keyed
/// on this value, so bumping it invalidates previously cached analyses.
pub const IR_SCHEMA_VERSION: u32 = 1;

/// Open metadata map for language-specific information.
pub type Attrs = BTreeMap<String, serde_json::Value>;

/// A source range. Lines are 1-based. Columns are 0-based UTF-8 byte offsets
/// within the line. Byte offsets are relative to the start of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Python,
    Javascript,
    Typescript,
    Tsx,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Python => "python",
            Language::Javascript => "javascript",
            Language::Typescript => "typescript",
            Language::Tsx => "tsx",
        }
    }

    pub fn parse(s: &str) -> Option<Language> {
        match s {
            "python" => Some(Language::Python),
            "javascript" | "jsx" => Some(Language::Javascript),
            "typescript" => Some(Language::Typescript),
            "tsx" => Some(Language::Tsx),
            _ => None,
        }
    }

    /// Languages that share module resolution and scoping rules.
    pub fn family(&self) -> LanguageFamily {
        match self {
            Language::Python => LanguageFamily::Python,
            _ => LanguageFamily::Ecmascript,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageFamily {
    Python,
    Ecmascript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Interface,
    /// Type aliases and enums. `attrs.type_kind` distinguishes them.
    Type,
    /// Module-level variables and class fields. `attrs.member` marks fields.
    Variable,
    /// TypeScript namespaces / internal modules.
    Namespace,
}

impl SymbolKind {
    pub fn is_callable(&self) -> bool {
        matches!(self, SymbolKind::Function | SymbolKind::Method)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Interface => "interface",
            SymbolKind::Type => "type",
            SymbolKind::Variable => "variable",
            SymbolKind::Namespace => "namespace",
        }
    }
}

/// A declaration extracted from one file. `id` is the index of the symbol in
/// [`FileIr::symbols`]; `parent` forms the lexical scope tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    pub kind: SymbolKind,
    pub name: String,
    /// Dotted path of enclosing symbols within the file, e.g. `Outer.method`.
    pub qualified_name: String,
    /// Range of the whole declaration.
    pub span: Span,
    /// Range of the declared name.
    pub name_span: Span,
    /// Declaration header text (parameters and return type), truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Whether the declaration is exported by syntax (`export` in ECMAScript).
    /// Always false for Python, where export status is not syntactic.
    #[serde(default)]
    pub exported: bool,
    /// Base classes / extended interfaces, as written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<NamePath>,
    /// Implemented interfaces (`implements` in TypeScript), as written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<NamePath>,
    /// Names bound inside this function-like scope (parameters, local
    /// assignments, nested declarations). Used for shadowing checks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<String>,
    /// First line of the documentation comment, truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: Attrs,
}

/// A possibly dotted name as written in source, e.g. `module.Base`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamePath {
    /// Simple-name segments. Empty when the expression is not a plain dotted
    /// name (e.g. a call or subscript); `text` still holds the source.
    pub segments: Vec<String>,
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    /// `import { a as b } from "m"` / `from m import a as b`
    Named,
    /// `import d from "m"`
    Default,
    /// `import * as ns from "m"` / `import m` / `import m as x` / `require("m")`
    Namespace,
    /// `import "m"`
    SideEffect,
    /// `from m import *`
    Wildcard,
    /// `import("m")` — target known statically, binding is not.
    Dynamic,
}

/// One binding introduced by an import statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Import {
    /// Module specifier exactly as written, without relative-level dots for
    /// Python (`from ..pkg import x` has `module = "pkg"`, `level = 2`).
    pub module: String,
    /// Python relative import level (number of leading dots). 0 otherwise.
    #[serde(default)]
    pub level: u32,
    pub kind: ImportKind,
    /// Name imported from the module. `None` for namespace, side-effect,
    /// wildcard and dynamic imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<String>,
    /// Local binding name. For an un-aliased Python `import a.b.c` this is
    /// the dotted path `a.b.c`. `None` when no binding is introduced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// Enclosing function-like symbol when the import is not at module level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<u32>,
    /// `import type` in TypeScript.
    #[serde(default)]
    pub type_only: bool,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
    /// A named export of a local binding or a re-export of a named binding.
    Named,
    /// `export default ...`
    Default,
    /// `export * from "m"`
    Wildcard,
    /// `export * as ns from "m"`
    Namespace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Export {
    pub kind: ExportKind,
    /// Exported name (`default` for default exports, empty for wildcards).
    pub name: String,
    /// Local (or, for re-exports, source-module) name being exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// Source module for re-exports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Call,
    /// `new Foo()`
    New,
    /// `<Foo />`
    Jsx,
    /// `@decorator`
    Decorator,
}

impl ReferenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReferenceKind::Call => "call",
            ReferenceKind::New => "new",
            ReferenceKind::Jsx => "jsx",
            ReferenceKind::Decorator => "decorator",
        }
    }
}

/// An unresolved use site. Resolution happens in the semantic engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub kind: ReferenceKind,
    /// Innermost enclosing symbol; `None` at module level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<u32>,
    /// Target as simple-name segments (`["self", "save"]`, `["os", "path",
    /// "join"]`). Empty when the receiver is not a plain dotted name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// Final member name when the target is a member access whose receiver
    /// is not a plain dotted name (e.g. `make().run()` has `member = "run"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Target source text, truncated.
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    SyntaxError,
    MissingToken,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub message: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParserInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ParseStats {
    /// Number of syntax tree nodes visited.
    pub nodes: u64,
    /// Number of diagnostics before truncation.
    pub diagnostics_total: u64,
    pub parse_micros: u64,
    pub extract_micros: u64,
}

/// Normalized structural representation of one source file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileIr {
    pub schema_version: u32,
    pub language: Language,
    pub parser: ParserInfo,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub references: Vec<Reference>,
    /// Syntax diagnostics, truncated to a bounded number.
    pub diagnostics: Vec<Diagnostic>,
    pub stats: ParseStats,
    /// File-level language-specific metadata (e.g. Python `__all__`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: Attrs,
}

impl FileIr {
    pub fn has_syntax_errors(&self) -> bool {
        self.stats.diagnostics_total > 0
    }
}

/// Epistemic status of a derived relationship. Ordered from strongest to
/// weakest; see `docs/specifications/semantic-resolution.md` for the exact
/// rules that produce each level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Established by the language's lexical binding and module rules without
    /// heuristics or type inference.
    Confirmed,
    /// Established by a rule that holds unless the program uses dynamic
    /// features the analysis cannot see (e.g. `self.m()` dispatch).
    Probable,
    /// Name-based heuristic match. May be wrong.
    Inferred,
    /// The analysis could not establish a target.
    Unknown,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::Confirmed => "confirmed",
            Confidence::Probable => "probable",
            Confidence::Inferred => "inferred",
            Confidence::Unknown => "unknown",
        }
    }

    /// The weaker of two confidences.
    pub fn min(self, other: Confidence) -> Confidence {
        std::cmp::max(self, other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_orders_strongest_first() {
        assert!(Confidence::Confirmed < Confidence::Probable);
        assert_eq!(
            Confidence::Confirmed.min(Confidence::Inferred),
            Confidence::Inferred
        );
    }

    #[test]
    fn language_round_trips() {
        for l in [
            Language::Python,
            Language::Javascript,
            Language::Typescript,
            Language::Tsx,
        ] {
            assert_eq!(Language::parse(l.as_str()), Some(l));
        }
        assert_eq!(Language::parse("cobol"), None);
    }

    #[test]
    fn symbol_serialization_omits_empty_fields() {
        let s = Symbol {
            id: 0,
            parent: None,
            kind: SymbolKind::Function,
            name: "f".into(),
            qualified_name: "f".into(),
            span: Span::default(),
            name_span: Span::default(),
            signature: None,
            exported: false,
            bases: vec![],
            implements: vec![],
            locals: vec![],
            doc: None,
            attrs: Attrs::new(),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert!(json.get("parent").is_none());
        assert!(json.get("locals").is_none());
        let back: Symbol = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }
}
