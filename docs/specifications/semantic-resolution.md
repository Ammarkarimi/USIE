# Semantic resolution

Status: **implemented** for Python, JavaScript, TypeScript and TSX.
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
| `confirmed` | Follows directly from declarations, imports and exports in the analyzed files, with exactly one candidate. | Calling an imported function; relative imports; `ClassName.method` defined on that class; standard-library and declared-dependency imports. |
| `probable` | Correct under ordinary runtime behaviour, but depends on something static analysis cannot fix. | `self.m()` / `this.m()` (an override in a subclass could be selected); inherited members; `super`; wildcard imports; workspace packages and tsconfig `baseUrl` (a same-named installed package could shadow them); undeclared third-party packages; several definitions of one name. |
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
| `ext:<ecosystem>:<name>` | an external package (`python`, `npm`, `node`, and for declared-but-unimported Python distributions `pypi`; also `go`, `cargo`, `maven` for declared dependencies) |

Paths are repository-relative with `/` separators.

## 4. Relationship kinds

| Kind | From → to | Produced by |
|---|---|---|
| `CONTAINS` | repo/dir → dir/file; file → top-level symbol; symbol → member | `structure.filesystem`, `structure.declaration` |
| `IMPORTS` | file → file, or file → package | import statements, `require`, dynamic `import("literal")`, re-export sources |
| `EXPORTS` | file → symbol | ES `export` forms and CommonJS `module.exports`; Python `__all__` only |
| `CALLS` | symbol or file → symbol or package | call expressions |
| `INSTANTIATES` | symbol or file → class | `new X()`; in Python, calling a class |
| `EXTENDS` | class/interface → class/interface/package | base classes, `extends` clauses |
| `IMPLEMENTS` | class → interface | TypeScript `implements` |
| `REFERENCES` | symbol or file → symbol | JSX element names, decorators, `@prop.setter` |
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
| `import.cycle`, `*.chain` | A cyclic or overlong import/re-export chain. |

## 8. Known limitations

- No type inference or data-flow analysis: method calls on values are
  resolved only through receivers, class names, or the unique-name heuristic.
- No evaluation of dynamic constructs: `getattr`, computed property access,
  `importlib`, non-literal `import()`, monkey-patching, dependency injection.
- Python: conditional definitions are treated as definitions; `@overload`
  stubs appear as separate symbols with `~n` keys.
- TypeScript: type-only references (annotations, generics) are not recorded;
  `declare module` augmentation is not merged across files.
- Package `exports` subpath maps, conditional exports and installed
  `node_modules`/site-packages are not read.
- Languages other than Python, JavaScript and TypeScript are inventoried but
  not parsed or resolved.
