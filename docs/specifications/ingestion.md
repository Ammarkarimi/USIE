# Repository ingestion

Code: `services/repository-ingestion`, used by `internal/pipeline`.

## Sources

| Kind | How | Restrictions |
|---|---|---|
| `git` | `git clone --depth 500 --single-branch` of a branch, tag or commit | `https://` only; no credentials, query or fragment in the URL; the host must not resolve to a loopback, private, link-local or unspecified address (override: `USIE_GIT_ALLOW_PRIVATE_HOSTS`); refs must match `[A-Za-z0-9][A-Za-z0-9._/+-]*` without `..`. |
| `upload` | ZIP archive uploaded through the API | size limit; entries validated (below) |
| `local` | an existing directory on the worker | admin role; must resolve (after symlinks) inside `USIE_ALLOWED_LOCAL_ROOTS`; git metadata is read from `.git/HEAD` and refs as files — git is never run inside a directory USIE did not create |

Git runs with an empty global and system configuration and with hooks,
submodules, LFS smudging, symlink creation, credential helpers, redirects and
every transport except HTTPS disabled. Clones are shallow, so recorded commit
history is the most recent 500 commits.

ZIP extraction rejects absolute paths, `..`, drive letters, backslash
traversal and duplicate entries; skips symlinks and special files; strips a
single wrapping directory; and counts actually decompressed bytes against
the repository limit (headers are not trusted).

## Scanning

- Symbolic links are counted and skipped, never followed; files are opened
  with `O_NOFOLLOW`.
- Always skipped: `.git`, `.hg`, `.svn`, `node_modules`, `__pycache__`,
  virtualenvs, tool caches. `.gitignore` and `.usieignore` files are honoured
  at every level (negation, anchoring, directory-only and `**` patterns).
- Limits (`USIE_MAX_REPO_FILES`, `USIE_MAX_REPO_BYTES`) fail the analysis
  with a clear error. Files larger than `USIE_MAX_FILE_BYTES` are hashed and
  listed but their content is not retained or parsed.

## Per-file record

| Field | Meaning |
|---|---|
| `path` | repository-relative, `/`-separated |
| `content_hash` | SHA-256 of the content |
| `size_bytes`, `line_count` | measured |
| `language` | from file name: the ten target languages (Python, JavaScript, TypeScript, Java, C, C++, Rust, Go, C#, Kotlin) and common formats |
| `category` | `source`, `test`, `config`, `documentation`, `build`, `data`, `asset`, `binary` |
| `encoding` | `utf-8`, `utf-8-bom`, `utf-16le/be`, `binary`, `unknown` |
| `is_generated` | name conventions (lock files, `*.min.js`, `*_pb2.py`, …), header markers (`Code generated`, `@generated`, `DO NOT EDIT`) and minified-bundle detection — a heuristic |
| `is_vendored` | inside `vendor/`, `third_party/`, `node_modules/`, … |
| `parse_status` | `parsed`, `parsed_with_errors`, `unsupported`, `skipped` (vendored, generated, oversize, non-UTF-8), `failed` |
| `change` | `added`, `modified`, `unchanged` relative to the previous ready version |
| version id, `analyzed_at` | the version the record belongs to and when it was analyzed |

## Manifests and build systems

Dependencies are read from `package.json` (all four dependency groups),
`pyproject.toml` (PEP 621, dependency groups, Poetry), `requirements*.txt`,
`go.mod`, `Cargo.toml` (local `path` crates excluded) and `pom.xml`.
`tsconfig.json`/`jsconfig.json` (comments and trailing commas allowed)
provide `baseUrl`/`paths`; `package.json` names and entry points define
workspace packages. `setup.py` is recognised but never executed. Build
systems detected: npm, python, go, cargo, maven, gradle, cmake, make,
docker, meson, bazel, dotnet. Manifests that fail to parse are reported in
the version statistics.

## Incremental analysis

A version records a tree hash (SHA-256 over sorted path/content-hash pairs)
and its parent (the previous ready version). Parsing is cached per
organization by `(content hash, language, parser fingerprint)`; a parser or
extractor upgrade changes the fingerprint and invalidates the cache.
Resolution always runs over the whole version, because a change in one file
can change how references in unchanged files resolve.
