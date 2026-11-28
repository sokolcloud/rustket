//! The templates for administrators (docs/policy.md section 14): the
//! checked-in files equal the generator's output, and each template matches
//! what the readers of its system accept.

#![allow(clippy::unwrap_used)]

mod admin_templates;
#[path = "admin_templates/xml.rs"]
mod xml;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use admin_templates::texts::{self, Language};
use admin_templates::{Example, example};
use rustket_core::policy::{
    Class, Context, Key, Os, RawValue, RowStatus, Snapshot, SourceId, Trust, ValueType,
    read_policy_dir, resolve, specs,
};
use xml::{El, Plist, get, parse_xml, plist_pairs, plist_value, single};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A checked-in file with `\n` line endings (Windows checkouts get CRLF).
fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .replace("\r\n", "\n")
}

/// The versions the registry's keys were introduced in, without repeats.
fn introduced_versions() -> BTreeSet<&'static str> {
    specs().iter().map(|s| s.introduced).collect()
}

/// The newest `introduced` value: a client of that version knows every key.
fn newest_version() -> &'static str {
    specs()
        .iter()
        .map(|s| s.introduced)
        .max_by_key(|v| version(v))
        .unwrap()
}

fn generated(rel: &str) -> String {
    admin_templates::generate()
        .into_iter()
        .find(|(path, _)| path == rel)
        .unwrap_or_else(|| panic!("{rel} is not generated"))
        .1
}

#[test]
fn checked_in_files_are_up_to_date() {
    let files = admin_templates::generate();
    assert_eq!(files.len(), 9, "section 14's file table");
    for (rel, content) in files {
        assert!(
            read(&rel) == content,
            "{rel} differs from the generator's output; regenerate from the repository root: {}",
            admin_templates::REGENERATE
        );
    }
}

/// Nothing else lives in packaging/policy/: a stale file would ship in the
/// release archive.
#[test]
fn no_stale_template_files() {
    let generated: BTreeSet<String> = admin_templates::generate()
        .into_iter()
        .map(|(rel, _)| rel)
        .filter(|rel| rel.starts_with(admin_templates::DIR))
        .collect();
    let mut found = BTreeSet::new();
    let mut dirs = vec![root().join(admin_templates::DIR)];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                let rel = path.strip_prefix(root()).unwrap();
                found.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    assert_eq!(found, generated);
}

// --- texts ------------------------------------------------------------------

/// Section 11: the Mac App Store binary must not contain these.
const MARKERS: [&str; 4] = [
    "api.github.com",
    "install-kind",
    "SHA256SUMS",
    "didDismissAlert",
];

/// Plain single-spaced text that ADML can show raw.
fn check_plain(what: &str, text: &str) {
    assert!(!text.is_empty(), "{what}: empty");
    assert_eq!(text.trim(), text, "{what}: leading or trailing space");
    assert!(
        !text.contains("  ")
            && !text.contains('\n')
            && !text.contains('\r')
            && !text.contains('\t'),
        "{what}: not single-spaced"
    );
    for markup in ["`", "**", "](", "<", ">", "&", "#", "__"] {
        assert!(!text.contains(markup), "{what}: markup {markup:?}");
    }
    assert!(!text.chars().any(char::is_control), "{what}: control");
    for m in MARKERS {
        assert!(!text.contains(m), "{what}: contains {m}");
    }
}

#[test]
fn every_key_has_texts() {
    for s in specs() {
        let t = texts::key_texts(s.key).unwrap_or_else(|| panic!("{}: no admin texts", s.name));
        check_plain(&format!("{} not_set", s.name), t.not_set);
        for lang in Language::ALL {
            let t = t.get(lang);
            let what = format!("{} {lang:?}", s.name);
            check_plain(&format!("{what} name"), t.name);
            check_plain(&format!("{what} explain"), t.explain);
            assert!(
                t.explain.chars().count() <= 700,
                "{what}: explanation of {} characters",
                t.explain.chars().count()
            );
            if let Some(w) = t.windows {
                check_plain(&format!("{what} windows"), w);
            }
            // The Group Policy sentence included.
            let adml = admin_templates::adml_explain(s.key, lang);
            assert!(
                adml.chars().count() <= 700,
                "{what}: ADML explanation of {} characters",
                adml.chars().count()
            );
            assert_eq!(
                t.label.is_some(),
                s.value_type != ValueType::Bool,
                "{what}: a label exactly for keys with a value field"
            );
            if let Some(label) = t.label {
                check_plain(&format!("{what} label"), label);
            }
        }
        if let ValueType::Enum(values) = s.value_type {
            for v in values {
                let e = texts::enum_text(s.key, v).unwrap_or_else(|| panic!("{}={v}", s.name));
                for lang in Language::ALL {
                    check_plain(&format!("{}={v} {lang:?}", s.name), e.get(lang));
                }
            }
        }
    }
    assert_eq!(texts::KEYS.len(), specs().len(), "texts of unknown keys");
    for lang in Language::ALL {
        check_plain("category", texts::category(lang));
        for v in introduced_versions() {
            check_plain("supportedOn", &texts::supported_on(lang, v));
        }
        let (name, description) = texts::adml_header(lang);
        check_plain("ADML name", name);
        check_plain("ADML description", description);
    }
}

