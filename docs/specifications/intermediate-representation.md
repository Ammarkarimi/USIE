# Intermediate representation (IR)

Code: `engines/intermediate-representation` (`IR_SCHEMA_VERSION = 1`).

The IR is a normalized, per-file description produced by parsing. It is
deliberately **not** a shared AST: it keeps only what cross-file analysis
needs and puts language-specific facts in open `attrs` maps rather than
forcing them into common fields.

```
FileIr
  schema_version, language, parser {name, version}
  symbols[]     id, parent, kind, name, qualified_name, span, name_span,
                signature, exported, bases[], implements[], locals[], doc, attrs
  imports[]     module, level, kind (named|default|namespace|side_effect|
                wildcard|dynamic), imported, local, scope, type_only, span
  exports[]     kind (named|default|wildcard|namespace), name, local, source, span
  references[]  kind (call|new|jsx|decorator), scope, path[], member, text, span
  diagnostics[] code (syntax_error|missing_token), message, span
  stats         nodes, diagnostics_total, parse_micros, extract_micros
  attrs         e.g. commonjs, dunder_all, dunder_all_dynamic,
                anonymous_locals, truncated, export_assignment
```

Invariants (checked by property tests over random bytes and random
source-like text, and re-validated by `usie-engine resolve` on input):

- symbol ids are dense and in order; a parent precedes its children;
- `qualified_name` is the parent's qualified name plus `.name`;
- every span lies inside the file; line numbers are 1-based, columns are
  0-based byte offsets;
- `scope` fields refer to existing symbols;
- `locals` are sorted and unique;
- the IR round-trips through JSON unchanged.

`locals` lists the names a function binds (parameters, assignments, loop
variables, function-level imports, minus `global`/`nonlocal` names). It is
what makes shadowing-aware resolution possible.

A reference's `path` is the static dotted path of the callee
(`["os","path","join"]`, `["self","save"]`, `["super","run"]`). When the
callee is computed, `path` is empty and `member` holds the final property
name if there is one.

The IR schema version and the extractor version are part of the parser
fingerprint, so any change to extraction output invalidates cached IR.
