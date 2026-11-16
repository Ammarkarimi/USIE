//! Code quality and security rules evaluated on the syntax tree.
//!
//! Every rule is a syntactic pattern match over one file. Rules do not see
//! types, other files or runtime values, so a finding means "this code has
//! the shape the rule describes", not "this code is exploitable". The
//! catalog returned by [`rules`] states what each rule matches; the UI and
//! API show it next to every finding.
//!
//! The walk is bounded by depth and by the number of findings per file.

use crate::common::{children, collapse_ws, named_children, span};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;
use usie_ir::{CodeMetrics, Finding, Language, Span};

/// Version of the rule set. Part of the parser fingerprint, so changing a
/// rule invalidates cached analyses.
pub const CHECKS_VERSION: &str = "1";

pub const COMPLEXITY_LIMIT: u32 = 15;
pub const LENGTH_LIMIT: u32 = 100;
pub const PARAMETER_LIMIT: usize = 7;
pub const NESTING_LIMIT: u32 = 4;

const MAX_FINDINGS: usize = 2_000;
const MAX_DEPTH: u32 = 400;

/// A rule of the catalog.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub id: &'static str,
    pub name: &'static str,
    /// `bug`, `vulnerability`, `hotspot` (security-sensitive code to review)
    /// or `smell` (maintainability).
    pub category: &'static str,
    /// `blocker`, `critical`, `major`, `minor` or `info`.
    pub severity: &'static str,
    pub languages: &'static [&'static str],
    /// What the rule matches, including its known blind spots.
    pub description: &'static str,
    pub remediation: &'static str,
}

const ALL: &[&str] = &[
    "python",
    "javascript",
    "typescript",
    "tsx",
    "go",
    "java",
    "csharp",
    "kotlin",
    "rust",
    "c",
    "cpp",
];
const JS: &[&str] = &["javascript", "typescript", "tsx"];

/// The engine's rule catalog, in a stable order.
pub fn rules() -> &'static [Rule] {
    RULES
}

static RULES: &[Rule] = &[
    Rule {
        id: "function-complexity",
        name: "Functions should not be too complex",
        category: "smell",
        severity: "major",
        languages: ALL,
        description: "Cyclomatic complexity above 15: one plus the number of branches (if, loops, cases, catch clauses, conditional expressions and && / || / ?? operators) in a function. Anonymous functions that are not bound to a name count towards the enclosing function.",
        remediation: "Split the function into smaller functions with one purpose each, or replace branching with lookup tables or polymorphism.",
    },
    Rule {
        id: "function-length",
        name: "Functions should not be too long",
        category: "smell",
        severity: "minor",
        languages: ALL,
        description: "A function spans more than 100 lines, including blank lines and comments.",
        remediation: "Extract cohesive parts into well-named helper functions.",
    },
    Rule {
        id: "too-many-parameters",
        name: "Functions should not have too many parameters",
        category: "smell",
        severity: "minor",
        languages: ALL,
        description: "A function declares more than 7 parameters (self, this and C `void` are not counted).",
        remediation: "Group related parameters into a structure or options object.",
    },
    Rule {
        id: "deep-nesting",
        name: "Control flow should not be nested too deeply",
        category: "smell",
        severity: "minor",
        languages: ALL,
        description: "if, loop, switch/match and try statements nested more than 4 levels deep inside one function. `else if` chains do not add levels. Reported once per function, at the first statement that is too deep.",
        remediation: "Return early, invert conditions, or extract the inner blocks into functions.",
    },
    Rule {
        id: "todo-comment",
        name: "Track TODO and FIXME comments",
        category: "smell",
        severity: "info",
        languages: ALL,
        description: "A comment contains the word TODO, FIXME, XXX or HACK in upper case.",
        remediation: "Do the work, or move it to the issue tracker and remove the comment.",
    },
    Rule {
        id: "empty-catch",
        name: "Exceptions should not be silently ignored",
        category: "smell",
        severity: "major",
        languages: &["python", "javascript", "typescript", "tsx", "java", "csharp", "kotlin", "cpp"],
        description: "A catch / except block is empty (in Python: contains only `pass`). A block that contains a comment is treated as intentional and not reported.",
        remediation: "Handle the error, log it, or add a comment explaining why ignoring it is correct.",
    },
    Rule {
        id: "bare-except",
        name: "Bare except clauses should not be used",
        category: "smell",
        severity: "major",
        languages: &["python"],
        description: "An `except:` clause without an exception type also catches SystemExit and KeyboardInterrupt.",
        remediation: "Catch `Exception` or the specific exceptions the code can handle.",
    },
    Rule {
        id: "debugger-statement",
        name: "Debugger statements should be removed",
        category: "smell",
        severity: "major",
        languages: JS,
        description: "A `debugger` statement pauses execution whenever developer tools are open.",
        remediation: "Remove the statement.",
    },
    Rule {
        id: "loose-equality",
        name: "Strict equality operators should be used",
        category: "smell",
        severity: "minor",
        languages: JS,
        description: "`==` or `!=` performs type coercion. Comparisons with `null` are not reported, since `x == null` is a common way to test for null and undefined.",
        remediation: "Use `===` and `!==`.",
    },
    Rule {
        id: "self-comparison",
        name: "Identical expressions should not be on both sides of an operator",
        category: "bug",
        severity: "major",
        languages: ALL,
        description: "Both operands of a comparison, logical, subtraction or division operator are the same expression (after whitespace normalization). Operands containing calls or increments are not reported. `x != x` is a valid NaN test in some languages; it is still reported because an explicit isNaN call says what is meant.",
        remediation: "Fix the operand that was meant to differ, or simplify the expression.",
    },
    Rule {
        id: "identical-branches",
        name: "Both branches of a conditional should not be identical",
        category: "bug",
        severity: "major",
        languages: ALL,
        description: "An if/else with exactly two branches, or a conditional expression, has the same code (after whitespace normalization) in both branches.",
        remediation: "Fix the branch that was meant to differ, or remove the condition.",
    },
    Rule {
        id: "assignment-in-condition",
        name: "Assignments should not be used as conditions",
        category: "bug",
        severity: "major",
        languages: &["javascript", "typescript", "tsx", "c", "cpp"],
        description: "The condition of an if, while or do-while is an assignment, which is usually a typo for a comparison. Wrapping the assignment in an extra pair of parentheses marks it as intentional and is not reported.",
        remediation: "Use a comparison operator, or move the assignment out of the condition.",
    },
    Rule {
        id: "mutable-default-argument",
        name: "Default argument values should not be mutable",
        category: "bug",
        severity: "major",
        languages: &["python"],
        description: "A parameter defaults to a list, dict or set literal. The default is created once and shared by every call.",
        remediation: "Default to None and create the value inside the function.",
    },
    Rule {
        id: "unreachable-code",
        name: "Code after return, throw, break or continue is unreachable",
        category: "bug",
        severity: "major",
        languages: &["python", "javascript", "typescript", "tsx", "go", "java", "csharp", "c", "cpp"],
        description: "A statement follows a return, raise/throw, break or continue in the same block. Labels, hoisted function declarations, local functions and preprocessor directives are not reported.",
        remediation: "Remove the dead code, or fix the control flow that was meant to reach it.",
    },
    Rule {
        id: "sql-injection",
        name: "SQL queries should not be built from untrusted strings",
        category: "vulnerability",
        severity: "critical",
        languages: &["python", "javascript", "typescript", "tsx", "go", "java", "csharp", "kotlin", "rust"],
        description: "A database query method (execute, query, Query, executeQuery, rawQuery, ExecuteSqlRaw, ...) receives SQL that is built by concatenation, interpolation or string formatting, directly or through a local variable assigned in the same function. Only strings whose literal parts look like SQL are reported. Values that are constants or already validated are reported too, since the rule cannot see where they come from.",
        remediation: "Pass values as query parameters (placeholders such as ?, $1 or :name) instead of putting them in the SQL text.",
    },
    Rule {
        id: "command-execution",
        name: "OS commands built from non-constant values should be reviewed",
        category: "hotspot",
        severity: "critical",
        languages: &["python", "javascript", "typescript", "tsx", "go", "java", "csharp", "kotlin", "c", "cpp"],
        description: "A shell or process is started with a command that is not a constant string: os.system, os.popen and subprocess with shell=True (Python), child_process.exec/execSync (JavaScript), exec.Command through a shell or with a built command (Go), Runtime.exec (Java, Kotlin), Process.Start (C#), system and popen (C, C++).",
        remediation: "Avoid the shell: pass the program and its arguments separately, and validate any value that reaches the command against an allow-list.",
    },
    Rule {
        id: "dynamic-code-execution",
        name: "Dynamically executed code should be reviewed",
        category: "hotspot",
        severity: "critical",
        languages: &["python", "javascript", "typescript", "tsx"],
        description: "eval or exec with a non-constant argument (Python), eval, new Function, and setTimeout / setInterval with a string (JavaScript).",
        remediation: "Replace dynamic evaluation with explicit code paths, or parse the input with a safe parser (for example JSON).",
    },
    Rule {
        id: "weak-hash",
        name: "Weak hash algorithms should not be used for security",
        category: "hotspot",
        severity: "major",
        languages: ALL,
        description: "MD5 or SHA-1 is used (hashlib.md5/sha1, createHash('md5'), md5.New/Sum, MessageDigest.getInstance(\"MD5\"), MD5.Create, md5::compute, MD5()/SHA1() in C). Both are broken for signatures, passwords and integrity against an attacker; they are acceptable for non-security checksums. Python calls with usedforsecurity=False are not reported.",
        remediation: "Use SHA-256 or stronger for integrity, and a password hash (argon2, bcrypt, scrypt) for passwords.",
    },
    Rule {
        id: "tls-verification-disabled",
        name: "TLS certificate verification should not be disabled",
        category: "vulnerability",
        severity: "critical",
        languages: &["python", "javascript", "typescript", "tsx", "go", "csharp", "rust"],
        description: "Certificate or host name verification is turned off: verify=False, ssl._create_unverified_context and ssl.CERT_NONE (Python), rejectUnauthorized: false and NODE_TLS_REJECT_UNAUTHORIZED=0 (JavaScript), InsecureSkipVerify: true (Go), DangerousAcceptAnyServerCertificateValidator or a validation callback that returns true (C#), danger_accept_invalid_certs(true) (Rust).",
        remediation: "Keep verification on. For private certificate authorities, add the CA certificate to the trust store instead.",
    },
    Rule {
        id: "unsafe-deserialization",
        name: "Deserializing untrusted data should be reviewed",
        category: "hotspot",
        severity: "critical",
        languages: &["python", "java", "csharp", "kotlin"],
        description: "A deserializer that can run code or instantiate arbitrary types is used: pickle, marshal, dill, jsonpickle and yaml.load without a safe loader (Python), ObjectInputStream and XMLDecoder (Java, Kotlin), BinaryFormatter, SoapFormatter, NetDataContractSerializer, LosFormatter and ObjectStateFormatter (C#).",
        remediation: "Deserialize untrusted input only with data-only formats (JSON, yaml.safe_load) and explicit type allow-lists.",
    },
    Rule {
        id: "unbounded-string-copy",
        name: "Unbounded string functions should be reviewed",
        category: "hotspot",
        severity: "major",
        languages: &["c", "cpp"],
        description: "strcpy, strcat, sprintf, vsprintf, wcscpy, wcscat or gets is called. These do not check the destination size. gets cannot be used safely at all and was removed in C11.",
        remediation: "Use bounded alternatives (snprintf, strlcpy/strlcat or strncpy with explicit termination, fgets) or C++ strings.",
    },
    Rule {
        id: "unsafe-block",
        name: "Unsafe blocks should be reviewed",
        category: "hotspot",
        severity: "minor",
        languages: &["rust"],
        description: "An `unsafe` block disables some of the compiler's memory-safety checks.",
        remediation: "Keep unsafe blocks small, document the invariant that makes them sound in a SAFETY comment, and prefer safe abstractions.",
    },
];

