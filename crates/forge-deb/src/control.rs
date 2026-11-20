//! Debian control files (deb822): the `Packages` index and a `.deb`'s `control`.

use std::collections::BTreeMap;

/// One paragraph: field names are lower-cased; continuation lines are joined with newlines.
pub type Stanza = BTreeMap<String, String>;

pub fn parse_stanzas(text: &str) -> Vec<Stanza> {
    let mut out = Vec::new();
    let mut cur = Stanza::new();
    let mut last: Option<String> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            last = None;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(k) = &last {
                if let Some(v) = cur.get_mut(k) {
                    v.push('\n');
                    v.push_str(line.trim_start());
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            cur.insert(k.clone(), v.trim().to_string());
            last = Some(k);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A dependency field as groups of alternatives: `a (>= 1) | b, c` is `[[a, b], [c]]`. Versions, architecture
/// qualifiers (`:any`) and build profiles are dropped; an alternative that excludes amd64 is dropped too.
pub fn parse_depends(field: &str) -> Vec<Vec<String>> {
    let mut groups = Vec::new();
    for group in field.split(',') {
        let mut alts = Vec::new();
        for alt in group.split('|') {
            let alt = alt.trim();
            if alt.is_empty() {
                continue;
            }
            // `name:any (>= 1.0) [amd64 arm64] <!nocheck>`
            let name_end = alt.find(|c: char| c == ' ' || c == '(' || c == '[' || c == '<').unwrap_or(alt.len());
            let name = alt[..name_end].split(':').next().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            if let Some(a) = alt.find('[') {
                if let Some(b) = alt[a..].find(']') {
                    let archs: Vec<&str> = alt[a + 1..a + b].split_whitespace().collect();
                    let negated = archs.iter().any(|x| x.starts_with('!'));
                    let listed = archs.iter().any(|x| x.trim_start_matches('!') == "amd64" || x.trim_start_matches('!') == "linux-any");
                    if (negated && listed) || (!negated && !listed) {
                        continue;
                    }
                }
            }
            alts.push(name.to_string());
        }
        if !alts.is_empty() {
            groups.push(alts);
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stanzas_and_continuations() {
        let s = parse_stanzas("Package: a\nDescription: one\n two\n\nPackage: b\nDepends: x\n");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0]["description"], "one\ntwo");
        assert_eq!(s[1]["package"], "b");
    }

    #[test]
    fn depends_alternatives_versions_and_arches() {
        let d = parse_depends("libc6 (>= 2.34), foo:any | bar [amd64], baz [!amd64], qux [arm64] <!nocheck>");
        assert_eq!(d, vec![vec!["libc6"], vec!["foo", "bar"]]);
    }
}
