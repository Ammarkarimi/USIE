# Code quality and security checks

USIE checks every analyzed version for bugs, vulnerabilities, security
hotspots and maintainability problems, tracks which findings are new since the
previous version, records people's reviews of findings, and evaluates a
quality gate. Design rationale: [ADR 0013](../adr/0013-code-quality.md).

## What a finding is

A finding is a **pattern match**. Rules see one file at a time, its syntax
tree and its text. They do not see types, other files, configuration or
runtime values. A finding therefore means "this code has the shape the rule
describes", and the absence of findings means only that no rule matched.
Every rule's description says what it matches and where it is known to be
wrong; the web interface shows it next to each finding.

Categories:

| Category | Meaning |
|---|---|
| `bug` | Code that is likely wrong. |
| `vulnerability` | Code that is likely exploitable. |
| `hotspot` | Security-sensitive code that a person should review; often safe. |
| `smell` | Code that is hard to understand or change. |

Severities, most severe first: `blocker`, `critical`, `major`, `minor`, `info`.
A rule has one category and one severity.

## Where findings come from

1. **Syntax-tree rules** run in the analysis engine during parsing
   (`engines/parser/src/checks.rs`), for the 11 parsed languages. Results are
   cached with the parse result, so unchanged files are not re-checked.
2. **Text rules** run in the worker while files are scanned
   (`services/code-quality`): credential formats and hard-coded passwords in
   every text file that is not vendored or generated, and duplicated code
   across non-test source files of parsed languages.

Findings of the `smell` category are dropped for test files: test code is
not held to the structure rules. Bugs, vulnerabilities and hotspots in tests
are kept and marked `in_test`.

## Rules

Thresholds: cyclomatic complexity 15, function length 100 lines, 7
parameters, nesting depth 4, duplication window 10 code lines.

