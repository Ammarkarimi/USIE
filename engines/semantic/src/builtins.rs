//! Names provided by the language runtime. References to these are
//! classified as `builtin` rather than reported as unresolved.

pub(crate) fn is_python_builtin(name: &str) -> bool {
    PYTHON_BUILTINS.binary_search(&name).is_ok()
        || name.ends_with("Error")
        || name.ends_with("Exception")
        || name.ends_with("Warning")
}

pub(crate) fn is_ecmascript_global(name: &str) -> bool {
    ECMASCRIPT_GLOBALS.binary_search(&name).is_ok() || name.ends_with("Error")
}

pub(crate) fn is_python_stdlib(top_level: &str) -> bool {
    PYTHON_STDLIB.binary_search(&top_level).is_ok()
}

pub(crate) fn is_node_builtin(name: &str) -> bool {
    NODE_BUILTINS.binary_search(&name).is_ok()
}

/// Method names so common on runtime types (lists, dicts, strings, promises,
/// arrays) that a unique user-defined method of the same name is not credible
/// evidence for a call on a receiver of unknown type.
pub(crate) fn is_ubiquitous_method(name: &str) -> bool {
    UBIQUITOUS_METHODS.binary_search(&name).is_ok()
}

// All tables are sorted for binary search; `tables_are_sorted` enforces it.

const PYTHON_BUILTINS: &[&str] = &[
    "BaseException", "KeyboardInterrupt", "StopAsyncIteration", "StopIteration", "SystemExit", "__import__", "abs",
    "aiter", "all", "anext", "any", "ascii", "bin", "bool", "breakpoint", "bytearray", "bytes", "callable", "chr",
    "classmethod", "compile", "complex", "delattr", "dict", "dir", "divmod", "enumerate", "eval", "exec", "exit",
    "filter", "float", "format", "frozenset", "getattr", "globals", "hasattr", "hash", "help", "hex", "id", "input",
    "int", "isinstance", "issubclass", "iter", "len", "list", "locals", "map", "max", "memoryview", "min", "next",
    "object", "oct", "open", "ord", "pow", "print", "property", "quit", "range", "repr", "reversed", "round", "set",
    "setattr", "slice", "sorted", "staticmethod", "str", "sum", "super", "tuple", "type", "vars", "zip",
];

const ECMASCRIPT_GLOBALS: &[&str] = &[
    "AbortController", "Array", "ArrayBuffer", "BigInt", "Blob", "Boolean", "Buffer", "DataView", "Date", "Event",
    "EventTarget", "File", "FormData", "Function", "Headers", "Intl", "JSON", "Map", "Math", "Number", "Object",
    "Promise", "Proxy", "Reflect", "RegExp", "Request", "Response", "Set", "String", "Symbol", "TextDecoder",
    "TextEncoder", "URL", "URLSearchParams", "Uint8Array", "WeakMap", "WeakRef", "WeakSet", "WebSocket", "Worker",
    "alert", "atob", "btoa", "cancelAnimationFrame", "clearInterval", "clearTimeout", "confirm", "console", "crypto",
    "decodeURI", "decodeURIComponent", "document", "encodeURI", "encodeURIComponent", "fetch", "globalThis",
    "history", "isFinite", "isNaN", "localStorage", "location", "module", "navigator", "parseFloat", "parseInt",
    "performance", "process", "queueMicrotask", "requestAnimationFrame", "require", "sessionStorage", "setImmediate",
    "setInterval", "setTimeout", "structuredClone", "window",
];

