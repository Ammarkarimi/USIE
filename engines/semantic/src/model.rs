//! Input and output types of the semantic engine. These are the wire types of
//! the `usie-engine resolve` command.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use usie_ir::{Attrs, Confidence, FileIr, Span};

/// A parsed source file. `path` is repository-relative with `/` separators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub ir: FileIr,
}

/// Repository facts the resolver needs but cannot derive from source IR.
/// All of them are extracted from manifests by the ingestion service.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Every file path in the repository version, including unparsed files,
    /// so imports of assets (`./style.css`) and package markers resolve.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<DeclaredDependency>,
    #[serde(default)]
    pub tsconfigs: Vec<TsConfig>,
    /// Packages defined inside the repository (npm workspaces).
    #[serde(default)]
    pub packages: Vec<WorkspacePackage>,
    /// Extra Python source roots (directories on the import path).
    #[serde(default)]
    pub python_roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclaredDependency {
    /// `npm` or `pypi`.
    pub ecosystem: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TsConfig {
    /// Directory containing the tsconfig/jsconfig file ("" for the root).
    pub dir: String,
    /// `compilerOptions.baseUrl`, relative to `dir`.
    #[serde(default)]
    pub base_url: Option<String>,
    /// `compilerOptions.paths`.
    #[serde(default)]
    pub paths: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacePackage {
    pub name: String,
    pub dir: String,
    /// Candidate entry files relative to `dir` (from `exports`, `main`, …).
    #[serde(default)]
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub config: ProjectConfig,
    pub files: Vec<SourceFile>,
}

/// A knowledge-graph node produced by resolution: a symbol or an external
/// package. File and directory nodes are owned by the ingestion service and
/// are referred to by key (`file:<path>`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub key: String,
    /// `function`, `method`, `class`, `interface`, `type`, `variable`,
    /// `namespace` or `package`.
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
    /// Defining file, for symbols.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default)]
    pub exported: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: Attrs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeRecord {
    pub src: String,
    pub dst: String,
    /// `CONTAINS`, `IMPORTS`, `EXPORTS`, `CALLS`, `INSTANTIATES`, `EXTENDS`,
    /// `IMPLEMENTS` or `REFERENCES`.
    pub kind: String,
    pub confidence: Confidence,
    /// Identifier of the resolution rule that produced the edge (provenance).
    pub rule: String,
    /// Where the strongest evidence for the edge is located.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// Number of source occurrences aggregated into this edge.
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub path: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Resolved to a node inside the repository.
    Resolved,
    /// Resolved to an external package.
    External,
    /// A language builtin; no graph edge is produced.
    Builtin,
    /// Could not be resolved. `reason` says why. Nothing is guessed.
    Unresolved,
}

/// One syntactic reference (call, instantiation, JSX use, decorator, import,
/// base class) and what resolution established about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceRecord {
    pub path: String,
    pub line: u32,
    pub col: u32,
    /// `call`, `new`, `jsx`, `decorator`, `import`, `extends`, `implements`.
    pub kind: String,
    pub text: String,
    /// Key of the node the reference occurs in (symbol or file).
    pub source: String,
    pub outcome: Outcome,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub rule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionStats {
    pub files: u64,
    pub symbols: u64,
    pub external_packages: u64,
    pub edges: u64,
    pub edges_by_kind: BTreeMap<String, u64>,
    pub edges_by_confidence: BTreeMap<String, u64>,
    pub references: u64,
    pub references_by_outcome: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub nodes: Vec<NodeRecord>,
    pub edges: Vec<EdgeRecord>,
    pub references: Vec<ReferenceRecord>,
    pub stats: ResolutionStats,
}
