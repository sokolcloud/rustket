//! A minimal XML and plist reader for the template tests (test-only).
//! Shared by path: `crates/rustket-core/tests/policy_templates.rs` and the
//! tests of `crates/rustket` that feed the generated templates through the
//! app's readers.

#[derive(Debug, Clone, PartialEq)]
pub struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<El>,
    pub text: String,
}

impl El {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn children(&self, name: &str) -> Vec<&El> {
        self.children.iter().filter(|c| c.name == name).collect()
    }

    pub fn child(&self, name: &str) -> &El {
        let found = self.children(name);
        assert_eq!(
            found.len(),
            1,
            "<{}> has {} <{name}>",
            self.name,
            found.len()
        );
        found[0]
    }

    pub fn names(&self) -> Vec<&str> {
        self.children.iter().map(|c| c.name.as_str()).collect()
    }

    pub fn walk<'a>(&'a self, out: &mut Vec<&'a El>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
}

pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let end = rest[i..]
            .find(';')
            .map(|e| i + e)
            .expect("entity without ;");
        out.push(match &rest[i + 1..end] {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            other => panic!("unknown entity &{other};"),
        });
        rest = &rest[end + 1..];
    }
    assert!(!rest.contains('<'), "raw < in text");
    out.push_str(rest);
    out
}

/// Parses a well-formed document: declaration, comments and a DOCTYPE in
/// the prolog, then one element. Comments inside elements are skipped.
pub fn parse_xml(doc: &str) -> El {
    let mut rest = doc;
    assert!(rest.starts_with("<?xml "), "XML declaration first");
    rest = &rest[rest.find("?>").unwrap() + 2..];
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("<!--") {
            let end = r.find("-->").unwrap();
            assert!(!r[..end].contains("--"), "-- inside a comment");
            rest = &r[end + 3..];
        } else if rest.starts_with("<!DOCTYPE") {
            rest = &rest[rest.find('>').unwrap() + 1..];
        } else {
            break;
        }
    }
    let (el, after) = element(rest);
    assert!(after.trim().is_empty(), "content after the root element");
    el
}

pub fn element(s: &str) -> (El, &str) {
    let s = s.strip_prefix('<').expect("element");
    let tag_end = s.find('>').unwrap();
    let tag = &s[..tag_end];
    let (tag, empty) = match tag.strip_suffix('/') {
        Some(t) => (t, true),
        None => (tag, false),
    };
    let mut parts = tag.trim();
    let name_end = parts
        .find(|c: char| c.is_whitespace())
        .unwrap_or(parts.len());
    let name = parts[..name_end].to_owned();
    parts = parts[name_end..].trim_start();
    let mut attrs = Vec::new();
    while !parts.is_empty() {
        let eq = parts.find('=').unwrap();
        let key = parts[..eq].trim().to_owned();
        let v = parts[eq + 1..]
            .trim_start()
            .strip_prefix('"')
            .expect("quoted");
        let close = v.find('"').unwrap();
        assert!(
            attrs.iter().all(|(k, _): &(String, String)| *k != key),
            "repeated attribute {key}"
        );
        attrs.push((key, unescape(&v[..close])));
        parts = v[close + 1..].trim_start();
    }
    let mut el = El {
        name,
        attrs,
        children: Vec::new(),
        text: String::new(),
    };
    let mut rest = &s[tag_end + 1..];
    if empty {
        return (el, rest);
    }
    loop {
        let lt = rest.find('<').unwrap();
        el.text.push_str(&unescape(&rest[..lt]));
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').unwrap();
            assert_eq!(&r[..end], el.name, "mismatched end tag");
            el.text = el.text.trim().to_owned();
            return (el, &r[end + 1..]);
        }
        if let Some(r) = rest.strip_prefix("<!--") {
            let end = r.find("-->").unwrap();
            assert!(!r[..end].contains("--"), "-- inside a comment");
            rest = &r[end + 3..];
            continue;
        }
        let (child, r) = element(rest);
        el.children.push(child);
        rest = r;
    }
}

/// A plist value, as the test reads it.
#[derive(Debug, Clone, PartialEq)]
pub enum Plist {
    Bool(bool),
    Integer(i64),
    String(String),
    Array(Vec<Plist>),
    Dict(Vec<(String, Plist)>),
}

pub fn plist_value(el: &El) -> Plist {
    match el.name.as_str() {
        "true" => Plist::Bool(true),
        "false" => Plist::Bool(false),
        "integer" => Plist::Integer(el.text.parse().unwrap()),
        "string" => Plist::String(el.text.clone()),
        "array" => Plist::Array(el.children.iter().map(plist_value).collect()),
        "dict" => Plist::Dict(plist_pairs(&el.children)),
        other => panic!("unexpected plist element {other}"),
    }
}

pub fn plist_pairs(children: &[El]) -> Vec<(String, Plist)> {
    assert!(children.len().is_multiple_of(2), "key without value");
    children
        .chunks(2)
        .map(|pair| {
            assert_eq!(pair[0].name, "key");
            (pair[0].text.clone(), plist_value(&pair[1]))
        })
        .collect()
}

pub fn get<'a>(dict: &'a Plist, key: &str) -> &'a Plist {
    let Plist::Dict(pairs) = dict else {
        panic!("not a dict")
    };
    let found: Vec<&Plist> = pairs
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v)
        .collect();
    assert_eq!(found.len(), 1, "{key}");
    found[0]
}

pub fn single(array: &Plist) -> &Plist {
    match array {
        Plist::Array(items) if items.len() == 1 => &items[0],
        other => panic!("not a one-item array: {other:?}"),
    }
}
