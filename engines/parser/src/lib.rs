//! USIE universal parsing engine.
//!
//! Converts source text into the normalized [`usie_ir::FileIr`] using
//! tree-sitter grammars and one extraction adapter per language family.
//!
//! The engine only *parses*. It never executes, imports or evaluates the
//! code it is given, and it treats all input as untrusted: parsing is bounded
//! by a time budget, extraction by depth and count limits, and malformed
//! input yields diagnostics rather than failures.

mod common;
mod ecmascript;
mod python;

pub use common::Limits;

use std::ops::ControlFlow;
use std::time::{Duration, Instant};
use usie_ir::{FileIr, Language, ParseStats, ParserInfo, IR_SCHEMA_VERSION};

/// Version of the extraction logic. Bump whenever an adapter changes the IR
/// it produces for the same input, so cached analyses are invalidated.
pub const EXTRACTOR_VERSION: &str = "1";

#[derive(Debug, Clone)]
pub struct ParseOptions {
    /// Wall-clock budget for the tree-sitter parse of one file.
    pub timeout: Duration,
    pub limits: Limits,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            timeout: Duration::from_secs(10),
            limits: Limits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The grammar could not be loaded (ABI mismatch). A build defect.
    Grammar(String),
    /// The parse exceeded its time budget.
    Timeout,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Grammar(m) => write!(f, "grammar error: {m}"),
            ParseError::Timeout => write!(f, "parse timed out"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Identity of the parser stack for a language; part of the cache key.
pub fn parser_info(language: Language) -> ParserInfo {
    let grammar = match language {
        Language::Python => "tree-sitter-python@0.25",
        Language::Javascript => "tree-sitter-javascript@0.25",
        Language::Typescript => "tree-sitter-typescript@0.23",
        Language::Tsx => "tree-sitter-tsx@0.23",
    };
    ParserInfo {
        name: grammar.to_string(),
        version: format!("ir{IR_SCHEMA_VERSION}.x{EXTRACTOR_VERSION}"),
    }
}

/// A single string identifying everything that affects parser output.
pub fn parser_fingerprint(language: Language) -> String {
    let info = parser_info(language);
    format!("{}+{}", info.name, info.version)
}

/// Parse one source file into normalized IR.
///
/// Syntax errors are not failures: the returned IR carries diagnostics and
/// whatever structure tree-sitter's error recovery could establish.
pub fn parse(
    language: Language,
    source: &[u8],
    options: &ParseOptions,
) -> Result<FileIr, ParseError> {
    let ts_language: tree_sitter::Language = match language {
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Javascript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Typescript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
    };
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|e| ParseError::Grammar(e.to_string()))?;

    let started = Instant::now();
    let deadline = started + options.timeout;
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let ts_options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
    let len = source.len();
    let tree = parser
        .parse_with_options(
            &mut |offset: usize, _| {
                if offset < len {
                    &source[offset..]
                } else {
                    &[][..]
                }
            },
            None,
            Some(ts_options),
        )
        .ok_or(ParseError::Timeout)?;
    let parse_micros = started.elapsed().as_micros() as u64;

    let extract_started = Instant::now();
    let root = tree.root_node();
    let (diagnostics, diagnostics_total) = common::collect_diagnostics(root, source);
    let extracted = match language {
        Language::Python => python::extract(root, source, options.limits),
        Language::Javascript | Language::Typescript | Language::Tsx => {
            ecmascript::extract(root, source, language, options.limits)
        }
    };

    Ok(FileIr {
        schema_version: IR_SCHEMA_VERSION,
        language,
        parser: parser_info(language),
        symbols: extracted.symbols,
        imports: extracted.imports,
        exports: extracted.exports,
        references: extracted.references,
        diagnostics,
        stats: ParseStats {
            nodes: extracted.nodes,
            diagnostics_total,
            parse_micros,
            extract_micros: extract_started.elapsed().as_micros() as u64,
        },
        attrs: extracted.attrs,
    })
}
