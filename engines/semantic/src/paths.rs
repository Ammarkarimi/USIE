//! Repository-relative path helpers. Paths use `/` and never start with `./`.
//! The repository root is the empty string.

/// Directory part of a path ("" for a file at the root).
pub(crate) fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

pub(crate) fn file_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Parent directory, or `None` at the root.
pub(crate) fn parent(dir: &str) -> Option<&str> {
    if dir.is_empty() {
        None
    } else {
        Some(dir_of(dir))
    }
}

/// Join and normalize. Returns `None` when the result escapes the root.
pub(crate) fn join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Whether `path` lies inside directory `dir`.
pub(crate) fn is_under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/')
}

/// `path` relative to `dir`; caller guarantees `is_under(path, dir)`.
pub(crate) fn strip_dir<'p>(path: &'p str, dir: &str) -> &'p str {
    if dir.is_empty() {
        path
    } else {
        &path[dir.len() + 1..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_normalizes_and_guards_root() {
        assert_eq!(join("a/b", "./c").as_deref(), Some("a/b/c"));
        assert_eq!(join("a/b", "../c").as_deref(), Some("a/c"));
        assert_eq!(join("a", "../../c"), None);
        assert_eq!(join("", "x/./y").as_deref(), Some("x/y"));
        assert_eq!(join("a", "..").as_deref(), Some(""));
    }

    #[test]
    fn dirs() {
        assert_eq!(dir_of("a/b/c.py"), "a/b");
        assert_eq!(dir_of("c.py"), "");
        assert_eq!(file_name("a/b/c.py"), "c.py");
        assert_eq!(parent("a/b"), Some("a"));
        assert_eq!(parent("a"), Some(""));
        assert_eq!(parent(""), None);
        assert!(is_under("a/b.py", "a") && is_under("a/b.py", "") && !is_under("ab/c.py", "a"));
        assert_eq!(strip_dir("a/b/c.py", "a"), "b/c.py");
    }
}