/// Results of the checks for one file.
pub struct Checked {
    pub findings: Vec<Finding>,
    pub metrics: CodeMetrics,
}

pub fn run(root: Node, src: &[u8], language: Language) -> Checked {
    let mut c = Checker {
        src,
        lang: language,
        findings: Vec::new(),
        units: Vec::new(),
        scopes: vec![HashMap::new()],
        metrics: CodeMetrics::default(),
        constants: HashSet::new(),
    };
    c.collect_constants(root);
    c.visit(root, 0);
    Checked {
        findings: c.findings,
        metrics: c.metrics,
    }
}

struct Unit {
    name: String,
    complexity: u32,
    nesting: u32,
    deep_reported: bool,
    report: Span,
}

struct Checker<'s> {
    src: &'s [u8],
    lang: Language,
    findings: Vec<Finding>,
    units: Vec<Unit>,
    /// Per function: local names bound to a dynamically built SQL string.
    scopes: Vec<HashMap<String, ()>>,
    metrics: CodeMetrics,
    /// Names declared anywhere in the file as constants with a constant
    /// string value (`const X = "..."`, `static final`, `#define`, ...).
    constants: HashSet<String>,
}

/// A call or constructor invocation, normalized across grammars.
struct Call<'t> {
    /// Last segment of the callee (`execute` in `cursor.execute`).
    name: String,
    /// The callee without its last segment (`cursor`), empty for bare names.
    qual: String,
    args: Vec<Node<'t>>,
    kwargs: Vec<(String, Node<'t>)>,
    ctor: bool,
}

const COMMENT_KINDS: &[&str] = &["comment", "line_comment", "block_comment"];
const BLOCK_KINDS: &[&str] = &["block", "statement_block", "compound_statement"];

