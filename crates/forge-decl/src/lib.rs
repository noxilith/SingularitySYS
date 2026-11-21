//! Round-trip editing of declaration files.
//!
//! `forge pkg add/remove` edits the `packages` list in place, preserving the
//! user's comments and layout, so the declaration stays the single source of
//! truth.

use toml_edit::{Array, DocumentMut, Item, Value};

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("invalid TOML")]
    Parse(#[from] toml_edit::TomlError),
    #[error("`packages` exists but is not an array of strings")]
    BadPackages,
    #[error("invalid package name `{0}`")]
    BadName(String),
}

/// Outcome of an edit: the new file text and whether anything changed.
#[derive(Debug, PartialEq, Eq)]
pub struct Edited {
    pub text: String,
    pub changed: bool,
}

fn check_name(name: &str) -> Result<(), EditError> {
    let ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'));
    if ok { Ok(()) } else { Err(EditError::BadName(name.into())) }
}

fn packages_mut(doc: &mut DocumentMut) -> Result<&mut Array, EditError> {
    let item = doc.entry("packages").or_insert(Item::Value(Value::Array(Array::new())));
    let arr = item.as_array_mut().ok_or(EditError::BadPackages)?;
    if arr.iter().any(|v| !v.is_str()) {
        return Err(EditError::BadPackages);
    }
    Ok(arr)
}

/// Names in a `packages = """ ... """` text: separated by commas and/or white space; a `#` starts a comment
/// that runs to the end of the line (so the block can carry examples and notes).
pub fn names_in_text(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|l| l.split('#').next().unwrap_or("").split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(String::from).collect::<Vec<_>>())
        .collect()
}

/// A `packages` text value with these lines, printed as a multi-line string:
///
/// ```toml
/// packages = """
///   git,
///   htop,
/// """
/// ```
fn text_repr(lines: &[String]) -> String {
    let mut s = String::from("\"\"\"\n");
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s.push_str("\"\"\"");
    s
}

/// `packages` as a text value made of `lines`, keeping the comment after it on the `packages` line.
fn set_text(doc: &mut DocumentMut, lines: &[String]) {
    let decor = doc.get("packages").and_then(|i| i.as_value()).map(|v| v.decor().clone());
    // Parsing the wanted text gives a value that prints exactly like that (the repr cannot be set directly).
    let fresh: DocumentMut = format!("packages = {}\n", text_repr(lines)).parse().expect("the generated text is valid TOML");
    let mut value = fresh["packages"].as_value().expect("a value").clone();
    if let Some(d) = decor {
        *value.decor_mut() = d;
    }
    doc["packages"] = Item::Value(value);
}

fn is_text(doc: &DocumentMut) -> bool {
    doc.get("packages").and_then(|i| i.as_value()).is_some_and(|v| v.is_str())
}

/// The lines of the text form, comments and all.
fn text_lines(doc: &DocumentMut) -> Vec<String> {
    doc.get("packages").and_then(|i| i.as_str()).map(|s| s.lines().map(String::from).collect()).unwrap_or_default()
}

/// Add `name` to `packages` (no-op if already present). In the text form the new name goes on its own line at the end;
/// every other line, comments included, stays as it is.
pub fn add_package(text: &str, name: &str) -> Result<Edited, EditError> {
    check_name(name)?;
    let mut doc: DocumentMut = text.parse()?;
    if is_text(&doc) {
        let mut lines = text_lines(&doc);
        if names_in_text(&lines.join("\n")).iter().any(|n| n == name) {
            return Ok(Edited { text: text.to_string(), changed: false });
        }
        lines.push(format!("  {name},"));
        set_text(&mut doc, &lines);
        return Ok(Edited { text: doc.to_string(), changed: true });
    }
    let arr = packages_mut(&mut doc)?;
    if arr.iter().any(|v| v.as_str() == Some(name)) {
        return Ok(Edited { text: text.to_string(), changed: false });
    }
    arr.push(name);
    Ok(Edited { text: doc.to_string(), changed: true })
}

