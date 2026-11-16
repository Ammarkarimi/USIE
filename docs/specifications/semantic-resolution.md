# Semantic resolution

Status: **implemented** for Python, JavaScript, TypeScript, TSX, Go and Java.
Code: `engines/semantic` (resolver), `engines/parser` (extraction),
`internal/pipeline/build.go` (graph assembly).

This document specifies how USIE turns per-file syntax into cross-file
relationships, and exactly what each confidence level promises. Where this
document and the code disagree, the code's tests are authoritative and this
document is wrong; please fix it.

## 1. Principles

1. **No silent guessing.** Every syntactic reference the resolver examines is
   recorded, including the ones it cannot resolve. An unresolved reference
   carries the rule that stopped it and a human-readable reason.
2. **Every relationship has provenance.** Each edge records the identifier of
   the rule that produced it, the location of its strongest evidence, and the
   number of source occurrences it aggregates.
3. **Confidence is about the analysis, not the program.** "Confirmed" means
   the resolver established the relationship from declarations and imports
   alone. It does not mean the code path executes at runtime.
4. **Bounded work.** Import chains, re-export chains and inheritance walks
   are capped (16 steps) and cycle-checked; module-surface lookups are
   memoized. A pathological repository degrades to unresolved references, not
   to a hang.

## 2. Confidence levels

Levels are ordered; combining evidence along a chain yields the weakest link.

| Level | Meaning | Typical sources |
|---|---|---|
| `confirmed` | Follows directly from declarations, imports and exports in the analyzed files, with exactly one candidate. | Calling an imported function; relative imports; `ClassName.method` defined on that class; standard-library and declared-dependency imports; in Go and Java, a method of a value whose declared type is known (`var s *Store; s.Save()`; static, private and final Java methods), including values returned by functions with a declared result type. |
| `probable` | Correct under ordinary runtime behaviour, but depends on something static analysis cannot fix. | `self.m()` / `this.m()` and Java instance methods on typed values (an override in a subclass could be selected); Go calls through interfaces; inherited members; `super`; wildcard imports; workspace packages and tsconfig `baseUrl` (a same-named installed package could shadow them); undeclared third-party packages; several definitions of one name. |
| `inferred` | A heuristic match. Should be verified before being relied on. | `obj.method()` where the type of `obj` is unknown and exactly one method of that name exists in the repository (excluding common names such as `get`, `push`, `map`). |
| `unknown` | Not established. Used only on unresolved reference records, never on edges. | — |

## 3. Node keys

Nodes have a snapshot-local integer id and a **stable key** used to compare
versions and to link between views:

| Key | Node |
|---|---|
| `repo` | the repository |
| `dir:<path>` | a directory |
| `file:<path>` | a file |
| `sym:<path>#<qualified name>` | a symbol; a second definition with the same qualified name in the same file gets `~2`, and so on |
| `ext:<ecosystem>:<name>` | an external package (`python`, `npm`, `node`, and for declared-but-unimported Python distributions `pypi`; `go` for Go modules and standard-library packages; `jdk` for Java SE packages; `maven` for other Java packages; `cargo` for declared dependencies) |

Paths are repository-relative with `/` separators.

## 4. Relationship kinds

| Kind | From → to | Produced by |
|---|---|---|
| `CONTAINS` | repo/dir → dir/file; file → top-level symbol; symbol → member | `structure.filesystem`, `structure.declaration` |
| `IMPORTS` | file → file, file → directory (a Go package; a Java package of an on-demand import), file → symbol (a Java type or static member), or file → package | import statements, `require`, dynamic `import("literal")`, re-export sources |
| `EXPORTS` | file → symbol | ES `export` forms and CommonJS `module.exports`; Python `__all__` only |
| `CALLS` | symbol or file → symbol or package | call expressions |
| `INSTANTIATES` | symbol or file → class | `new X()`; in Python, calling a class; Go composite literals `T{…}` |
| `EXTENDS` | class/interface → class/interface/package | base classes, `extends` clauses |
| `IMPLEMENTS` | class → interface | TypeScript and Java `implements` |
| `REFERENCES` | symbol or file → symbol | JSX element names, decorators, Java annotations, `@prop.setter`, Go conversions to named non-struct types (`Celsius(x)`) |
| `DEPENDS_ON` | repo → package | manifest declarations (`manifest.declared-dependency`) |

The source of a call is the innermost enclosing symbol; calls at module level
are attributed to the file.