/// The Russian names differ from the English ones (a missing translation
/// shows up as a copy).
#[test]
fn russian_texts_are_translated() {
    for t in texts::KEYS {
        assert_ne!(t.en.name, t.ru.name, "{}", t.key);
        assert_ne!(t.en.explain, t.ru.explain, "{}", t.key);
        assert_eq!(t.en.windows.is_some(), t.ru.windows.is_some(), "{}", t.key);
        if t.en.windows.is_some() {
            assert_ne!(t.en.windows, t.ru.windows, "{}", t.key);
        }
        if t.en.label.is_some() {
            assert_ne!(t.en.label, t.ru.label, "{}", t.key);
        }
    }
}

/// The section 3 facts every explanation states, checked in English.
#[test]
fn explanations_name_applicability_and_effect() {
    for s in specs() {
        let en = texts::key_texts(s.key).unwrap().en.explain;
        let a = &s.applicability;
        if a.mas && !a.needs_self_update {
            assert!(en.contains("Applies on all systems."), "{}", s.name);
        } else {
            assert!(en.contains("Mac App Store version"), "{}", s.name);
            assert!(!en.contains("Applies on all systems."), "{}", s.name);
        }
        // The shared text holds on every system; channel facts go into
        // the sentence of their own format.
        for channel in ["user-scope", "HKLM", "Group Policy", "registry"] {
            assert!(!en.contains(channel), "{}: {channel}", s.name);
        }
        let windows = texts::key_texts(s.key).unwrap().en.windows;
        if s.class == Class::TrustExpanding {
            assert!(
                windows.is_some_and(|w| w.contains("a user-scope value is ignored")),
                "{}",
                s.name
            );
            assert!(
                admin_templates::adml_explain(s.key, Language::En)
                    .contains("a user-scope value is ignored")
            );
        }
        // The JSON schemas speak in values, not Group Policy states.
        for linux in [false, true] {
            let d = admin_templates::json_description(s, linux);
            for term in [
                "Enabled",
                "Disabled",
                "configured:",
                "Not configured",
                "user-scope",
            ] {
                assert!(!d.contains(term), "{} {linux}: {term} in {d}", s.name);
            }
            assert_eq!(
                d.contains("ownership check"),
                linux && s.class == Class::TrustExpanding,
                "{}",
                s.name
            );
        }
    }
    let rhttps = specs().iter().find(|s| s.key == Key::RequireHttps).unwrap();
    assert!(admin_templates::json_description(rhttps, true).starts_with("true: "));
    let allowed = specs()
        .iter()
        .find(|s| s.key == Key::AllowedServers)
        .unwrap();
    assert!(
        admin_templates::json_description(allowed, true)
            .contains("Not set: every server is allowed.")
    );
    let en = |k: Key| texts::key_texts(k).unwrap().en.explain;
    assert!(en(Key::HardwareAcceleration).contains("next start"));
    assert!(en(Key::AllowPasswordLogin).contains("next sign-in"));
    assert!(en(Key::AllowTokenLogin).contains("next sign-in"));
    // Rocket.Chat has no switch that refuses passwords; tokens are forbidden
    // by the permission.
    assert!(en(Key::AllowPasswordLogin).contains("the server still accepts passwords"));
    assert!(en(Key::AllowTokenLogin).contains("create-personal-access-tokens"));
    for k in [Key::AllowPasswordLogin, Key::AllowTokenLogin] {
        let t = texts::key_texts(k).unwrap();
        assert!(!t.en.explain.contains("server's settings"), "{k:?}");
        assert!(!t.ru.explain.contains("настройках сервера"), "{k:?}");
    }
    assert!(
        texts::key_texts(Key::AllowedServers)
            .unwrap()
            .en
            .windows
            .unwrap()
            .contains("list the managed servers")
    );
    assert!(en(Key::AllowedServers).contains("Disabled or not configured: every server"));
    assert!(en(Key::ManagedServers).contains("Require HTTPS"));
    assert!(en(Key::AllowedServers).contains("*."));
    assert!(en(Key::AllowCertificateExceptions).contains("next start of Rustket"));
    assert!(en(Key::UpdateMode).contains("built-in updater"));
}