/// Remove `name` from `packages` (no-op if absent). In the text form its line goes (or just the name, when the line
/// lists several); comment lines stay.
pub fn remove_package(text: &str, name: &str) -> Result<Edited, EditError> {
    check_name(name)?;
    let mut doc: DocumentMut = text.parse()?;
    if doc.get("packages").is_none() {
        return Ok(Edited { text: text.to_string(), changed: false });
    }
    if is_text(&doc) {
        let lines = text_lines(&doc);
        if !names_in_text(&lines.join("\n")).iter().any(|n| n == name) {
            return Ok(Edited { text: text.to_string(), changed: false });
        }
        let mut out = Vec::new();
        for l in lines {
            let (code, comment) = match l.find('#') {
                Some(i) => (&l[..i], &l[i..]),
                None => (l.as_str(), ""),
            };
            let names = names_in_text(code);
            if !names.iter().any(|n| n == name) {
                out.push(l);
                continue;
            }
            let rest: Vec<String> = names.into_iter().filter(|n| n != name).collect();
            if rest.is_empty() {
                continue;
            }
            let tail = if comment.is_empty() { String::new() } else { format!("  {comment}") };
            out.push(format!("  {},{tail}", rest.join(", ")));
        }
        set_text(&mut doc, &out);
        return Ok(Edited { text: doc.to_string(), changed: true });
    }
    let arr = packages_mut(&mut doc)?;
    let Some(idx) = arr.iter().position(|v| v.as_str() == Some(name)) else {
        return Ok(Edited { text: text.to_string(), changed: false });
    };
    arr.remove(idx);
    Ok(Edited { text: doc.to_string(), changed: true })
}

/// Add a repository to `repos` (no-op if it is already listed).
pub fn add_repo(text: &str, repo: &str) -> Result<Edited, EditError> {
    let mut doc: DocumentMut = text.parse()?;
    let item = doc.entry("repos").or_insert(Item::Value(Value::Array(Array::new())));
    let arr = item.as_array_mut().ok_or(EditError::BadPackages)?;
    if arr.iter().any(|v| v.as_str() == Some(repo)) {
        return Ok(Edited { text: text.to_string(), changed: false });
    }
    arr.push(repo);
    Ok(Edited { text: doc.to_string(), changed: true })
}

/// Flatpak entries are `application.id` or `remote:application.id`.
fn check_flatpak(entry: &str) -> Result<(), EditError> {
    let id = entry.rsplit(':').next().unwrap_or("");
    let ok = id.contains('.') && entry.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    if ok { Ok(()) } else { Err(EditError::BadName(entry.into())) }
}

/// Add a Flatpak application to `flatpaks` (no-op if already listed).
pub fn add_flatpak(text: &str, entry: &str) -> Result<Edited, EditError> {
    check_flatpak(entry)?;
    let mut doc: DocumentMut = text.parse()?;
    let item = doc.entry("flatpaks").or_insert(Item::Value(Value::Array(Array::new())));
    let arr = item.as_array_mut().ok_or(EditError::BadPackages)?;
    if arr.iter().any(|v| v.as_str() == Some(entry)) {
        return Ok(Edited { text: text.to_string(), changed: false });
    }
    arr.push(entry);
    Ok(Edited { text: doc.to_string(), changed: true })
}