impl<'s> Checker<'s> {
    fn text(&self, n: Node) -> &'s str {
        n.utf8_text(self.src).unwrap_or("")
    }

    fn report(&mut self, rule: &str, at: Span, message: String, context: &str) {
        if self.findings.len() >= MAX_FINDINGS {
            return;
        }
        let function = self.units.last().map(|u| u.name.clone());
        self.findings.push(Finding {
            rule: rule.to_string(),
            span: at,
            message,
            function,
            context: fnv(&format!("{rule}\u{0}{context}")),
        });
    }

    fn report_node(&mut self, rule: &str, n: Node, message: String) {
        let context = collapse_ws(&truncate_bytes(self.text(n), 400));
        self.report(rule, span(n), message, &context);
    }

    fn visit(&mut self, node: Node, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        let kind = node.kind();

        let unit = self.enter_function(node);
        if unit.is_none() {
            self.count_decision(node);
        }
        let nests = self.is_nesting(node);
        if nests {
            self.enter_nesting(node);
        }
        self.check_node(node, kind);

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.visit(child, depth + 1);
        }

        if nests {
            if let Some(u) = self.units.last_mut() {
                u.nesting = u.nesting.saturating_sub(1);
            }
        }
        if unit.is_some() {
            self.leave_function();
        }
    }

    // ---- constants ----------------------------------------------------

    fn collect_constants(&mut self, root: Node) {
        let mut candidates: Vec<(String, Node)> = Vec::new();
        let mut stack = vec![(root, 0u32)];
        while let Some((n, depth)) = stack.pop() {
            if depth > MAX_DEPTH || candidates.len() > 10_000 {
                break;
            }
            if let Some(c) = self.constant_declaration(n) {
                candidates.push(c);
            } else if n.kind() == "preproc_def" {
                // `#define SQL "SELECT ..."`: the value is preprocessor text.
                let value = n
                    .child_by_field_name("value")
                    .map(|v| self.text(v).trim())
                    .unwrap_or("");
                if let Some(name) = n.child_by_field_name("name") {
                    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                        self.constants.insert(self.text(name).to_string());
                    }
                }
            }
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                stack.push((child, depth + 1));
            }
        }
        // Constants may be defined in terms of each other.
        for _ in 0..4 {
            let before = self.constants.len();
            for (name, value) in &candidates {
                if !self.constants.contains(name) && self.is_constant_string(*value) {
                    self.constants.insert(name.clone());
                }
            }
            if self.constants.len() == before {
                break;
            }
        }
    }

    /// `(name, value)` when `n` declares a constant.
    fn constant_declaration<'t>(&self, n: Node<'t>) -> Option<(String, Node<'t>)> {
        let name_value =
            |name: Option<Node<'t>>, value: Option<Node<'t>>| -> Option<(String, Node<'t>)> {
                let (name, value) = (name?, value?);
                let value = if value.kind() == "expression_list" {
                    let v = named_children(value);
                    if v.len() != 1 {
                        return None;
                    }
                    v[0]
                } else {
                    value
                };
                Some((self.text(name).to_string(), value))
            };
        // Text of the declaration before the declarator: `const`, `final`, ...
        let prefix_has = |decl: Node, at: Node, words: &[&str]| {
            let text = &self.src[decl.start_byte()..at.start_byte().max(decl.start_byte())];
            let text = std::str::from_utf8(text).unwrap_or("");
            words.iter().any(|w| has_word(text, w))
        };
        match n.kind() {
            "const_spec" => {
                let mut cursor = n.walk();
                let names: Vec<Node> = n.children_by_field_name("name", &mut cursor).collect();
                if names.len() != 1 {
                    return None;
                }
                name_value(Some(names[0]), n.child_by_field_name("value"))
            }
            "const_item" | "static_item" => name_value(
                n.child_by_field_name("name"),
                n.child_by_field_name("value"),
            ),
            "variable_declarator" | "init_declarator" => {
                let decl = n.parent()?;
                let decl = if decl.kind() == "variable_declaration" {
                    decl.parent().unwrap_or(decl)
                } else {
                    decl
                };
                if !prefix_has(decl, n, &["const", "final", "readonly"]) {
                    return None;
                }
                let name = n
                    .child_by_field_name("name")
                    .or_else(|| n.child_by_field_name("declarator"));
                let value = n
                    .child_by_field_name("value")
                    .or_else(|| named_children(n).into_iter().nth(1));
                name_value(name, value)
            }
            "property_declaration" => {
                let parts = named_children(n);
                let var = parts.iter().find(|c| c.kind() == "variable_declaration")?;
                if !prefix_has(n, *var, &["val", "const"]) {
                    return None;
                }
                name_value(
                    named_children(*var).into_iter().next(),
                    parts.last().copied(),
                )
            }
            "assignment" if self.lang == Language::Python => {
                let at_module = n
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|g| g.kind() == "module")
                    .unwrap_or(false);
                let left = n.child_by_field_name("left")?;
                let upper = self.text(left);
                let is_constant_name = !upper.is_empty()
                    && upper
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                (at_module && is_constant_name && left.kind() == "identifier")
                    .then(|| (upper.to_string(), n.child_by_field_name("right")))
                    .and_then(|(name, v)| Some((name, v?)))
            }
            _ => None,
        }
    }

    // ---- functions ----------------------------------------------------

    /// Starts a measured unit when `node` is a named function (or an
    /// anonymous function bound to a name) with a body.
    fn enter_function(&mut self, node: Node) -> Option<()> {
        let kind = node.kind();
        let named = matches!(
            kind,
            "function_definition"
                | "function_declaration"
                | "generator_function_declaration"
                | "method_definition"
                | "method_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration"
                | "local_function_statement"
                | "operator_declaration"
                | "destructor_declaration"
                | "secondary_constructor"
                | "function_item"
        );
        let anonymous = matches!(
            kind,
            "lambda"
                | "arrow_function"
                | "function_expression"
                | "function"
                | "generator_function"
                | "func_literal"
                | "lambda_expression"
                | "anonymous_method_expression"
                | "lambda_literal"
                | "anonymous_function"
                | "closure_expression"
        );
        if !named && !anonymous {
            return None;
        }
        let body = node.child_by_field_name("body").or_else(|| {
            named_children(node)
                .into_iter()
                .find(|c| c.kind() == "function_body")
        });
        // Lambdas without a block body (`x => x + 1`) still have a body field.
        body?;
        let (name, name_node) = if named {
            self.function_name(node)?
        } else {
            let (n, nn) = self.bound_name(node)?;
            (n, Some(nn))
        };

        let report = match name_node {
            Some(n) => span(n),
            None => first_line(node),
        };
        self.units.push(Unit {
            name: name.clone(),
            complexity: 1,
            nesting: 0,
            deep_reported: false,
            report,
        });
        self.scopes.push(HashMap::new());

        let lines = node.end_position().row - node.start_position().row + 1;
        if lines as u32 > LENGTH_LIMIT {
            self.report(
                "function-length",
                report,
                format!("`{name}` is {lines} lines long (the limit is {LENGTH_LIMIT})."),
                &name,
            );
        }
        let params = self.parameter_count(node);
        if params > PARAMETER_LIMIT {
            self.report(
                "too-many-parameters",
                report,
                format!("`{name}` has {params} parameters (the limit is {PARAMETER_LIMIT})."),
                &name,
            );
        }
        Some(())
    }

    fn leave_function(&mut self) {
        self.scopes.pop();
        let Some(u) = self.units.pop() else { return };
        self.metrics.functions += 1;
        self.metrics.complexity += u.complexity;
        self.metrics.max_complexity = self.metrics.max_complexity.max(u.complexity);
        if u.complexity > COMPLEXITY_LIMIT {
            let name = u.name.clone();
            // Report as if inside the function so `function` is set.
            self.units.push(u);
            let report = self.units.last().map(|u| u.report).unwrap_or_default();
            let complexity = self.units.last().map(|u| u.complexity).unwrap_or(0);
            self.report(
                "function-complexity",
                report,
                format!("`{name}` has a cyclomatic complexity of {complexity} (the limit is {COMPLEXITY_LIMIT})."),
                &name,
            );
            self.units.pop();
        }
    }

    fn function_name<'t>(&self, node: Node<'t>) -> Option<(String, Option<Node<'t>>)> {
        if node.kind() == "secondary_constructor" {
            return Some(("constructor".to_string(), None));
        }
        if let Some(n) = node.child_by_field_name("name") {
            return Some((self.text(n).to_string(), Some(n)));
        }
        // C and C++: the name is at the bottom of the declarator chain.
        let mut d = node.child_by_field_name("declarator")?;
        for _ in 0..16 {
            match d.child_by_field_name("declarator") {
                Some(inner) => d = inner,
                None => break,
            }
        }
        Some((collapse_ws(self.text(d)), Some(d)))
    }

    /// Name of the variable, field or key an anonymous function is assigned to.
    fn bound_name<'t>(&self, node: Node<'t>) -> Option<(String, Node<'t>)> {
        let parent = node.parent()?;
        let target = match parent.kind() {
            "variable_declarator" => parent.child_by_field_name("name"),
            "pair" => parent.child_by_field_name("key"),
            "assignment" | "assignment_expression" => parent.child_by_field_name("left"),
            "public_field_definition" | "field_definition" => parent
                .child_by_field_name("name")
                .or_else(|| parent.child_by_field_name("property")),
            "let_declaration" => parent.child_by_field_name("pattern"),
            "init_declarator" => parent.child_by_field_name("declarator"),
            "property_declaration" => named_children(parent)
                .into_iter()
                .find(|c| c.kind() == "variable_declaration")
                .and_then(|v| named_children(v).into_iter().next()),
            "expression_list" => {
                // Go: `f := func() {...}` with one name and one value.
                let decl = parent.parent()?;
                if !matches!(
                    decl.kind(),
                    "short_var_declaration" | "assignment_statement"
                ) {
                    return None;
                }
                let left = decl.child_by_field_name("left")?;
                let names = named_children(left);
                if names.len() != 1 || named_children(parent).len() != 1 {
                    return None;
                }
                Some(names[0])
            }
            _ => None,
        }?;
        // A function is only bound when it is the assigned value itself.
        if target.id() == node.id() {
            return None;
        }
        let name = self.text(target);
        if name.is_empty() || name.len() > 120 || name.contains(char::is_whitespace) {
            return None;
        }
        Some((
            name.trim_matches(|c| c == '"' || c == '\'').to_string(),
            target,
        ))
    }

    fn parameter_count(&self, node: Node) -> usize {
        let params = node
            .child_by_field_name("parameters")
            .or_else(|| {
                named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "function_value_parameters")
            })
            .or_else(|| {
                // C/C++: parameters belong to the function declarator.
                let mut d = node.child_by_field_name("declarator")?;
                for _ in 0..16 {
                    if d.kind() == "function_declarator" {
                        return d.child_by_field_name("parameters");
                    }
                    d = d.child_by_field_name("declarator")?;
                }
                None
            });
        let Some(params) = params else {
            // `x => ...` has a single `parameter` field.
            return usize::from(node.child_by_field_name("parameter").is_some());
        };
        let mut count = 0;
        for p in named_children(params) {
            let text = self.text(p);
            match p.kind() {
                k if COMMENT_KINDS.contains(&k) => {}
                "self_parameter"
                | "keyword_separator"
                | "positional_separator"
                | "variadic_parameter" => {}
                "identifier" if text == "self" || text == "cls" => {}
                "parameter_declaration" if text.trim() == "void" => {}
                "parameter_declaration" => {
                    // Go: `a, b int` declares two parameters.
                    let mut cursor = p.walk();
                    let names = p.children_by_field_name("name", &mut cursor).count();
                    count += names.max(1);
                }
                _ if self.lang.family() == usie_ir::LanguageFamily::Ecmascript
                    && (text == "this"
                        || text.starts_with("this:")
                        || text.starts_with("this ")) => {}
                _ => count += 1,
            }
        }
        count
    }

    // ---- complexity and nesting ----------------------------------------

    fn count_decision(&mut self, node: Node) {
        if self.units.is_empty() {
            return;
        }
        let add = match node.kind() {
            "if_statement"
            | "if_expression"
            | "elif_clause"
            | "if_clause"
            | "for_statement"
            | "for_in_statement"
            | "for_in_clause"
            | "enhanced_for_statement"
            | "foreach_statement"
            | "for_expression"
            | "for_range_loop"
            | "while_statement"
            | "while_expression"
            | "do_statement"
            | "do_while_statement"
            | "loop_expression"
            | "catch_clause"
            | "except_clause"
            | "catch_block"
            | "ternary_expression"
            | "conditional_expression"
            | "case_clause"
            | "switch_case"
            | "expression_case"
            | "type_case"
            | "communication_case" => true,
            "case_statement" => node.child_by_field_name("value").is_some(),
            "switch_label" | "switch_rule" | "switch_section" => {
                !self.text(node).trim_start().starts_with("default")
            }
            "switch_expression_arm" | "match_arm" => {
                let pattern = node
                    .child_by_field_name("pattern")
                    .or_else(|| named_children(node).into_iter().next());
                pattern.map(|p| self.text(p).trim() != "_").unwrap_or(true)
            }
            "when_entry" => node.child_by_field_name("condition").is_some(),
            "binary_expression" | "boolean_operator" => {
                matches!(
                    self.operator(node),
                    "&&" | "||" | "and" | "or" | "??" | "?:"
                )
            }
            _ => false,
        };
        if add {
            if let Some(u) = self.units.last_mut() {
                u.complexity += 1;
            }
        }
    }

    fn is_nesting(&self, node: Node) -> bool {
        let nests = matches!(
            node.kind(),
            "if_statement"
                | "if_expression"
                | "for_statement"
                | "for_in_statement"
                | "enhanced_for_statement"
                | "foreach_statement"
                | "for_expression"
                | "for_range_loop"
                | "while_statement"
                | "while_expression"
                | "do_statement"
                | "do_while_statement"
                | "loop_expression"
                | "switch_statement"
                | "expression_switch_statement"
                | "type_switch_statement"
                | "select_statement"
                | "switch_expression"
                | "when_expression"
                | "match_expression"
                | "match_statement"
                | "try_statement"
                | "try_expression"
        );
        if !nests || self.units.is_empty() {
            return false;
        }
        // `else if` continues a chain instead of nesting.
        if matches!(node.kind(), "if_statement" | "if_expression") {
            if let Some(p) = node.parent() {
                if matches!(p.kind(), "else_clause" | "if_statement" | "if_expression") {
                    return false;
                }
            }
        }
        true
    }

    fn enter_nesting(&mut self, node: Node) {
        let Some(u) = self.units.last_mut() else {
            return;
        };
        u.nesting += 1;
        if u.nesting > NESTING_LIMIT && !u.deep_reported {
            u.deep_reported = true;
            let (name, depth) = (u.name.clone(), u.nesting);
            self.report(
                "deep-nesting",
                first_line(node),
                format!("Control flow is nested {depth} levels deep in `{name}` (the limit is {NESTING_LIMIT})."),
                &name,
            );
        }
    }

    // ---- node rules ----------------------------------------------------

    fn check_node(&mut self, node: Node, kind: &str) {
        match kind {
            k if COMMENT_KINDS.contains(&k) => self.check_comment(node),
            "catch_clause" | "catch_block" | "except_clause" => self.check_catch(node),
            "debugger_statement" => self.report_node(
                "debugger-statement",
                node,
                "Remove this debugger statement.".into(),
            ),
            "binary_expression"
            | "comparison_operator"
            | "boolean_operator"
            | "binary_operator" => self.check_binary(node),
            "if_statement" | "if_expression" | "ternary_expression" | "conditional_expression" => {
                self.check_branches(node);
                self.check_assignment_condition(node);
            }
            "while_statement" | "do_statement" => self.check_assignment_condition(node),
            "default_parameter" | "typed_default_parameter" => self.check_default(node),
            "unsafe_block" => self.report(
                "unsafe-block",
                first_line(node),
                "Review this unsafe block: the compiler does not check its memory safety.".into(),
                &collapse_ws(&truncate_bytes(self.text(node), 400)),
            ),
            "pair"
            | "keyed_element"
            | "assignment_expression"
            | "assignment_statement"
            | "assignment" => {
                self.check_tls_setting(node);
                self.record_binding(node);
            }
            "variable_declarator"
            | "short_var_declaration"
            | "let_declaration"
            | "init_declarator"
            | "property_declaration"
            | "var_spec" => self.record_binding(node),
            "member_access_expression" | "attribute" => self.check_member(node),
            _ => {}
        }
        if BLOCK_KINDS.contains(&kind)
            || matches!(
                kind,
                "statement_list"
                    | "case_statement"
                    | "switch_section"
                    | "switch_case"
                    | "switch_block_statement_group"
                    | "expression_case"
                    | "default_case"
                    | "type_case"
            )
        {
            self.check_unreachable(node);
        }
        if let Some(call) = self.call(node) {
            self.check_call(node, &call);
        }
    }

    fn check_comment(&mut self, node: Node) {
        let text = self.text(node);
        let marker = ["TODO", "FIXME", "XXX", "HACK"]
            .into_iter()
            .find(|m| has_word(text, m));
        if let Some(m) = marker {
            let line = text
                .lines()
                .find(|l| has_word(l, m))
                .unwrap_or("")
                .trim()
                .trim_start_matches(['/', '*', '#', '-', ' '])
                .trim();
            let shown: String = line.chars().take(100).collect();
            self.report_node("todo-comment", node, format!("{m} comment: {shown}"));
        }
    }

    fn check_catch(&mut self, node: Node) {
        let body = node.child_by_field_name("body").or_else(|| {
            named_children(node)
                .into_iter()
                .rev()
                .find(|c| BLOCK_KINDS.contains(&c.kind()))
        });
        if node.kind() == "except_clause" {
            let typed = named_children(node)
                .iter()
                .any(|c| !BLOCK_KINDS.contains(&c.kind()) && !COMMENT_KINDS.contains(&c.kind()));
            if !typed {
                self.report(
                    "bare-except",
                    first_line(node),
                    "Catch a specific exception type instead of using a bare `except:`.".into(),
                    &collapse_ws(&truncate_bytes(self.text(node), 400)),
                );
            }
        }
        let Some(body) = body else { return };
        let inner = named_children(body);
        let empty = if node.kind() == "except_clause" {
            !inner.is_empty() && inner.iter().all(|c| c.kind() == "pass_statement")
        } else {
            inner.is_empty()
        };
        if empty {
            self.report(
                "empty-catch",
                first_line(node),
                "This handler ignores the exception. Handle or log it, or explain in a comment why it is safe to ignore.".into(),
                &collapse_ws(&truncate_bytes(self.text(node), 400)),
            );
        }
    }

    fn operator(&self, node: Node) -> &'s str {
        if let Some(op) = node.child_by_field_name("operator") {
            return self.text(op);
        }
        for c in children(node) {
            if !c.is_named() {
                return self.text(c);
            }
        }
        ""
    }

    fn operands<'t>(&self, node: Node<'t>) -> Option<(Node<'t>, Node<'t>)> {
        if let (Some(l), Some(r)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            return Some((l, r));
        }
        let ops: Vec<Node> = named_children(node)
            .into_iter()
            .filter(|c| !COMMENT_KINDS.contains(&c.kind()))
            .collect();
        if ops.len() == 2 {
            Some((ops[0], ops[1]))
        } else {
            None
        }
    }

    fn check_binary(&mut self, node: Node) {
        let op = self.operator(node);
        let Some((left, right)) = self.operands(node) else {
            return;
        };
        let family = self.lang.family();
        if family == usie_ir::LanguageFamily::Ecmascript && (op == "==" || op == "!=") {
            let (l, r) = (self.text(left).trim(), self.text(right).trim());
            if l != "null" && r != "null" {
                self.report_node(
                    "loose-equality",
                    node,
                    format!(
                        "Use `{op}=` instead of `{op}`; `{op}` converts types before comparing."
                    ),
                );
            }
        }
        if matches!(
            op,
            "==" | "!="
                | "==="
                | "!=="
                | "<"
                | ">"
                | "<="
                | ">="
                | "&&"
                | "||"
                | "and"
                | "or"
                | "-"
                | "/"
        ) {
            let (l, r) = (collapse_ws(self.text(left)), collapse_ws(self.text(right)));
            let side_effect = |s: &str| s.contains('(') || s.contains("++") || s.contains("--");
            if !l.is_empty() && l == r && !side_effect(&l) {
                self.report_node(
                    "self-comparison",
                    node,
                    format!("Both sides of `{op}` are `{}`.", shorten(&l, 60)),
                );
            }
        }
    }

    fn check_branches(&mut self, node: Node) {
        let kind = node.kind();
        let (cons, alt) = if kind == "conditional_expression" && self.lang == Language::Python {
            let parts = named_children(node);
            if parts.len() != 3 {
                return;
            }
            (parts[0], parts[2])
        } else if kind == "if_expression" && self.lang == Language::Kotlin {
            let blocks: Vec<Node> = named_children(node)
                .into_iter()
                .filter(|c| c.kind() == "block")
                .collect();
            if blocks.len() != 2 {
                return;
            }
            (blocks[0], blocks[1])
        } else {
            let (Some(cons), Some(alt)) = (
                node.child_by_field_name("consequence"),
                node.child_by_field_name("alternative"),
            ) else {
                return;
            };
            let mut cursor = node.walk();
            if node
                .children_by_field_name("alternative", &mut cursor)
                .count()
                != 1
            {
                return; // Python if/elif/else
            }
            let alt = if alt.kind() == "else_clause" {
                match alt.child_by_field_name("body").or_else(|| {
                    named_children(alt)
                        .into_iter()
                        .find(|c| !COMMENT_KINDS.contains(&c.kind()))
                }) {
                    Some(b) => b,
                    None => return,
                }
            } else {
                alt
            };
            let is_conditional = matches!(kind, "ternary_expression" | "conditional_expression");
            if !is_conditional
                && !(BLOCK_KINDS.contains(&alt.kind()) && BLOCK_KINDS.contains(&cons.kind()))
            {
                return;
            }
            (cons, alt)
        };
        let (a, b) = (collapse_ws(self.text(cons)), collapse_ws(self.text(alt)));
        let trivial = a
            .trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
            .is_empty();
        if a == b && !trivial {
            self.report(
                "identical-branches",
                first_line(node),
                "Both branches of this conditional contain the same code.".into(),
                &collapse_ws(&truncate_bytes(self.text(node), 400)),
            );
        }
    }

    fn check_assignment_condition(&mut self, node: Node) {
        if !matches!(
            self.lang,
            Language::Javascript
                | Language::Typescript
                | Language::Tsx
                | Language::C
                | Language::Cpp
        ) {
            return;
        }
        let Some(cond) = node.child_by_field_name("condition") else {
            return;
        };
        let inner = if cond.kind() == "parenthesized_expression" {
            named_children(cond).into_iter().next()
        } else {
            Some(cond)
        };
        if let Some(inner) = inner {
            if inner.kind() == "assignment_expression" {
                self.report_node(
                    "assignment-in-condition",
                    inner,
                    "This condition is an assignment. Use a comparison, or wrap the assignment in extra parentheses if it is intended.".into(),
                );
            }
        }
    }

    fn check_default(&mut self, node: Node) {
        if self.lang != Language::Python {
            return;
        }
        let Some(value) = node.child_by_field_name("value") else {
            return;
        };
        if matches!(
            value.kind(),
            "list"
                | "dictionary"
                | "set"
                | "list_comprehension"
                | "dictionary_comprehension"
                | "set_comprehension"
        ) {
            let name = node
                .child_by_field_name("name")
                .map(|n| self.text(n).to_string())
                .unwrap_or_default();
            self.report_node(
                "mutable-default-argument",
                node,
                format!("The default of `{name}` is created once and shared between calls. Default to None instead."),
            );
        }
    }

    fn check_unreachable(&mut self, node: Node) {
        if matches!(self.lang, Language::Rust | Language::Kotlin) {
            return;
        }
        let mut jumped: Option<&str> = None;
        for child in named_children(node) {
            let k = child.kind();
            if let Some(jump) = jumped {
                let ignored = COMMENT_KINDS.contains(&k)
                    || k.starts_with("preproc")
                    || matches!(
                        k,
                        "labeled_statement"
                            | "function_declaration"
                            | "generator_function_declaration"
                            | "class_declaration"
                            | "local_function_statement"
                            | "type_alias_declaration"
                            | "interface_declaration"
                            | "empty_statement"
                            | "switch_case"
                            | "switch_default"
                            | "case_statement"
                            | "expression_case"
                            | "default_case"
                            | "type_case"
                            | "communication_case"
                            | "switch_label"
                            | "switch_section"
                            | "switch_block_statement_group"
                            | "fallthrough_statement"
                    );
                if ignored {
                    continue;
                }
                self.report(
                    "unreachable-code",
                    first_line(child),
                    format!("This code can never run: it follows a `{jump}`."),
                    &collapse_ws(&truncate_bytes(self.text(child), 400)),
                );
                return;
            }
            jumped = match k {
                "return_statement" => Some("return"),
                "throw_statement" => Some("throw"),
                "raise_statement" => Some("raise"),
                "break_statement" => Some("break"),
                "continue_statement" => Some("continue"),
                _ => None,
            };
        }
    }

    fn check_member(&mut self, node: Node) {
        let text = self.text(node);
        match self.lang {
            Language::Csharp if text.ends_with("DangerousAcceptAnyServerCertificateValidator") => {
                self.report_node(
                    "tls-verification-disabled",
                    node,
                    "This handler accepts any server certificate.".into(),
                );
            }
            Language::Python if collapse_ws(text) == "ssl.CERT_NONE" => {
                self.report_node(
                    "tls-verification-disabled",
                    node,
                    "ssl.CERT_NONE disables certificate verification.".into(),
                );
            }
            _ => {}
        }
    }

    fn check_tls_setting(&mut self, node: Node) {
        let (key, value) = match node.kind() {
            "pair" => (
                node.child_by_field_name("key"),
                node.child_by_field_name("value"),
            ),
            "keyed_element" => {
                let parts = named_children(node);
                (parts.first().copied(), parts.get(1).copied())
            }
            _ => (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ),
        };
        let (Some(key), Some(value)) = (key, value) else {
            return;
        };
        let key = self.text(key);
        let value = self.text(value).trim();
        // `a.b.KEY`, `env["KEY"]` and `"KEY"` all name KEY.
        let last = key
            .split(['.', '[', ']', '"', '\'', ' '])
            .rfind(|p| !p.is_empty())
            .unwrap_or("");
        let disabled = match last {
            "rejectUnauthorized" => value == "false",
            "NODE_TLS_REJECT_UNAUTHORIZED" => value.trim_matches(|c| c == '"' || c == '\'') == "0",
            "InsecureSkipVerify" => value == "true",
            "ServerCertificateCustomValidationCallback" | "ServerCertificateValidationCallback" => {
                // `(...) => true`
                value.contains("=>") && value.rsplit("=>").next().map(str::trim) == Some("true")
            }
            _ => false,
        };
        if disabled {
            self.report_node(
                "tls-verification-disabled",
                node,
                format!(
                    "`{}` disables TLS certificate verification.",
                    shorten(&collapse_ws(self.text(node)), 80)
                ),
            );
        }
    }

    // ---- bindings ------------------------------------------------------

    /// Remembers local names assigned a dynamically built SQL string, so a
    /// later `execute(query)` in the same function is recognized.
    fn record_binding(&mut self, node: Node) {
        let (name, value) = match node.kind() {
            "variable_declarator" => (
                node.child_by_field_name("name"),
                node.child_by_field_name("value")
                    .or_else(|| named_children(node).into_iter().nth(1)),
            ),
            "assignment" | "assignment_expression" => (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ),
            "let_declaration" => (
                node.child_by_field_name("pattern"),
                node.child_by_field_name("value"),
            ),
            "init_declarator" => (
                node.child_by_field_name("declarator"),
                node.child_by_field_name("value"),
            ),
            "short_var_declaration" | "assignment_statement" | "var_spec" => {
                let left = node
                    .child_by_field_name("left")
                    .or_else(|| node.child_by_field_name("name"));
                let right = node
                    .child_by_field_name("right")
                    .or_else(|| node.child_by_field_name("value"));
                match (left, right) {
                    (Some(l), Some(r)) => {
                        let (ln, rn) = (named_children(l), named_children(r));
                        if l.kind() == "expression_list" && (ln.len() != 1 || rn.len() != 1) {
                            return;
                        }
                        (
                            Some(if l.kind() == "expression_list" {
                                ln[0]
                            } else {
                                l
                            }),
                            Some(if r.kind() == "expression_list" {
                                rn[0]
                            } else {
                                r
                            }),
                        )
                    }
                    _ => return,
                }
            }
            "property_declaration" => {
                let parts = named_children(node);
                let name = parts
                    .iter()
                    .find(|c| c.kind() == "variable_declaration")
                    .and_then(|v| named_children(*v).into_iter().next());
                (name, parts.last().copied())
            }
            _ => return,
        };
        let (Some(name), Some(value)) = (name, value) else {
            return;
        };
        if !matches!(name.kind(), "identifier" | "simple_identifier") {
            return;
        }
        let name = self.text(name).to_string();
        if self.dynamic_sql(value).is_some() {
            if let Some(s) = self.scopes.last_mut() {
                s.insert(name, ());
            }
        } else if let Some(s) = self.scopes.last_mut() {
            s.remove(&name);
        }
    }

    // ---- calls ---------------------------------------------------------

    fn call<'t>(&self, node: Node<'t>) -> Option<Call<'t>> {
        let kind = node.kind();
        let (callee, args_node, ctor): (Option<Node>, Option<Node>, bool) = match kind {
            "call" | "call_expression" | "invocation_expression" => {
                if self.lang == Language::Kotlin {
                    let parts = named_children(node);
                    (
                        parts.first().copied(),
                        parts
                            .iter()
                            .find(|c| c.kind() == "value_arguments")
                            .copied(),
                        false,
                    )
                } else {
                    (
                        node.child_by_field_name("function"),
                        node.child_by_field_name("arguments"),
                        false,
                    )
                }
            }
            "method_invocation" => {
                let name = node.child_by_field_name("name")?;
                let qual = node
                    .child_by_field_name("object")
                    .map(|o| self.text(o).to_string())
                    .unwrap_or_default();
                let (args, kwargs) = self.arguments(node.child_by_field_name("arguments"));
                return Some(Call {
                    name: self.text(name).to_string(),
                    qual,
                    args,
                    kwargs,
                    ctor: false,
                });
            }
            "new_expression" => (
                node.child_by_field_name("constructor"),
                node.child_by_field_name("arguments"),
                true,
            ),
            "object_creation_expression" => (
                node.child_by_field_name("type"),
                node.child_by_field_name("arguments"),
                true,
            ),
            "macro_invocation" => {
                let name = node.child_by_field_name("macro")?;
                return Some(Call {
                    name: self.text(name).to_string(),
                    qual: String::new(),
                    args: Vec::new(),
                    kwargs: Vec::new(),
                    ctor: false,
                });
            }
            _ => return None,
        };
        let callee = callee?;
        let (name, qual) = self.split_callee(callee);
        let (args, kwargs) = self.arguments(args_node);
        Some(Call {
            name,
            qual,
            args,
            kwargs,
            ctor,
        })
    }

    fn split_callee(&self, callee: Node) -> (String, String) {
        let pair = |q: Option<Node>, n: Option<Node>| {
            (
                n.map(|n| self.text(n).to_string()).unwrap_or_default(),
                q.map(|q| collapse_ws(self.text(q))).unwrap_or_default(),
            )
        };
        match callee.kind() {
            "attribute" => pair(
                callee.child_by_field_name("object"),
                callee.child_by_field_name("attribute"),
            ),
            "member_expression" => pair(
                callee.child_by_field_name("object"),
                callee.child_by_field_name("property"),
            ),
            "selector_expression" => pair(
                callee.child_by_field_name("operand"),
                callee.child_by_field_name("field"),
            ),
            "member_access_expression" => pair(
                callee.child_by_field_name("expression"),
                callee.child_by_field_name("name"),
            ),
            "field_expression" => pair(
                callee
                    .child_by_field_name("value")
                    .or_else(|| callee.child_by_field_name("argument")),
                callee.child_by_field_name("field"),
            ),
            "scoped_identifier" | "qualified_identifier" => pair(
                callee
                    .child_by_field_name("path")
                    .or_else(|| callee.child_by_field_name("scope")),
                callee.child_by_field_name("name"),
            ),
            "navigation_expression" => {
                let parts = named_children(callee);
                pair(parts.first().copied(), parts.last().copied())
            }
            _ => {
                let text = collapse_ws(self.text(callee));
                // Generic and qualified type names: `new java.io.ObjectInputStream`.
                let base = text.split('<').next().unwrap_or("").trim().to_string();
                match base.rsplit_once('.') {
                    Some((q, n)) => (n.to_string(), q.to_string()),
                    None => (base, String::new()),
                }
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn arguments<'t>(&self, args: Option<Node<'t>>) -> (Vec<Node<'t>>, Vec<(String, Node<'t>)>) {
        let mut positional = Vec::new();
        let mut keyword = Vec::new();
        let Some(args) = args else {
            return (positional, keyword);
        };
        for a in named_children(args) {
            match a.kind() {
                k if COMMENT_KINDS.contains(&k) => {}
                "keyword_argument" => {
                    if let (Some(n), Some(v)) = (
                        a.child_by_field_name("name"),
                        a.child_by_field_name("value"),
                    ) {
                        keyword.push((self.text(n).to_string(), v));
                    }
                }
                "list_splat" | "dictionary_splat" | "spread_element" => {}
                "argument" | "value_argument" => {
                    let parts: Vec<Node> = named_children(a)
                        .into_iter()
                        .filter(|c| !COMMENT_KINDS.contains(&c.kind()))
                        .collect();
                    let named = parts.len() >= 2
                        && (parts[0].kind() == "name_colon"
                            || (parts[0].kind() == "identifier"
                                && children(a).iter().any(|c| self.text(*c) == "=")));
                    match (named, parts.last()) {
                        (true, Some(v)) => {
                            let n = self.text(parts[0]).trim_end_matches(':').trim().to_string();
                            keyword.push((n, *v));
                        }
                        (false, Some(v)) => positional.push(*v),
                        _ => {}
                    }
                }
                _ => positional.push(a),
            }
        }
        (positional, keyword)
    }

    fn check_call(&mut self, node: Node, call: &Call) {
        let name = call.name.as_str();
        let qual = call.qual.as_str();
        let first = call.args.first().copied();
        let lang = self.lang;
        let family = lang.family();
        use usie_ir::LanguageFamily as F;

        // SQL injection.
        if (SQL_SINKS.contains(&name) && !call.ctor)
            || (call.ctor && SQL_COMMAND_TYPES.contains(&name))
        {
            for arg in call.args.iter().chain(call.kwargs.iter().map(|(_, v)| v)) {
                if let Some(how) = self.sql_argument(*arg) {
                    self.report_node(
                        "sql-injection",
                        node,
                        format!(
                            "`{}` receives SQL {how}. Use query parameters instead.",
                            shorten(&self.callee_text(call), 60)
                        ),
                    );
                    break;
                }
            }
        }

        // OS command execution.
        let command = match family {
            F::Python => {
                let shell = call
                    .kwargs
                    .iter()
                    .any(|(k, v)| k == "shell" && self.text(*v) == "True");
                ((qual == "os" && matches!(name, "system" | "popen"))
                    || (qual == "commands" && name.starts_with("getoutput"))
                    || (shell
                        && (qual == "subprocess" || qual.is_empty())
                        && matches!(
                            name,
                            "Popen" | "run" | "call" | "check_output" | "check_call"
                        )))
                .then_some(first)
                .flatten()
                .filter(|a| !self.is_constant_string(*a))
            }
            F::Ecmascript => (matches!(name, "exec" | "execSync")
                && (matches!(qual, "child_process" | "cp" | "childProcess") || qual.is_empty()))
            .then_some(first)
            .flatten()
            .filter(|a| {
                self.dynamic_string(*a).is_some()
                    || (!qual.is_empty() && !self.is_constant_string(*a))
            }),
            F::Go if qual == "exec" && matches!(name, "Command" | "CommandContext") => {
                let offset = usize::from(name == "CommandContext");
                let program = call.args.get(offset).copied();
                let through_shell = program
                    .and_then(|p| self.literal_value(p))
                    .map(|p| {
                        matches!(
                            p.as_str(),
                            "sh" | "bash"
                                | "zsh"
                                | "/bin/sh"
                                | "/bin/bash"
                                | "cmd"
                                | "cmd.exe"
                                | "powershell"
                        )
                    })
                    .unwrap_or(false);
                if through_shell {
                    call.args
                        .iter()
                        .skip(offset + 1)
                        .copied()
                        .find(|a| !self.is_constant_string(*a))
                } else {
                    program.filter(|p| self.dynamic_string(*p).is_some())
                }
            }
            F::Java | F::Kotlin => (name == "exec" && qual.ends_with("getRuntime()"))
                .then_some(first)
                .flatten()
                .filter(|a| !self.is_constant_string(*a)),
            F::Csharp => (((qual == "Process" || qual.ends_with(".Process")) && name == "Start")
                || (call.ctor && name == "ProcessStartInfo"))
                .then_some(first)
                .flatten()
                .filter(|a| !self.is_constant_string(*a)),
            F::C => ((qual.is_empty() || qual == "std")
                && matches!(name, "system" | "popen" | "_popen" | "_wsystem"))
            .then_some(first)
            .flatten()
            .filter(|a| !self.is_constant_string(*a)),
            _ => None,
        };
        if command.is_some() {
            self.report_node(
                "command-execution",
                node,
                format!("`{}` runs a command that is not a constant string. Make sure no untrusted value can change it.", shorten(&self.callee_text(call), 60)),
            );
        }

        // Dynamic code execution.
        let dynamic = match family {
            F::Python => {
                matches!(name, "eval" | "exec")
                    && (qual.is_empty() || qual == "builtins")
                    && first.map(|a| !self.is_constant_string(a)).unwrap_or(false)
            }
            F::Ecmascript => {
                (name == "eval"
                    && qual.is_empty()
                    && first.map(|a| !self.is_constant_string(a)).unwrap_or(false))
                    || (name == "Function" && call.ctor)
                    || (matches!(name, "setTimeout" | "setInterval")
                        && first.map(|a| self.is_string_like(a)).unwrap_or(false))
            }
            _ => false,
        };
        if dynamic {
            self.report_node(
                "dynamic-code-execution",
                node,
                format!("`{}` executes code built at run time. Make sure it cannot contain untrusted input.", shorten(&self.callee_text(call), 60)),
            );
        }

        // Weak hashes.
        let lit = |i: usize| {
            call.args
                .get(i)
                .and_then(|a| self.literal_value(*a))
                .map(|s| s.to_ascii_lowercase())
        };
        let weak = match family {
            F::Python => {
                let insecure_ok = call
                    .kwargs
                    .iter()
                    .any(|(k, v)| k == "usedforsecurity" && self.text(*v) == "False");
                !insecure_ok
                    && qual == "hashlib"
                    && (matches!(name, "md5" | "sha1")
                        || (name == "new" && matches!(lit(0).as_deref(), Some("md5" | "sha1"))))
            }
            F::Ecmascript => {
                name == "createHash"
                    && matches!(lit(0).as_deref(), Some("md5" | "sha1" | "md4" | "md2"))
            }
            F::Go => matches!(qual, "md5" | "sha1") && matches!(name, "New" | "Sum"),
            F::Java | F::Kotlin => {
                (name == "getInstance"
                    && qual.ends_with("MessageDigest")
                    && matches!(
                        lit(0).as_deref(),
                        Some("md5" | "sha-1" | "sha1" | "md2" | "md4")
                    ))
                    || (qual.ends_with("DigestUtils")
                        && (name.starts_with("md5") || name.starts_with("sha1")))
            }
            F::Csharp => {
                (matches!(qual, "MD5" | "SHA1") && name == "Create")
                    || (call.ctor
                        && matches!(
                            name,
                            "MD5CryptoServiceProvider"
                                | "SHA1CryptoServiceProvider"
                                | "SHA1Managed"
                        ))
            }
            F::Rust => {
                (qual == "md5" && name == "compute")
                    || (matches!(qual, "Md5" | "Sha1") && matches!(name, "new" | "digest"))
            }
            F::C => {
                qual.is_empty()
                    && matches!(
                        name,
                        "MD5" | "MD5_Init" | "SHA1" | "SHA1_Init" | "EVP_md5" | "EVP_sha1"
                    )
            }
        };
        if weak {
            self.report_node(
                "weak-hash",
                node,
                "MD5 and SHA-1 are broken for security purposes. Make sure this hash is not used for passwords, signatures or integrity against an attacker.".into(),
            );
        }

        // TLS verification switched off through a call.
        let tls_off = match family {
            F::Python => {
                call.kwargs
                    .iter()
                    .any(|(k, v)| k == "verify" && self.text(*v) == "False")
                    || (qual == "ssl" && name == "_create_unverified_context")
            }
            F::Rust => {
                matches!(
                    name,
                    "danger_accept_invalid_certs" | "danger_accept_invalid_hostnames"
                ) && first.map(|a| self.text(a) == "true").unwrap_or(false)
            }
            _ => false,
        };
        if tls_off {
            self.report_node(
                "tls-verification-disabled",
                node,
                format!(
                    "`{}` disables TLS certificate verification.",
                    shorten(&self.callee_text(call), 60)
                ),
            );
        }

        // Unsafe deserialization.
        let deserialize = match family {
            F::Python => {
                (matches!(qual, "pickle" | "cPickle" | "_pickle" | "dill")
                    && matches!(name, "load" | "loads" | "Unpickler"))
                    || (qual == "marshal" && matches!(name, "load" | "loads"))
                    || (qual == "jsonpickle" && name == "decode")
                    || (qual == "yaml" && name == "unsafe_load")
                    || (qual == "yaml"
                        && name == "load"
                        && !call
                            .kwargs
                            .iter()
                            .any(|(k, v)| k == "Loader" && self.text(*v).contains("Safe"))
                        && !call
                            .args
                            .get(1)
                            .map(|a| self.text(*a).contains("Safe"))
                            .unwrap_or(false))
            }
            F::Java | F::Kotlin => {
                (call.ctor || family == F::Kotlin)
                    && matches!(name, "ObjectInputStream" | "XMLDecoder")
            }
            F::Csharp => {
                call.ctor
                    && matches!(
                        name,
                        "BinaryFormatter"
                            | "SoapFormatter"
                            | "NetDataContractSerializer"
                            | "LosFormatter"
                            | "ObjectStateFormatter"
                    )
            }
            _ => false,
        };
        if deserialize {
            self.report_node(
                "unsafe-deserialization",
                node,
                format!("`{}` can run code or create arbitrary objects from its input. Make sure the data cannot come from an untrusted source.", shorten(&self.callee_text(call), 60)),
            );
        }

        // Unbounded string copies in C and C++.
        if family == F::C
            && (qual.is_empty() || qual == "std")
            && matches!(
                name,
                "strcpy" | "strcat" | "sprintf" | "vsprintf" | "wcscpy" | "wcscat" | "gets"
            )
        {
            let message = if name == "gets" {
                "`gets` cannot be used safely: it has no way to limit the input size. Use fgets."
                    .to_string()
            } else {
                format!("`{name}` does not check the size of its destination buffer.")
            };
            self.report_node("unbounded-string-copy", node, message);
        }
    }

    fn callee_text(&self, call: &Call) -> String {
        let prefix = if call.ctor { "new " } else { "" };
        if call.qual.is_empty() {
            format!("{prefix}{}", call.name)
        } else {
            format!("{prefix}{}.{}", call.qual, call.name)
        }
    }

    // ---- strings -------------------------------------------------------

    fn unwrap<'t>(&self, mut n: Node<'t>) -> Node<'t> {
        for _ in 0..8 {
            match n.kind() {
                "parenthesized_expression"
                | "argument"
                | "value_argument"
                | "reference_expression"
                | "await_expression" => {
                    match named_children(n)
                        .into_iter()
                        .rev()
                        .find(|c| !COMMENT_KINDS.contains(&c.kind()))
                    {
                        Some(inner) => n = inner,
                        None => return n,
                    }
                }
                _ => return n,
            }
        }
        n
    }

    fn is_string_kind(kind: &str) -> bool {
        matches!(
            kind,
            "string"
                | "string_literal"
                | "interpreted_string_literal"
                | "raw_string_literal"
                | "template_string"
                | "interpolated_string_expression"
                | "verbatim_string_literal"
                | "concatenated_string"
                | "multiline_string_literal"
                | "line_string_literal"
        )
    }

    fn has_interpolation(&self, n: Node) -> bool {
        let mut stack = vec![n];
        let mut seen = 0;
        while let Some(x) = stack.pop() {
            seen += 1;
            if seen > 500 {
                return false;
            }
            if matches!(
                x.kind(),
                "interpolation"
                    | "template_substitution"
                    | "string_interpolation"
                    | "interpolated_expression"
                    | "interpolated_identifier"
            ) {
                return true;
            }
            stack.extend(named_children(x));
        }
        if self.lang == Language::Kotlin {
            let t = self.text(n);
            let b = t.as_bytes();
            return b.windows(2).any(|w| {
                w[0] == b'$' && (w[1] == b'{' || w[1] == b'_' || w[1].is_ascii_alphabetic())
            });
        }
        false
    }

    /// A string literal without interpolation, a constant declared in this
    /// file, or a concatenation of them.
    fn is_constant_string(&self, n: Node) -> bool {
        let n = self.unwrap(n);
        if Self::is_string_kind(n.kind()) {
            return !self.has_interpolation(n);
        }
        if matches!(n.kind(), "identifier" | "simple_identifier")
            && self.constants.contains(self.text(n))
        {
            return true;
        }
        if matches!(n.kind(), "binary_expression" | "binary_operator") && self.operator(n) == "+" {
            if let Some((l, r)) = self.operands(n) {
                return self.is_constant_string(l) && self.is_constant_string(r);
            }
        }
        false
    }

    fn is_string_like(&self, n: Node) -> bool {
        let n = self.unwrap(n);
        Self::is_string_kind(n.kind()) || self.dynamic_string(n).is_some()
    }

    /// The content of a plain string literal, without quotes.
    fn literal_value(&self, n: Node) -> Option<String> {
        let n = self.unwrap(n);
        if !Self::is_string_kind(n.kind()) || self.has_interpolation(n) {
            return None;
        }
        let t = self.text(n);
        let start = t.find(['"', '\'', '`'])?;
        let q = t.as_bytes()[start] as char;
        let end = t.rfind(q)?;
        if end <= start {
            return None;
        }
        Some(t[start + 1..end].trim_matches(q).to_string())
    }

    /// When `n` is a string assembled at run time, returns how it was built
    /// and the text of its literal parts.
    fn dynamic_string(&self, n: Node) -> Option<(&'static str, String)> {
        let n = self.unwrap(n);
        let kind = n.kind();
        if Self::is_string_kind(kind) {
            return self
                .has_interpolation(n)
                .then(|| ("built by interpolation", self.text(n).to_string()));
        }
        if matches!(kind, "binary_expression" | "binary_operator") {
            let op = self.operator(n);
            self.operands(n)?;
            let python_format = self.lang == Language::Python && op == "%";
            if op == "+" || python_format {
                let literal = self.literal_parts(n);
                let has_literal = !literal.is_empty();
                if has_literal && !self.is_constant_string(n) {
                    let how = if python_format {
                        "built with % formatting"
                    } else {
                        "built by concatenation"
                    };
                    return Some((how, literal));
                }
            }
            return None;
        }
        if kind == "macro_invocation" {
            let name = n
                .child_by_field_name("macro")
                .map(|m| self.text(m))
                .unwrap_or("");
            if name == "format" {
                let literal: String = named_children(n)
                    .into_iter()
                    .flat_map(named_children)
                    .filter(|c| Self::is_string_kind(c.kind()))
                    .map(|c| self.text(c).to_string())
                    .collect();
                if literal.contains('{') {
                    return Some(("built with format!", literal));
                }
            }
            return None;
        }
        let call = self.call(n)?;
        let formatting = match call.name.as_str() {
            "format" => {
                // Python "...".format(x), Java/Kotlin String.format, Kotlin "...".format(x)
                call.qual == "String"
                    || call.qual == "string"
                    || n.child_by_field_name("function")
                        .and_then(|f| f.child_by_field_name("object"))
                        .map(|o| Self::is_string_kind(o.kind()))
                        .unwrap_or(false)
                    || call.qual.starts_with('"')
            }
            "Format" => call.qual == "string" || call.qual == "String",
            "Sprintf" => call.qual == "fmt",
            "formatted" => true,
            _ => false,
        };
        if !formatting {
            return None;
        }
        let mut literal = String::new();
        for a in &call.args {
            if Self::is_string_kind(self.unwrap(*a).kind()) {
                literal.push_str(self.text(*a));
            }
        }
        if call.qual.starts_with('"') {
            literal.push_str(&call.qual);
        }
        if let Some(obj) = n
            .child_by_field_name("function")
            .and_then(|f| f.child_by_field_name("object"))
        {
            if Self::is_string_kind(obj.kind()) {
                literal.push_str(self.text(obj));
            }
        }
        let has_values = call
            .args
            .iter()
            .chain(call.kwargs.iter().map(|(_, v)| v))
            .any(|a| !self.is_constant_string(*a));
        (!literal.is_empty() && has_values).then_some(("built with string formatting", literal))
    }

    /// Concatenated text of the string literals in a `+` / `%` expression.
    fn literal_parts(&self, n: Node) -> String {
        let n = self.unwrap(n);
        if Self::is_string_kind(n.kind()) {
            return self.text(n).to_string();
        }
        if matches!(n.kind(), "binary_expression" | "binary_operator") {
            if let Some((l, r)) = self.operands(n) {
                return self.literal_parts(l) + " " + &self.literal_parts(r);
            }
        }
        String::new()
    }

    fn dynamic_sql(&self, n: Node) -> Option<&'static str> {
        let (how, literal) = self.dynamic_string(n)?;
        looks_like_sql(&literal).then_some(how)
    }

    fn sql_argument(&self, arg: Node) -> Option<String> {
        if let Some(how) = self.dynamic_sql(arg) {
            return Some(how.to_string());
        }
        let a = self.unwrap(arg);
        if matches!(a.kind(), "identifier" | "simple_identifier") {
            let name = self.text(a);
            if self
                .scopes
                .last()
                .map(|s| s.contains_key(name))
                .unwrap_or(false)
            {
                return Some(format!("from `{name}`, which was built from strings"));
            }
        }
        None
    }
}