/// Group Policy states without a JSON value stay as they are, so the check
/// above fails instead of a wrong "Not set".
#[test]
fn json_terms_never_invent_not_set() {
    let text = "Enabled: a. Disabled: b. Enabled or not configured: c.";
    assert_eq!(admin_templates::json_terms(text, false), text);
    assert_eq!(
        admin_templates::json_terms(text, true),
        "true: a. false: b. true or not set: c."
    );
    assert_eq!(
        admin_templates::json_terms("Disabled or not configured: x. Not configured: y.", false),
        "Not set: x. Not set: y."
    );
}

// --- the XML reader (admin_templates/xml.rs) -------------------------------

#[test]
fn xml_reader_rejects_broken_documents() {
    let ok = parse_xml("<?xml version=\"1.0\"?>\n<a x=\"1 &amp; 2\"><b/>t&lt;</a>\n");
    assert_eq!(ok.attr("x"), Some("1 & 2"));
    assert_eq!(ok.text, "t<");
    for bad in [
        "<?xml version=\"1.0\"?><a><b></a>",
        "<?xml version=\"1.0\"?><a></a><c/>",
        "<?xml version=\"1.0\"?><!-- a -- b --><a/>",
        "<?xml version=\"1.0\"?><a x=\"1\" x=\"2\"/>",
    ] {
        assert!(
            std::panic::catch_unwind(|| parse_xml(bad)).is_err(),
            "{bad}"
        );
    }
}

// --- ADMX and ADML ----------------------------------------------------------

/// `$(string.X)` → `X`.
fn string_ref(v: &str) -> &str {
    v.strip_prefix("$(string.")
        .and_then(|v| v.strip_suffix(')'))
        .unwrap_or_else(|| panic!("not a string reference: {v}"))
}

fn admx() -> El {
    parse_xml(&generated(admin_templates::ADMX))
}

#[test]
fn admx_header() {
    let x = admx();
    assert_eq!(x.name, "policyDefinitions");
    assert_eq!(
        x.attr("xmlns"),
        Some("http://schemas.microsoft.com/GroupPolicy/2006/07/PolicyDefinitions")
    );
    let rev = admin_templates::REVISION;
    assert_eq!(x.attr("revision"), Some(format!("{rev}.0").as_str()));
    assert_eq!(x.attr("schemaVersion"), Some("1.0"));
    // The schema's order of the children.
    assert_eq!(
        x.names(),
        [
            "policyNamespaces",
            "resources",
            "supportedOn",
            "categories",
            "policies"
        ]
    );
    let ns = x.child("policyNamespaces");
    assert_eq!(ns.names(), ["target"], "only a target, no using");
    let target = ns.child("target");
    assert_eq!(target.attr("prefix"), Some("rustket"));
    assert_eq!(
        target.attr("namespace"),
        Some(format!("Rustket.Policies.R{rev}").as_str())
    );
    assert_eq!(
        x.child("resources").attr("minRequiredRevision"),
        Some(format!("{rev}.0").as_str())
    );
    let defs = x.child("supportedOn").child("definitions");
    let names: BTreeSet<&str> = defs
        .children("definition")
        .iter()
        .map(|d| d.attr("name").unwrap())
        .collect();
    let want: Vec<String> = introduced_versions()
        .into_iter()
        .map(admin_templates::supported_id)
        .collect();
    assert_eq!(names, want.iter().map(String::as_str).collect());
    assert_eq!(defs.children("definition").len(), want.len());
    let cats = x.child("categories");
    assert_eq!(cats.children.len(), 1);
    assert_eq!(cats.child("category").attr("name"), Some("Rustket"));
}