| Rule | Category | Severity | Languages | Matches |
|---|---|---|---|---|
| `function-complexity` | smell | major | all 11 | Cyclomatic complexity above 15: one plus the number of branches (if, loops, cases, catch clauses, conditional expressions and && / \|\| / ?? operators) in a function. Anonymous functions that are not bound to a name count towards the enclosing function. |
| `function-length` | smell | minor | all 11 | A function spans more than 100 lines, including blank lines and comments. |
| `too-many-parameters` | smell | minor | all 11 | A function declares more than 7 parameters (self, this and C `void` are not counted). |
| `deep-nesting` | smell | minor | all 11 | if, loop, switch/match and try statements nested more than 4 levels deep inside one function. `else if` chains do not add levels. Reported once per function, at the first statement that is too deep. |
| `todo-comment` | smell | info | all 11 | A comment contains the word TODO, FIXME, XXX or HACK in upper case. |
| `empty-catch` | smell | major | python, javascript, typescript, tsx, java, csharp, kotlin, cpp | A catch / except block is empty (in Python: contains only `pass`). A block that contains a comment is treated as intentional and not reported. |
| `bare-except` | smell | major | python | An `except:` clause without an exception type also catches SystemExit and KeyboardInterrupt. |
| `debugger-statement` | smell | major | javascript, typescript, tsx | A `debugger` statement pauses execution whenever developer tools are open. |
| `loose-equality` | smell | minor | javascript, typescript, tsx | `==` or `!=` performs type coercion. Comparisons with `null` are not reported, since `x == null` is a common way to test for null and undefined. |
| `self-comparison` | bug | major | all 11 | Both operands of a comparison, logical, subtraction or division operator are the same expression (after whitespace normalization). Operands containing calls or increments are not reported. `x != x` is a valid NaN test in some languages; it is still reported because an explicit isNaN call says what is meant. |
| `identical-branches` | bug | major | all 11 | An if/else with exactly two branches, or a conditional expression, has the same code (after whitespace normalization) in both branches. |
| `assignment-in-condition` | bug | major | javascript, typescript, tsx, c, cpp | The condition of an if, while or do-while is an assignment, which is usually a typo for a comparison. Wrapping the assignment in an extra pair of parentheses marks it as intentional and is not reported. |
| `mutable-default-argument` | bug | major | python | A parameter defaults to a list, dict or set literal. The default is created once and shared by every call. |
| `unreachable-code` | bug | major | python, javascript, typescript, tsx, go, java, csharp, c, cpp | A statement follows a return, raise/throw, break or continue in the same block. Labels, hoisted function declarations, local functions and preprocessor directives are not reported. |
| `sql-injection` | vulnerability | critical | python, javascript, typescript, tsx, go, java, csharp, kotlin, rust | A database query method (execute, query, Query, executeQuery, rawQuery, ExecuteSqlRaw, ...) receives SQL that is built by concatenation, interpolation or string formatting, directly or through a local variable assigned in the same function. Only strings whose literal parts look like SQL are reported. Values that are constants or already validated are reported too, since the rule cannot see where they come from. |
| `command-execution` | hotspot | critical | python, javascript, typescript, tsx, go, java, csharp, kotlin, c, cpp | A shell or process is started with a command that is not a constant string: os.system, os.popen and subprocess with shell=True (Python), child_process.exec/execSync (JavaScript), exec.Command through a shell or with a built command (Go), Runtime.exec (Java, Kotlin), Process.Start (C#), system and popen (C, C++). |
| `dynamic-code-execution` | hotspot | critical | python, javascript, typescript, tsx | eval or exec with a non-constant argument (Python), eval, new Function, and setTimeout / setInterval with a string (JavaScript). |
| `weak-hash` | hotspot | major | all 11 | MD5 or SHA-1 is used (hashlib.md5/sha1, createHash('md5'), md5.New/Sum, MessageDigest.getInstance("MD5"), MD5.Create, md5::compute, MD5()/SHA1() in C). Both are broken for signatures, passwords and integrity against an attacker; they are acceptable for non-security checksums. Python calls with usedforsecurity=False are not reported. |
| `tls-verification-disabled` | vulnerability | critical | python, javascript, typescript, tsx, go, csharp, rust | Certificate or host name verification is turned off: verify=False, ssl._create_unverified_context and ssl.CERT_NONE (Python), rejectUnauthorized: false and NODE_TLS_REJECT_UNAUTHORIZED=0 (JavaScript), InsecureSkipVerify: true (Go), DangerousAcceptAnyServerCertificateValidator or a validation callback that returns true (C#), danger_accept_invalid_certs(true) (Rust). |
| `unsafe-deserialization` | hotspot | critical | python, java, csharp, kotlin | A deserializer that can run code or instantiate arbitrary types is used: pickle, marshal, dill, jsonpickle and yaml.load without a safe loader (Python), ObjectInputStream and XMLDecoder (Java, Kotlin), BinaryFormatter, SoapFormatter, NetDataContractSerializer, LosFormatter and ObjectStateFormatter (C#). |
| `unbounded-string-copy` | hotspot | major | c, cpp | strcpy, strcat, sprintf, vsprintf, wcscpy, wcscat or gets is called. These do not check the destination size. gets cannot be used safely at all and was removed in C11. |
| `unsafe-block` | hotspot | minor | rust | An `unsafe` block disables some of the compiler's memory-safety checks. |
| `hardcoded-secret` | vulnerability | blocker | any text file | Exact formats of known credentials (AWS key IDs, GitHub, GitLab, npm, Slack, Stripe live, SendGrid, Google API, OpenAI, Anthropic keys, Slack webhook URLs) and private key blocks (a PEM header followed by key data). Tests included; values containing EXAMPLE or xxxx are ignored. |
| `hardcoded-password` | vulnerability | critical | any text file except tests and documentation | A quoted literal (6+ characters) assigned to a name containing password, secret, api_key, access_key, auth_token, access_token, client_secret or private_key; unquoted values in .env, properties, ini, conf and YAML files. Placeholders, references, constant names and low-entropy values are ignored. |
| `duplicated-block` | smell | minor | parsed languages, non-test source | 10 or more consecutive identical code lines (whitespace-normalized; blank, comment, bracket-only and import lines skipped) also found elsewhere in the repository. |

The catalog is also served by `GET /api/v1/quality/rules`.

## Measures

Per version (non-test files of parsed languages): number of functions, the
sum and the maximum of their cyclomatic complexity, and duplicated lines as a
share of the compared code lines. Cyclomatic complexity is one plus the number
of branches in a function: if, loops, non-default cases and match arms, catch
clauses, conditional expressions and `&&`, `||`, `??` operators. Anonymous
functions that are not bound to a name count towards the enclosing function.
Duplication detection keeps at most two million code lines in memory; larger
repositories report duplication as not measured.

Test coverage is **not measured**: USIE does not run tests during analysis.

## New findings and fingerprints

Each finding has a fingerprint: a hash of the rule, the file path, the
whitespace-normalized matched code (for function-level rules, the function
name) and the occurrence number of that same code in the file. Moving code
within a file keeps the fingerprint; editing the matched code or renaming the
file changes it. A secret's value never enters a fingerprint, message or
stored context: the line is hashed with the value replaced.

A finding is **new** when the baseline, the previous analyzed version of the
repository, has no finding with the same fingerprint. The first analysis (or
the first after a version analyzed before these checks existed) has no
baseline, and nothing is new.

## Reviews

A member can review a finding. The review applies to the fingerprint, so it
carries over to every version where the same finding appears; deleting it
reopens the finding. Reviews are audited.

| Status | For | Meaning |
|---|---|---|
| `accepted` | any | Real, deliberately not fixed. |
| `false_positive` | bugs, vulnerabilities, smells | The rule matched code it should not have. A comment is required. |
| `safe` | hotspots | Reviewed; the code is safe in context. |

Reviewed findings do not count as open and do not affect the gate or grades.

## Grades

| Grade | Reliability (bugs) and security (vulnerabilities) | Security review (hotspots reviewed) |
|---|---|---|
| A | no open findings | at least 80%, or no hotspots |
| B | only minor or info | at least 70% |
| C | at least one major | at least 50% |
| D | at least one critical | at least 30% |
| E | at least one blocker | less than 30% |

## Quality gate

A gate is a list of conditions; a condition fails when its metric is greater
than its threshold, and the gate fails when any condition fails. Conditions on
new findings report `no_baseline` and do not fail the gate when there is no
baseline. Metrics: `new_bugs`, `new_vulnerabilities`, `new_hotspots`,
`new_smells`, `bugs`, `vulnerabilities`, `hotspots`, `smells`,
`blocker_findings`, `critical_findings`, `duplicated_lines_percent` (open
findings only; hotspots count while unreviewed).

The default gate fails on any new bug, vulnerability or unreviewed hotspot,
and on any open blocker. Administrators can replace a repository's
conditions (`PUT /api/v1/repositories/{id}/quality-gate`); changes are
audited. The gate is evaluated when read, so reviews take effect
immediately without re-analysis.

## Limits

- No type information, data flow or cross-file analysis: for example
  `sql-injection` cannot tell whether a concatenated value was validated,
  and reports it for review.
- Secret detection covers the listed formats and assignment patterns only;
  it does not search git history.
- Rule thresholds are fixed; rules cannot be turned off per repository.
  Review a finding as accepted or false positive instead.
- At most 2,000 syntax-tree findings and 100 secret findings per file.