const SQL_SINKS: &[&str] = &[
    "execute",
    "executemany",
    "executescript",
    "raw",
    "text",
    "read_sql",
    "read_sql_query",
    "query",
    "$queryRawUnsafe",
    "$executeRawUnsafe",
    "prepare",
    "exec",
    "run",
    "all",
    "get",
    "each",
    "Query",
    "QueryRow",
    "Exec",
    "QueryContext",
    "QueryRowContext",
    "ExecContext",
    "Prepare",
    "PrepareContext",
    "Raw",
    "Select",
    "Get",
    "executeQuery",
    "executeUpdate",
    "executeLargeUpdate",
    "addBatch",
    "prepareStatement",
    "prepareCall",
    "createQuery",
    "createNativeQuery",
    "createSQLQuery",
    "queryForObject",
    "queryForList",
    "queryForMap",
    "update",
    "batchUpdate",
    "rawQuery",
    "execSQL",
    "ExecuteSqlRaw",
    "ExecuteSqlRawAsync",
    "FromSqlRaw",
    "SqlQueryRaw",
    "ExecuteSqlCommand",
    "FromSql",
    "QueryAsync",
    "Execute",
    "ExecuteAsync",
    "QueryFirst",
    "QueryFirstOrDefault",
    "QuerySingle",
    "query_row",
    "query_map",
    "execute_batch",
    "query_as",
    "query_scalar",
];