/// Every key: one policy with the class, element and registry location the
/// Windows reader expects (`crates/rustket/src/policy/registry.rs`).
#[test]
fn admx_policies_match_the_windows_reader() {
    let x = admx();
    let policies = x.child("policies");
    assert_eq!(policies.children.len(), specs().len());
    for s in specs() {
        let found: Vec<&El> = policies
            .children("policy")
            .into_iter()
            .filter(|p| p.attr("name") == Some(s.name))
            .collect();
        assert_eq!(found.len(), 1, "{}: one policy", s.name);
        let p = found[0];
        let class = if s.class == Class::TrustExpanding {
            "Machine"
        } else {
            "Both"
        };
        assert_eq!(p.attr("class"), Some(class), "{}", s.name);
        assert_eq!(
            p.attr("key"),
            Some(r"Software\Policies\Rustket"),
            "{}",
            s.name
        );
        for attr in ["displayName", "explainText", "presentation"] {
            assert!(p.attr(attr).is_some(), "{}: {attr}", s.name);
        }
        assert_eq!(
            p.attr("presentation"),
            Some(format!("$(presentation.{})", s.name).as_str())
        );
        assert_eq!(p.child("parentCategory").attr("ref"), Some("Rustket"));
        assert_eq!(
            p.child("supportedOn").attr("ref"),
            Some(admin_templates::supported_id(s.introduced).as_str())
        );
        match s.value_type {
            ValueType::Bool => {
                assert_eq!(
                    p.names(),
                    [
                        "parentCategory",
                        "supportedOn",
                        "enabledValue",
                        "disabledValue"
                    ]
                );
                assert_eq!(p.attr("valueName"), Some(s.name));
                for (el, v) in [("enabledValue", "1"), ("disabledValue", "0")] {
                    let d = p.child(el).child("decimal");
                    assert_eq!(d.attr("value"), Some(v), "{}", s.name);
                }
            }
            other => {
                assert_eq!(p.names(), ["parentCategory", "supportedOn", "elements"]);
                assert_eq!(p.attr("valueName"), None, "{}", s.name);
                let elements = p.child("elements");
                assert_eq!(elements.children.len(), 1, "{}", s.name);
                let e = &elements.children[0];
                assert_eq!(e.attr("id"), Some(s.name));
                match other {
                    ValueType::Enum(values) => {
                        assert_eq!(e.name, "enum");
                        assert_eq!(e.attr("valueName"), Some(s.name));
                        // The policy's key applies: no key of its own.
                        assert_eq!(e.attr("key"), None, "{}", s.name);
                        let items = e.children("item");
                        assert_eq!(items.len(), values.len());
                        for (item, v) in items.iter().zip(values) {
                            assert_eq!(item.child("value").child("string").text, *v);
                            assert!(item.attr("displayName").is_some());
                        }
                    }
                    ValueType::String => {
                        assert_eq!(e.name, "text");
                        assert_eq!(e.attr("valueName"), Some(s.name));
                        assert_eq!(e.attr("key"), None, "{}", s.name);
                        // REG_SZ, the type the reader maps without expansion.
                        assert_eq!(e.attr("expandable"), None, "{}", s.name);
                    }
                    ValueType::StringList => {
                        assert_eq!(e.name, "list");
                        // The subkey named like the key, numbered values.
                        assert_eq!(
                            e.attr("key"),
                            Some(format!(r"Software\Policies\Rustket\{}", s.name).as_str())
                        );
                        assert_eq!(e.attr("valuePrefix"), Some(""));
                        assert_eq!(e.attr("valueName"), None);
                        assert_eq!(e.attr("additive"), None);
                        // Numbered values only (no names chosen by the
                        // administrator), REG_SZ.
                        assert_eq!(e.attr("explicitValue"), None, "{}", s.name);
                        assert_eq!(e.attr("expandable"), None, "{}", s.name);
                    }
                    ValueType::Bool => unreachable!(),
                }
            }
        }
    }
}