## 5. Name resolution

A dotted reference `a.b.c` seen from scope `S` in file `F` is resolved by
resolving its first segment, then descending through members.

### 5.1 First segment

In order:

1. **Receivers.** `self`/`cls`/`super` (Python) or `this`/`super` (ECMAScript)
   inside a class resolve against the enclosing class (§5.3).
2. **Class body.** Code directly in a class body sees names bound in that body
   (`scope.class-member`); `@prop.setter` resolves to `prop`
   (`py.property-accessor`).
3. **Lexical scopes**, innermost first. If the name is a local binding of a
   function (parameter, assignment, loop variable, import inside the
   function):
   - a nested definition of that name → `scope.nested-definition`;
   - a function-level import → that import's target;
   - otherwise the reference is **unresolved** (`scope.local-binding`): the
     value of a local variable is not tracked. This is what makes shadowing
     safe — a parameter named like a module-level function never resolves to
     that function.
   Namespace members are visible inside the namespace (`scope.namespace-member`).
4. **Anonymous scopes.** Names bound inside anonymous functions or blocks at
   module level (callbacks, IIFEs) are tracked file-wide. A module-level
   reference to such a name is unresolved, unless the name is a function
   declared inside an anonymous scope, which resolves as `probable`
   (`scope.anonymous-function-definition`).
5. **Module symbols** (`scope.module-symbol`): `confirmed` with one value
   definition; `probable` with several, or when the definition sits in an
   anonymous scope. Declaration merging (interface + class, function +
   namespace) is not ambiguity.
6. **Module-level imports** (§6).
7. **Python only:** `import a.b.c` bindings (longest imported prefix) and
   wildcard imports (always at most `probable`, `py.import.wildcard`).
8. **Builtins** (`print`, `len`, `console`, `JSON`, …) are recorded with
   outcome `builtin` and produce no edge.
9. Otherwise unresolved: `scope.unbound`.

### 5.2 Member descent

- From a **class, interface or namespace**: the member defined on it
  (callables preferred over same-named fields). Members found on a resolved
  base class are at most `probable`.
- From a **module** (namespace import, `import pkg.mod`): the module's export
  surface (§6.3).
- From an **external package**: the edge stays attributed to the package.
- From a **function or variable**: unresolved, `member.value-type-unknown`.
  Return types and variable types are not inferred.

A call or `new` whose target resolves to a module object uses the module's
default export (ECMAScript); calling a Python module is unresolved
(`callee.module`).

### 5.3 Receivers

| Form | Rule | Confidence |
|---|---|---|
| `self.m()`, `this.m()` | `py.call.self-method`, `es.call.this-method` | `probable` (subclass overrides) |
| `super().m()`, `super.m()` | `py.call.super-method`, `es.call.super-method` | `probable` |
| `super(...)` in a constructor | `es.call.super-constructor` → base class | `probable` |
| `cls()` | `py.call.cls` → the class | `probable` |
| `self.a.b()` | unresolved, `receiver.member-type-unknown` | — |

Attributes assigned on instances (`self.x = …`) are not tracked; calling one
is unresolved with that explanation.

### 5.4 Unknown receivers

When a receiver's type is unknown (a local variable, a computed expression,
`self.attr.m()`), the resolver looks for methods named `m` across the
repository (same language family). If there is **exactly one** and the name
is not on the list of ubiquitous method names (see
`engines/semantic/src/builtins.rs`), an `inferred` edge is produced
(`heuristic.unique-method-name`). Otherwise the reference stays unresolved.

## 6. Modules

### 6.1 Python

- The **import root** of a file is the parent of its outermost package (a
  chain of directories containing `__init__.py`/`__init__.pyi`). The
  repository root and configured extra roots are also searched; a module
  found only through such a fallback root is `probable`.
- `src/` layouts work because `src` is the parent of the top package.
- Relative imports (`from ..x import y`) resolve against the file's directory
  (`py.import.relative`).
- `from pkg import name` tries the attribute first, then the submodule
  `pkg/name.py` (`py.import.submodule`); inside a package's own `__init__`,
  `from . import x` can only be a submodule.
- If two files define the same absolute module name on equal footing, the
  import is unresolved and reported as ambiguous.
- Standard-library top-level names are external (`py.import.stdlib`,
  confirmed) unless a repository module of that name is on the importer's own
  root. Declared dependencies (PEP 503-normalized) are confirmed; anything else
  is `probable` (`py.import.undeclared-module`): distribution names often
  differ from import names (`PyYAML` → `yaml`).