/// Remove a Flatpak application from `flatpaks` (no-op if absent).
pub fn remove_flatpak(text: &str, entry: &str) -> Result<Edited, EditError> {
    check_flatpak(entry)?;
    let mut doc: DocumentMut = text.parse()?;
    let Some(arr) = doc.get_mut("flatpaks").and_then(|i| i.as_array_mut()) else {
        return Ok(Edited { text: text.to_string(), changed: false });
    };
    let Some(idx) = arr.iter().position(|v| v.as_str() == Some(entry)) else {
        return Ok(Edited { text: text.to_string(), changed: false });
    };
    arr.remove(idx);
    Ok(Edited { text: doc.to_string(), changed: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_schema::UserDecl;

    #[test]
    fn flatpaks_are_added_removed_and_parse() {
        let src = "hostname = \"x\"\nlibc = \"glibc\"\n# apps\n";
        let a = add_flatpak(src, "org.mozilla.firefox").unwrap();
        assert!(a.changed && a.text.contains("# apps"));
        let b = add_flatpak(&a.text, "org.mozilla.firefox").unwrap();
        assert!(!b.changed);
        let c = add_flatpak(&a.text, "other:com.example.App").unwrap();
        let decl = forge_schema::SystemDecl::parse(&c.text).unwrap();
        assert_eq!(decl.flatpaks, ["org.mozilla.firefox", "other:com.example.App"]);
        let d = remove_flatpak(&c.text, "org.mozilla.firefox").unwrap();
        assert_eq!(forge_schema::SystemDecl::parse(&d.text).unwrap().flatpaks, ["other:com.example.App"]);
        assert!(add_flatpak(src, "not an id").is_err());
    }

    #[test]
    fn adds_to_empty_file() {
        let e = add_package("", "kitty").unwrap();
        assert!(e.changed);
        assert_eq!(UserDecl::parse(&e.text).unwrap().packages, ["kitty"]);
    }

    #[test]
    fn add_is_idempotent() {
        let first = add_package("", "kitty").unwrap();
        let second = add_package(&first.text, "kitty").unwrap();
        assert!(!second.changed);
        assert_eq!(second.text, first.text);
    }

    #[test]
    fn preserves_comments_and_other_keys() {
        let src = "# my machine\nhostname = \"singularity\" # name\n\npackages = [\"a\"] # apps\n";
        let e = add_package(src, "b").unwrap();
        assert!(e.text.contains("# my machine"));
        assert!(e.text.contains("# name"));
        assert!(e.text.contains("# apps"));
        assert!(e.text.contains("hostname = \"singularity\""));
        assert!(e.text.contains("\"b\""));
    }


    #[test]
    fn text_form_gets_one_name_per_line_and_keeps_its_comments() {
        let src = "hostname = \"x\"\npackages = \"\"\"\n  # python,   # an example\n\"\"\" # mine\n";
        let e = add_package(src, "htop").unwrap();
        assert!(e.changed);
        assert!(e.text.contains("packages = \"\"\"\n  # python,   # an example\n  htop,\n\"\"\" # mine"), "{}", e.text);
        let e = add_package(&e.text, "git").unwrap();
        assert!(e.text.contains("  htop,\n  git,\n"), "{}", e.text);
        assert!(!add_package(&e.text, "git").unwrap().changed);
        let names = |t: &str| forge_schema::SystemDecl::parse(&t.replace("hostname = \"x\"", "hostname = \"x\"\nlibc = \"glibc\"")).unwrap().packages;
        assert_eq!(names(&e.text), ["htop", "git"], "commented-out examples are not packages");
        let e = remove_package(&e.text, "htop").unwrap();
        assert_eq!(names(&e.text), ["git"]);
        assert!(e.text.contains("# python,   # an example"), "comments survive: {}", e.text);
        let e = remove_package(&e.text, "git").unwrap();
        assert!(names(&e.text).is_empty() && e.text.contains("# mine"), "{}", e.text);
        assert!(!remove_package(&e.text, "zzz").unwrap().changed);
    }

    #[test]
    fn a_line_with_several_names_loses_only_the_one_removed() {
        let src = "packages = \"\"\"\n  a, b, c  # three\n\"\"\"\n";
        let e = remove_package(src, "b").unwrap();
        assert!(e.text.contains("  a, c,  # three"), "{}", e.text);
    }
    #[test]
    fn removes_package() {
        let e = remove_package("packages = [\"a\", \"b\"]\n", "a").unwrap();
        assert!(e.changed);
        assert_eq!(UserDecl::parse(&e.text).unwrap().packages, ["b"]);
    }

    #[test]
    fn remove_missing_is_noop() {
        assert!(!remove_package("packages = [\"a\"]\n", "z").unwrap().changed);
        assert!(!remove_package("hostname = \"x\"\n", "z").unwrap().changed);
    }

    #[test]
    fn rejects_bad_names_and_shapes() {
        assert!(matches!(add_package("", "a b"), Err(EditError::BadName(_))));
        assert!(matches!(add_package("", ""), Err(EditError::BadName(_))));
        assert!(matches!(add_package("packages = 3\n", "a"), Err(EditError::BadPackages)));
        assert!(matches!(add_package("packages = [1]\n", "a"), Err(EditError::BadPackages)));
    }
}