/// Every `$(string.X)` and `$(presentation.X)` of the ADMX resolves in both
/// ADML files, and no ADML id is unused.
#[test]
fn adml_matches_the_admx() {
    let x = admx();
    let mut all = Vec::new();
    x.walk(&mut all);
    let mut strings = BTreeSet::new();
    let mut presentations = BTreeSet::new();
    for el in &all {
        for (k, v) in &el.attrs {
            if let Some(id) = v.strip_prefix("$(presentation.") {
                assert_eq!(k, "presentation");
                presentations.insert(id.strip_suffix(')').unwrap().to_owned());
            } else if v.starts_with("$(") {
                strings.insert(string_ref(v).to_owned());
            }
        }
    }
    // Element ids each presentation refers to, with the control their kind
    // needs.
    let control = |element: &str| match element {
        "text" => "textBox",
        "enum" => "dropdownList",
        "list" => "listBox",
        "decimal" => "decimalTextBox",
        "boolean" => "checkBox",
        other => panic!("element {other}"),
    };
    let ids: BTreeMap<String, Vec<(String, &str)>> = x
        .child("policies")
        .children("policy")
        .iter()
        .map(|p| {
            let ids = p
                .children("elements")
                .iter()
                .flat_map(|e| e.children.iter())
                .map(|e| (e.attr("id").unwrap().to_owned(), control(&e.name)))
                .collect();
            (p.attr("name").unwrap().to_owned(), ids)
        })
        .collect();
    for lang in Language::ALL {
        let rel = admin_templates::adml(lang);
        let doc = parse_xml(&generated(&rel));
        assert_eq!(doc.name, "policyDefinitionResources", "{rel}");
        assert_eq!(
            doc.attr("revision"),
            Some(format!("{}.0", admin_templates::REVISION).as_str())
        );
        assert_eq!(doc.attr("schemaVersion"), Some("1.0"));
        assert_eq!(doc.names(), ["displayName", "description", "resources"]);
        let res = doc.child("resources");
        assert_eq!(res.names(), ["stringTable", "presentationTable"], "{rel}");
        let table: Vec<(String, String)> = res
            .child("stringTable")
            .children
            .iter()
            .map(|s| {
                assert_eq!(s.name, "string");
                (s.attr("id").unwrap().to_owned(), s.text.clone())
            })
            .collect();
        let defined: BTreeSet<String> = table.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(defined.len(), table.len(), "{rel}: repeated string id");
        assert_eq!(defined, strings, "{rel}: string ids");
        for (id, text) in &table {
            check_plain(&format!("{rel} {id}"), text);
        }
        let pres = res.child("presentationTable");
        let defined: BTreeSet<String> = pres
            .children
            .iter()
            .map(|p| p.attr("id").unwrap().to_owned())
            .collect();
        assert_eq!(defined.len(), pres.children.len());
        assert_eq!(defined, presentations, "{rel}: presentation ids");
        for p in &pres.children {
            let id = p.attr("id").unwrap();
            let refs: Vec<(String, &str)> = p
                .children
                .iter()
                .map(|c| (c.attr("refId").unwrap().to_owned(), c.name.as_str()))
                .collect();
            assert_eq!(&refs, &ids[id], "{rel}: presentation {id} (refId, control)");
            for c in &p.children {
                let label = match c.name.as_str() {
                    "textBox" => c.child("label").text.clone(),
                    "dropdownList" | "listBox" => c.text.clone(),
                    other => panic!("{rel}: unexpected {other}"),
                };
                check_plain(&format!("{rel} {id} label"), &label);
            }
        }
    }
}

/// What Intune's ADMX import refuses, and its size limit.
#[test]
fn intune_limits() {
    let admx = generated(admin_templates::ADMX);
    assert!(!admx.contains("comboBox"));
    assert!(!admx.contains("<using"));
    for (rel, content) in admin_templates::generate() {
        assert!(content.len() < 1024 * 1024, "{rel} is over 1 MB");
    }
    assert!(admin_templates::adml(Language::En).ends_with("/en-US/rustket.adml"));
    assert!(root().join(admin_templates::adml(Language::En)).is_file());
}

/// The ids of the ADMX that GPOs and Intune configurations refer to.
fn admx_ids(x: &El) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut all = Vec::new();
    x.walk(&mut all);
    for el in all {
        match el.name.as_str() {
            "policy" => {
                let name = el.attr("name").unwrap();
                ids.insert(format!("policy {name}"));
                for e in el
                    .children("elements")
                    .iter()
                    .flat_map(|e| e.children.iter())
                {
                    ids.insert(format!("element {name}/{}", e.attr("id").unwrap()));
                }
            }
            "category" => {
                ids.insert(format!("category {}", el.attr("name").unwrap()));
            }
            "definition" => {
                ids.insert(format!("supported {}", el.attr("name").unwrap()));
            }
            _ => {}
        }
        for (_, v) in &el.attrs {
            if v.starts_with("$(string.") {
                ids.insert(format!("string {}", string_ref(v)));
            }
        }
    }
    ids
}

/// `MAJOR.MINOR.PATCH` as numbers, for ordering.
fn version(v: &str) -> Vec<u64> {
    v.split('.').map(|n| n.parse().unwrap()).collect()
}

