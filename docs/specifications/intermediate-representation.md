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
  references[]  kind (call|new|jsx|decorator), scope, path[], member, chain[],
                text, span
  diagnostics[] code (syntax_error|missing_token), message, span
  stats         nodes, diagnostics_total, parse_micros, extract_micros
  attrs         e.g. commonjs, dunder_all, dunder_all_dynamic,
                anonymous_locals, truncated, export_assignment, package
```

Languages: `python`, `javascript`, `typescript`, `tsx`, `go`, `java`.

Invariants (checked by property tests over random bytes and random
source-like text, and re-validated by `usie-engine resolve` on input):

- symbol ids are dense and in order; a parent precedes its children;
- `qualified_name` is the parent's qualified name plus `.name` (exception:
  Go methods are package-level symbols qualified as `Receiver.name`);
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

For statically typed languages (Go, Java) a reference whose receiver is a
call or an instantiation has a `chain` of steps instead of a `path`: a name
is a member access (leading names are looked up in scope), `()` takes the
result of calling what precedes it, and `type:T` is a value of type `T`
(`new T()`, a cast, a Go composite literal). `new GsonBuilder().create()` is
`["type:GsonBuilder", "create"]`; `a.b().c()` is `["a", "b", "()", "c"]`.

Symbol attributes used by Go and Java resolution:

| Attribute | On | Meaning |
|---|---|---|
| `type_path` | fields, package variables, enum constants | segments of the declared named type (`["pkg","T"]`); absent for slices, maps, primitives, arrays |
| `local_types` | functions, methods | local name → declared type segments; a name bound with conflicting types is omitted |
| `local_calls` | functions, methods | local name → `{callee: segments, result: index}` for locals initialized from a call |
| `result_types` | functions, methods | declared result types by position (`null` where not a named type) |
| `receiver_type`, `receiver_name` | Go methods | the receiver's type name and variable |
| `type_kind` | types | Go: `struct`, `interface`, `defined`, `alias`; Java: `class`, `interface`, `enum`, `record`, `annotation` |
| `modifiers`, `annotations`, `static`, `constructor`, `abstract` | Java declarations | as written |

File attribute `package` holds the Go package clause or the Java package
declaration.

The IR schema version and the extractor version are part of the parser
fingerprint, so any change to extraction output invalidates cached IR.