- Module attributes: module-level symbols, then module-level import bindings
  (`py.reexport`), then wildcard imports, then (for `__init__`) submodules.
- `EXPORTS` edges come only from literal `__all__` lists. A computed
  `__all__` is flagged in the file's IR attributes, not guessed.

### 6.2 ECMAScript

Specifier resolution, in order:

1. Relative (`./`, `../`): exact file, then `.ts .tsx .d.ts .mts .cts .js
   .jsx .mjs .cjs`, then emitted-name mapping (`./a.js` → `a.ts`), then
   `index.*` (`es.import.relative`, confirmed). Non-source files (CSS, JSON)
   resolve to their file node.
2. `node:` prefix → `ext:node:<name>`.
3. The nearest `tsconfig.json`/`jsconfig.json` with `paths` (longest prefix
   wins; confirmed) or `baseUrl` (probable).
4. In-repository npm workspace packages, by `package.json` `name`, using
   `source`, `types`, `module`, `main` and the root `exports` entry, then
   `src/index`, `index` (`es.import.workspace-package`, probable: package
   `exports` maps and build output are not modelled).
5. Declared dependencies (confirmed), Node built-ins (confirmed), otherwise
   undeclared packages (probable).

Absolute specifiers (`/x`) are unresolved: their meaning depends on the
bundler or host.

### 6.3 Export surface (ECMAScript)

`export function/class/const`, export clauses with aliases, `export default`
(including anonymous defaults, recorded as a symbol named `default`),
`export =`, re-exports (`export { a as b } from`, `export * as ns from`) and
wildcard re-exports (`export * from`, followed only when exactly one source
provides the name). CommonJS `module.exports = {…}`, `module.exports.x =`,
`exports.x =`; for pure CommonJS files, a top-level definition with the
requested name is accepted as `probable` (`js.cjs.name-match`).

### 6.4 Go

A **package** is the set of `.go` files in one directory with the same
`package` clause; an external test package (`foo_test`) in the same directory
is separate, and an import of the directory means the non-test package.
Package-level declarations are visible in every file of the package. Methods
are package-level symbols keyed `Type.Method` and are found as members of
their receiver type wherever they are declared in the package.

Import paths (rule in parentheses):

1. A path under the `module` path of a repository `go.mod` maps to the
   corresponding directory (`go.import.module`, confirmed). The longest
   module path wins. No package in that directory is reported unresolved.
2. `vendor/<path>` in the importing directory or any ancestor
   (`go.import.vendor`, confirmed).
3. A path whose first element has no dot is the standard library
   (`go.import.stdlib`, confirmed, node `ext:go:<path>`).
4. A path under a `require`d module (`go.import.declared-module`, confirmed,
   node `ext:go:<module>`).
5. Anything else (`go.import.undeclared-module`, probable): the module root is
   estimated from the path (`github.com/a/b/...` → `github.com/a/b`).

The **name** an import binds is its alias, else the `package` clause of the
repository package, else the last path element of a standard-library path.
For external modules the clause is unknown; the conventional name (last
element without `/vN`, `.vN`, `go-` or `-go`) and, for dashed names, each
part are tried, and references through such a name are at most probable.

Name lookup in a Go file, first segment: function-local bindings
(parameters, receivers, `:=`, `var`, range and type-switch variables;
closure bindings are attributed to the enclosing function), then
package-level declarations (`go.package-symbol`; several declarations of one
name can only coexist under exclusive build constraints, so one is chosen as
`probable` with `go.package-symbol.build-variant`), then import names, then
dot imports (`go.import.dot`), then predeclared identifiers (`builtin`).
Members of an imported repository package must be exported (upper-case).

**Declared types.** Locals record their declared type (parameters,
receivers, `var x T`, `x := T{…}` / `&T{…}`) or, when initialized from a
call, the callee and result position. A member access on such a value
(`go.typed-value`, `go.call-result`) looks the member up in the type's
fields and methods: confirmed for a struct or named type, probable through an
interface (dynamic dispatch) or for members promoted from an embedded field.
Struct fields and package variables with a named type are followed the same
way (`s.db.Exec()`). Method chains (`a.B().C()`) follow declared result
types. The type of a value returned by code outside the repository is not
known; such calls are reported unresolved with that reason.