/// Ids an administrator's GPO already refers to never change. The fixture
/// groups them by template revision: a later key needs a new revision, and
/// the namespace follows the last one.
fn check_admx_ids(fixture: &str, x: &El) {
    let mut sections: Vec<(u32, Vec<String>)> = Vec::new();
    for line in fixture.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(n) = line.strip_prefix("revision ") {
            let n: u32 = n.parse().unwrap();
            assert_eq!(
                n as usize,
                sections.len() + 1,
                "revisions are numbered 1, 2, ..."
            );
            sections.push((n, Vec::new()));
            continue;
        }
        let (_, ids) = sections
            .last_mut()
            .expect("ids before the first revision line");
        ids.push(line.to_owned());
    }
    let (last, _) = sections.last().expect("no revision in the fixture");
    let target = x.child("policyNamespaces").child("target");
    assert_eq!(
        target.attr("namespace"),
        Some(format!("Rustket.Policies.R{last}").as_str()),
        "the namespace names the last revision of the fixture"
    );
    assert_eq!(x.attr("revision"), Some(format!("{last}.0").as_str()));
    let current = admx_ids(x);
    let mut listed = BTreeSet::new();
    let mut previous: Option<Vec<u64>> = None;
    for (n, ids) in &sections {
        let mut introduced = BTreeSet::new();
        for id in ids {
            assert!(listed.insert(id.clone()), "{id} is listed twice");
            assert!(
                current.contains(id),
                "{id} was removed or renamed; published templates refer to it"
            );
            if let Some(name) = id.strip_prefix("policy ") {
                let spec = specs().iter().find(|s| s.name == name).unwrap();
                introduced.insert(spec.introduced);
            }
        }
        assert_eq!(
            introduced.len(),
            1,
            "revision {n}: its keys come from one Rustket version (a later key needs a new revision): {introduced:?}"
        );
        let v = version(introduced.first().unwrap());
        if let Some(p) = &previous {
            assert!(
                v > *p,
                "revision {n}: keys older than the revision before it"
            );
        }
        previous = Some(v);
    }
    for id in &current {
        assert!(
            listed.contains(id),
            "{id} is missing from tests/fixtures/policy/admx-ids.txt; a new key's ids go into a new revision section and raise REVISION"
        );
    }
}

#[test]
fn admx_ids_are_stable() {
    let fixture = read("crates/rustket-core/tests/fixtures/policy/admx-ids.txt");
    check_admx_ids(&fixture, &admx());
}

/// The rules of the fixture fail the changes section 14 forbids.
#[test]
fn admx_id_rules_catch_bad_changes() {
    let fixture = read("crates/rustket-core/tests/fixtures/policy/admx-ids.txt");
    let x = admx();
    let fails = |fixture: String, x: El| {
        std::panic::catch_unwind(move || check_admx_ids(&fixture, &x)).is_err()
    };
    // A removed or renamed id.
    let mut renamed = x.clone();
    let policies = renamed
        .children
        .iter_mut()
        .find(|c| c.name == "policies")
        .unwrap();
    for p in &mut policies.children {
        for el in &mut p.children {
            for e in &mut el.children {
                for (k, v) in &mut e.attrs {
                    if k == "id" && v == "UpdateMode" {
                        *v = "Mode".into();
                    }
                }
            }
        }
    }
    assert!(fails(fixture.clone(), renamed));
    // A raised revision without a section for it.
    let mut raised = x.clone();
    raised.attrs.retain(|(k, _)| k != "revision");
    raised.attrs.push(("revision".into(), "2.0".into()));
    let ns = raised
        .children
        .iter_mut()
        .find(|c| c.name == "policyNamespaces")
        .unwrap();
    for (k, v) in &mut ns.children[0].attrs {
        if k == "namespace" {
            *v = "Rustket.Policies.R2".into();
        }
    }
    assert!(fails(fixture.clone(), raised.clone()));
    // A new section whose key is not newer than the revision before it.
    let moved = fixture.replace("policy MinimumVersion\n", "");
    assert!(fails(
        format!("{moved}revision 2\npolicy MinimumVersion\n"),
        raised
    ));
    // An id listed twice.
    assert!(fails(
        format!("{fixture}policy ManagedServers\n"),
        x.clone()
    ));
    assert!(!fails(fixture, x));
}

// --- macOS ------------------------------------------------------------------

/// The macOS reader's mapping (section 4): numbers (a boolean is 1 or 0)
/// to `Integer`, strings, arrays to `List`.
fn mac_raw(v: &Plist) -> RawValue {
    match v {
        Plist::Bool(b) => RawValue::Integer(i64::from(*b)),
        Plist::Integer(i) => RawValue::Integer(*i),
        Plist::String(s) => RawValue::String(s.clone()),
        Plist::Array(items) => RawValue::List(items.iter().map(mac_raw).collect()),
        Plist::Dict(_) => RawValue::Unsupported("dictionary".into()),
    }
}

fn context(os: Os) -> Context {
    Context {
        client_version: newest_version().into(),
        os,
        mas: false,
        self_update: true,
    }
}