const PYTHON_STDLIB: &[&str] = &[
    "__future__", "_thread", "abc", "argparse", "array", "ast", "asyncio", "atexit", "base64", "bdb", "binascii",
    "bisect", "builtins", "bz2", "cProfile", "calendar", "cmath", "cmd", "code", "codecs", "codeop", "collections",
    "colorsys", "compileall", "concurrent", "configparser", "contextlib", "contextvars", "copy", "copyreg", "csv",
    "ctypes", "curses", "dataclasses", "datetime", "dbm", "decimal", "difflib", "dis", "doctest", "email",
    "encodings", "ensurepip", "enum", "errno", "faulthandler", "fcntl", "filecmp", "fileinput", "fnmatch",
    "fractions", "ftplib", "functools", "gc", "getopt", "getpass", "gettext", "glob", "graphlib", "grp", "gzip",
    "hashlib", "heapq", "hmac", "html", "http", "idlelib", "imaplib", "importlib", "inspect", "io", "ipaddress",
    "itertools", "json", "keyword", "linecache", "locale", "logging", "lzma", "mailbox", "marshal", "math",
    "mimetypes", "mmap", "modulefinder", "multiprocessing", "netrc", "numbers", "operator", "optparse", "os",
    "pathlib", "pdb", "pickle", "pickletools", "pkgutil", "platform", "plistlib", "poplib", "posix", "posixpath",
    "pprint", "profile", "pstats", "pty", "pwd", "py_compile", "pyclbr", "pydoc", "queue", "quopri", "random", "re",
    "readline", "reprlib", "resource", "rlcompleter", "runpy", "sched", "secrets", "select", "selectors", "shelve",
    "shlex", "shutil", "signal", "site", "smtplib", "socket", "socketserver", "sqlite3", "ssl", "stat", "statistics",
    "string", "stringprep", "struct", "subprocess", "symtable", "sys", "sysconfig", "syslog", "tabnanny", "tarfile",
    "tempfile", "termios", "test", "textwrap", "threading", "time", "timeit", "tkinter", "token", "tokenize",
    "tomllib", "trace", "traceback", "tracemalloc", "tty", "turtle", "types", "typing", "unicodedata", "unittest",
    "urllib", "uuid", "venv", "warnings", "wave", "weakref", "webbrowser", "winreg", "wsgiref", "xml", "xmlrpc",
    "zipapp", "zipfile", "zipimport", "zlib", "zoneinfo",
];

const NODE_BUILTINS: &[&str] = &[
    "assert", "async_hooks", "buffer", "child_process", "cluster", "console", "constants", "crypto", "dgram",
    "diagnostics_channel", "dns", "domain", "events", "fs", "http", "http2", "https", "inspector", "module", "net",
    "os", "path", "perf_hooks", "process", "punycode", "querystring", "readline", "repl", "stream", "string_decoder",
    "sys", "timers", "tls", "trace_events", "tty", "url", "util", "v8", "vm", "wasi", "worker_threads", "zlib",
];

const UBIQUITOUS_METHODS: &[&str] = &[
    "add", "all", "any", "append", "apply", "bind", "call", "catch", "clear", "close", "concat", "copy", "count",
    "decode", "delete", "encode", "endswith", "entries", "every", "extend", "filter", "finally", "find", "findIndex",
    "flat", "flatMap", "forEach", "format", "get", "has", "includes", "index", "indexOf", "insert", "items", "join",
    "keys", "lower", "map", "match", "next", "open", "pop", "push", "read", "reduce", "remove", "replace", "reverse",
    "send", "set", "shift", "slice", "some", "sort", "splice", "split", "startswith", "strip", "test", "then",
    "toString", "trim", "unshift", "update", "upper", "values", "write",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted() {
        for (name, table) in [
            ("PYTHON_BUILTINS", PYTHON_BUILTINS),
            ("ECMASCRIPT_GLOBALS", ECMASCRIPT_GLOBALS),
            ("PYTHON_STDLIB", PYTHON_STDLIB),
            ("NODE_BUILTINS", NODE_BUILTINS),
            ("UBIQUITOUS_METHODS", UBIQUITOUS_METHODS),
        ] {
            for pair in table.windows(2) {
                assert!(pair[0] < pair[1], "{name}: {:?} must sort before {:?}", pair[0], pair[1]);
            }
        }
    }

    #[test]
    fn lookups() {
        assert!(is_python_builtin("print") && is_python_builtin("ValueError") && !is_python_builtin("requests"));
        assert!(is_ecmascript_global("console") && !is_ecmascript_global("lodash"));
        assert!(is_python_stdlib("os") && !is_python_stdlib("numpy"));
        assert!(is_node_builtin("fs") && !is_node_builtin("express"));
        assert!(is_ubiquitous_method("push") && !is_ubiquitous_method("reconcileLedger"));
    }
}