const SQL_COMMAND_TYPES: &[&str] = &[
    "SqlCommand",
    "OleDbCommand",
    "OdbcCommand",
    "MySqlCommand",
    "NpgsqlCommand",
    "SqliteCommand",
    "SQLiteCommand",
];

/// The literal text reads like a SQL statement: a statement keyword plus a
/// clause keyword, or a statement keyword as the first word.
fn looks_like_sql(literal: &str) -> bool {
    let words: Vec<String> = literal
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        // SQL is written in upper or lower case; "Select a file" is prose.
        .filter(|w| {
            w.chars().all(|c| !c.is_ascii_lowercase()) || w.chars().all(|c| !c.is_ascii_uppercase())
        })
        .map(|w| w.to_ascii_uppercase())
        .collect();
    const STATEMENTS: &[&str] = &[
        "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "CREATE", "DROP", "ALTER",
        "TRUNCATE", "EXEC", "UPSERT",
    ];
    const CLAUSES: &[&str] = &["FROM", "INTO", "SET", "WHERE", "TABLE", "VALUES", "JOIN"];
    let statement = words.iter().any(|w| STATEMENTS.contains(&w.as_str()));
    let clause = words.iter().any(|w| CLAUSES.contains(&w.as_str()));
    // Skip a leading string prefix such as f, r or b.
    let first = words.iter().find(|w| {
        !matches!(
            w.as_str(),
            "F" | "R" | "B" | "FR" | "RF" | "BR" | "RB" | "U"
        )
    });
    statement
        && (clause
            || first
                .map(|w| STATEMENTS.contains(&w.as_str()))
                .unwrap_or(false))
}

fn has_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(pos) = text[from..].find(word) {
        let start = from + pos;
        let end = start + word.len();
        let before =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let after =
            end >= bytes.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
        if before && after {
            return true;
        }
        from = end;
    }
    false
}

fn first_line(n: Node) -> Span {
    let mut s = span(n);
    s.end_line = s.start_line;
    s.end_col = s.start_col;
    s.end_byte = s.start_byte;
    s
}

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// FNV-1a, 64 bit: stable across platforms and Rust versions.
fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_detection() {
        assert!(looks_like_sql("\"SELECT * FROM t WHERE id = \""));
        assert!(looks_like_sql("f\"SELECT {a}\""));
        assert!(looks_like_sql("\"DELETE FROM users\""));
        assert!(!looks_like_sql("\"Select a file\""));
        assert!(!looks_like_sql("\"hello \""));
    }

    #[test]
    fn words() {
        assert!(has_word("// TODO: x", "TODO"));
        assert!(!has_word("// TODOS", "TODO"));
        assert!(!has_word("todo", "TODO"));
    }
}