/// Every key applied, none invalid or unknown, nothing blocking.
fn assert_all_applied(snapshots: &[Snapshot], os: Os) {
    let policy = resolve(snapshots, &context(os));
    assert!(policy.blocking().is_empty(), "{:?}", policy.blocking());
    assert_eq!(policy.rows().len(), specs().len(), "{:?}", policy.rows());
    for row in policy.rows() {
        assert_eq!(row.status, RowStatus::Applied, "{row:?}");
        assert!(row.dropped.is_empty(), "{row:?}");
    }
    for s in specs() {
        assert!(policy.is_set(s.key), "{}", s.name);
    }
}

fn forced_settings() -> Vec<(String, Plist)> {
    let root = parse_xml(&generated(admin_templates::MOBILECONFIG));
    assert_eq!(root.name, "plist");
    let profile = plist_value(root.child("dict"));
    assert_eq!(
        get(&profile, "PayloadType"),
        &Plist::String("Configuration".into())
    );
    assert_eq!(
        get(&profile, "PayloadScope"),
        &Plist::String("System".into())
    );
    assert_eq!(get(&profile, "PayloadVersion"), &Plist::Integer(1));
    let payload = single(get(&profile, "PayloadContent"));
    assert_eq!(
        get(payload, "PayloadType"),
        &Plist::String("com.apple.ManagedClient.preferences".into())
    );
    for k in ["PayloadUUID", "PayloadIdentifier"] {
        assert_ne!(get(payload, k), get(&profile, k), "{k}");
    }
    let content = get(payload, "PayloadContent");
    let Plist::Dict(domains) = content else {
        panic!("PayloadContent")
    };
    assert_eq!(domains.len(), 1);
    let domain = get(content, admin_templates::DOMAIN);
    let Plist::Dict(kinds) = domain else {
        panic!("domain")
    };
    assert_eq!(kinds.len(), 1, "Forced only");
    let forced = single(get(domain, "Forced"));
    match get(forced, "mcx_preference_settings") {
        Plist::Dict(pairs) => pairs.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn mobileconfig_matches_the_macos_reader() {
    let doc = generated(admin_templates::MOBILECONFIG);
    let comment = &doc[doc.find("<!--").unwrap()..doc.find("-->").unwrap()];
    assert!(comment.contains("Replace before use"));
    // The administrator edits this file.
    assert!(!comment.contains("do not edit"));
    for id in [
        admin_templates::PROFILE_UUID,
        admin_templates::PAYLOAD_UUID,
        admin_templates::PROFILE_ID,
        admin_templates::PAYLOAD_ID,
    ] {
        assert!(comment.contains(id), "{id}");
    }
    assert!(doc.find("<!--").unwrap() < doc.find("<plist").unwrap());
    let settings = forced_settings();
    let names: Vec<&str> = settings.iter().map(|(k, _)| k.as_str()).collect();
    let keys: Vec<&str> = specs().iter().map(|s| s.name).collect();
    assert_eq!(names, keys, "exactly the registry's key names");
    let mut snap = Snapshot::new(SourceId::MacManaged, Trust::Full);
    for (k, v) in &settings {
        snap = snap.with(k, mac_raw(v));
    }
    assert_all_applied(&[snap], Os::MacOs);
}

/// The Intune preference file: the profile's forced pairs, bare.
#[test]
fn intune_plist_is_the_bare_settings() {
    let text = generated(admin_templates::INTUNE_PLIST);
    assert!(!text.contains("<plist") && !text.contains("<dict") && !text.contains("<?xml"));
    let wrapped = parse_xml(&format!("<?xml version=\"1.0\"?><dict>{text}</dict>"));
    assert_eq!(plist_pairs(&wrapped.children), forced_settings());
}

// --- Linux ------------------------------------------------------------------

#[test]
fn linux_example_resolves_through_the_directory_reader() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("50-example.json"),
        generated(admin_templates::LINUX_EXAMPLE),
    )
    .unwrap();
    let snaps = read_policy_dir(dir.path(), None);
    assert_eq!(snaps.len(), 1);
    assert!(snaps[0].error.is_none(), "{:?}", snaps[0].error);
    assert_all_applied(&snaps, Os::Linux);
}

/// The subset of JSON Schema the schema uses.
fn validate(schema: &serde_json::Value, value: &serde_json::Value, at: &str) {
    use serde_json::Value;
    if let Some(ty) = schema.get("type").and_then(Value::as_str) {
        let ok = match ty {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            other => panic!("{at}: type {other}"),
        };
        assert!(ok, "{at}: not {ty}");
    }
    if let Some(variants) = schema.get("enum").and_then(Value::as_array) {
        assert!(variants.contains(value), "{at}: not in enum");
    }
    if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
        for (i, v) in values.iter().enumerate() {
            validate(items, v, &format!("{at}[{i}]"));
        }
    }
    if let (Some(props), Some(obj)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) {
        for (k, v) in obj {
            match props.get(k) {
                Some(p) => validate(p, v, &format!("{at}.{k}")),
                None => assert_ne!(
                    schema.get("additionalProperties"),
                    Some(&Value::Bool(false)),
                    "{at}.{k}: unknown"
                ),
            }
        }
    }
}