### 6.5 Java

Types are identified by **fully qualified name**: the `package` declaration
plus the nesting of member types. File paths and source roots are not used,
so multi-module builds and non-standard layouts resolve the same way.
Constructors are not members.

Imports: a single-type import resolves to the type (`java.import.type`); if
the name is not a type, `import static a.b.C.m` resolves `m` as a member of
`C` (`java.import.static-member`); an on-demand import of a repository
package is an edge to the package's directory (`java.import.on-demand`,
probable when the package spans several directories); of a type, to the type.
Imports outside the repository become external nodes: Java SE packages
(`java.*`, `jdk.*`, the JDK's `javax.*` packages, `org.w3c.dom`,
`org.xml.sax`, `org.ietf.jgss`) as `ext:jdk:<first two segments>`
(confirmed); packages starting with a declared Maven or Gradle group id as
`ext:maven:<group>` (probable: group ids conventionally, not necessarily,
prefix package names); anything else as `ext:maven:<first three segments>`
(probable, `declared: false`).

Simple names follow the Java Language Specification order: locals and
parameters; members of enclosing types, including inherited ones
(`java.scope.member`); types of the same file; single-type and single-static
imports; types of the same package (`java.scope.package-type`); on-demand
imports (ambiguity between several is reported); `java.lang` (`builtin`);
fully qualified names (`java.scope.qualified-name`). `this.m()` and
`super.m()` resolve in the enclosing class (`java.call.this-member`,
`java.call.super-method`, the latter confirmed because it is not virtually
dispatched); `super(...)` calls the superclass (`java.call.super-constructor`).

**Declared types.** Parameters, typed locals, `var x = new T()`, enhanced-for
variables, catch parameters, resources, pattern variables and fields carry
their declared type; `var x = f()` records the callee. Calls on such values
(`java.typed-value`, `java.call-result`) are **probable** for instance
methods, because the runtime class may override the method, and confirmed for
static, private and final methods and for fields. Casts and `new T()` in a
chain give the value a type. When a member is not found in the repository
and the class extends a type outside the repository, the reference is
attributed to that external package (`java.member.inherited-external`,
probable).

## 7. Unresolved reference reasons

| Rule | Reason |
|---|---|
| `scope.unbound` | No definition or import of the name in the file. |
| `scope.local-binding` | The name is a local variable or parameter; its value is not tracked. |
| `receiver.dynamic`, `callee.dynamic` | The receiver or callee is a computed expression. |
| `receiver.member-type-unknown`, `member.value-type-unknown` | Requires type information USIE does not compute. |
| `member.not-found` | A class, namespace or module was resolved but has no such member. |
| `*.import.*` | The module could not be found, is ambiguous, or the specifier is unsupported. |
| `es.export.not-found`, `py.module-attribute` | The module was found but does not provide the name. |
| `go.call-result`, `java.call-result`, `go.typed-value`, `java.typed-value` | The value's declared type is unknown, is outside the repository, or has no such member. |
| `go.package-member` | The package does not declare the name, or it is not exported. |
| `import.cycle`, `*.chain` | A cyclic or overlong import/re-export chain. |

## 8. Known limitations

- No type inference or data-flow analysis: method calls on values are
  resolved only through receivers, class names, declared types (Go, Java), or
  the unique-name heuristic. Generic type parameters are not substituted
  (`List<User>.get(0).name()` stops at `List`), and Java overload resolution
  is by name only.
- No evaluation of dynamic constructs: `getattr`, computed property access,
  `importlib`, non-literal `import()`, monkey-patching, dependency injection.
- Python: conditional definitions are treated as definitions; `@overload`
  stubs appear as separate symbols with `~n` keys.
- TypeScript: type-only references (annotations, generics) are not recorded;
  `declare module` augmentation is not merged across files.
- Package `exports` subpath maps, conditional exports and installed
  `node_modules`/site-packages are not read.
- Go: build constraints and `//go:build` tags are not evaluated; cgo,
  `go:linkname` and `replace` directives in `go.mod` are not followed;
  `go.work` workspaces are resolved through each module's own `go.mod`.
- Java: Kotlin/Groovy build logic, annotation processors (Lombok-generated
  members) and generated sources (protobuf, OpenAPI) are not seen; a
  reference to them is reported unresolved. Method references (`Foo::bar`)
  are not recorded.
- C, C++, C#, Rust and Kotlin are inventoried but not parsed or resolved.