fn json(rel: &str) -> serde_json::Value {
    serde_json::from_str(&generated(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn linux_schema_describes_the_example() {
    let schema = json(admin_templates::LINUX_SCHEMA);
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], true);
    check_properties(&schema, true);
    let example = json(admin_templates::LINUX_EXAMPLE);
    validate(&schema, &example, "50-example.json");
    let obj = example.as_object().unwrap();
    assert_eq!(obj.len(), specs().len());
    // A wrong type is caught by the validator.
    let bad = serde_json::json!({ "RequireHttps": "yes" });
    assert!(std::panic::catch_unwind(|| validate(&schema, &bad, "bad")).is_err());
}

/// Every registry key, with its title, description and type.
fn check_properties(schema: &serde_json::Value, linux: bool) {
    let props = schema["properties"].as_object().unwrap();
    assert_eq!(props.len(), specs().len());
    for s in specs() {
        let p = &props[s.name];
        let t = texts::key_texts(s.key).unwrap();
        assert_eq!(p["title"], t.en.name, "{}", s.name);
        assert_eq!(
            p["description"],
            admin_templates::json_description(s, linux),
            "{}",
            s.name
        );
        let (ty, items) = match s.value_type {
            ValueType::Bool => ("boolean", None),
            ValueType::Enum(_) | ValueType::String => ("string", None),
            ValueType::StringList => ("array", Some("string")),
        };
        assert_eq!(p["type"], ty, "{}", s.name);
        assert_eq!(p["items"]["type"].as_str(), items, "{}", s.name);
        match s.value_type {
            ValueType::Enum(values) => assert_eq!(p["enum"], serde_json::json!(values)),
            _ => assert!(p.get("enum").is_none()),
        }
    }
}

#[test]
fn jamf_schema() {
    let schema = json(admin_templates::JAMF_SCHEMA);
    assert_eq!(schema["title"], "Rustket");
    assert!(schema["description"].is_string());
    check_properties(&schema, false);
    let titles = &schema["properties"]["UpdateMode"]["options"]["enum_titles"];
    assert_eq!(titles.as_array().unwrap().len(), 3);
    // Jamf's own form: the profile's values fit it.
    let values: serde_json::Map<String, serde_json::Value> = specs()
        .iter()
        .map(|s| {
            let v = match example(s.key) {
                Example::Bool(b) => serde_json::json!(b),
                Example::Str(x) => serde_json::json!(x),
                Example::List(items) => serde_json::json!(items),
            };
            (s.name.to_owned(), v)
        })
        .collect();
    validate(&schema, &serde_json::Value::Object(values), "jamf");
}

// --- the reference ----------------------------------------------------------

#[test]
fn reference_has_a_section_per_key() {
    let doc = generated(admin_templates::REFERENCE);
    assert!(doc.contains("Do not edit"));
    assert!(doc.contains(admin_templates::REGENERATE));
    // The archive and the packages ship no docs/policy.md: links are
    // absolute.
    assert!(doc.contains(admin_templates::SPEC_URL));
    assert!(!doc.contains("](policy.md)"));
    for rel in [admin_templates::JAMF_SCHEMA, admin_templates::LINUX_SCHEMA] {
        let d = json(rel)["description"].as_str().unwrap().to_owned();
        assert!(d.contains(admin_templates::REFERENCE_URL), "{rel}");
    }
    for s in specs() {
        let head = format!("\n## {}\n", s.name);
        let at = doc.find(&head).unwrap_or_else(|| panic!("{}", s.name));
        let section = &doc[at + head.len()..];
        let section = &section[..section.find("\n## ").unwrap_or(section.len())];
        let since = format!("| Since | {} |", s.introduced);
        for row in [
            "| Type |",
            "| Values |",
            "| Not set |",
            "| Applies to |",
            since.as_str(),
            "| Takes effect |",
            "| Windows |",
            "| macOS |",
            "| Linux |",
            "| Example |",
        ] {
            assert!(section.contains(row), "{}: {row}", s.name);
        }
    }
    for m in MARKERS {
        assert!(!doc.contains(m), "{m}");
    }
}
