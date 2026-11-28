//! Generator of the templates for administrators (docs/policy.md section
//! 14) from the key registry and [`texts`]. Not part of the app: the example
//! `policy_templates` writes the files, the test `policy_templates` compares
//! them with the checked-in copies.
//!
//! Regenerate from the repository root:
//! `cargo run -p rustket-core --example policy_templates -- .`

pub mod texts;

use std::fmt::Write as _;

use rustket_core::policy::{
    Applicability, Class, Key, KeySpec, RefreshClass, StrictValue, ValueType, specs,
};

use texts::{Language, enum_text, key_texts};

/// Template revision: the `R<n>` of the ADMX namespace. Goes up only when
/// keys are added.
pub const REVISION: u32 = 1;

/// The ADMX target namespace and prefix.
pub fn namespace() -> String {
    format!("Rustket.Policies.R{REVISION}")
}
pub const PREFIX: &str = "rustket";

/// Registry key of every policy (section 2), as the Windows reader opens it.
pub const POLICY_KEY: &str = r"Software\Policies\Rustket";
/// The macOS preference domain (`rustket::BUNDLE_ID`).
pub const DOMAIN: &str = "io.github.sokolcloud.rustket";
/// The category of every policy.
pub const CATEGORY: &str = "Rustket";

pub const DIR: &str = "packaging/policy";
pub const ADMX: &str = "packaging/policy/rustket.admx";
pub const MOBILECONFIG: &str = "packaging/policy/rustket.mobileconfig";
pub const INTUNE_PLIST: &str = "packaging/policy/rustket-intune-macos.plist";
pub const JAMF_SCHEMA: &str = "packaging/policy/rustket-jamf-schema.json";
pub const LINUX_SCHEMA: &str = "packaging/policy/policy.schema.json";
pub const LINUX_EXAMPLE: &str = "packaging/policy/50-example.json";
pub const REFERENCE: &str = "docs/policies.md";

pub const REGENERATE: &str = "cargo run -p rustket-core --example policy_templates -- .";
/// The source repository, named where a shipped file cites repository paths.
pub const REPO_URL: &str = "https://github.com/sokolcloud/rustket";
/// Where the reference and the specification are published: the schemas
/// and the release archive do not ship `docs/policy.md`, and a package
/// installs no `docs/` directory.
pub const REFERENCE_URL: &str = "https://github.com/sokolcloud/rustket/blob/main/docs/policies.md";
pub const SPEC_URL: &str = "https://github.com/sokolcloud/rustket/blob/main/docs/policy.md";
/// The note in XML comments, which cannot contain the command's `--`. The
/// file may be copied anywhere (a Central Store, an Intune upload): it names
/// the reference by its file name and URL only.
pub fn xml_note() -> String {
    format!(
        "Generated from the Rustket key registry; do not edit. The reference policies.md (in the policy templates archive of each release, and at {REFERENCE_URL}) describes every key and how to regenerate this file."
    )
}

pub fn adml(lang: Language) -> String {
    format!("{DIR}/{}/rustket.adml", lang.locale())
}

/// An example value, the same in every format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Example {
    Bool(bool),
    Str(&'static str),
    List(&'static [&'static str]),
}

pub fn example(key: Key) -> Example {
    use Example::{Bool, List, Str};
    match key {
        Key::ManagedServers => List(&["https://chat.example.com Example Chat"]),
        Key::AllowUserServers => Bool(false),
        Key::AllowedServers => List(&["https://chat.example.com", "*.example.com"]),
        Key::RequireHttps => Bool(true),
        Key::AllowCertificateExceptions => Bool(false),
        Key::UpdateMode => Str("notify"),
        Key::AllowPasswordLogin => Bool(true),
        Key::AllowTokenLogin => Bool(false),
        Key::AllowSecretsFile => Bool(false),
        Key::NotificationPreview => Bool(false),
        Key::HardwareAcceleration => Bool(true),
        Key::LaunchAtLogin => Bool(true),
        Key::OrganizationName => Str("Example Corp"),
        Key::SupportUrl => Str("https://help.example.com/chat"),
        Key::MinimumVersion => Str("0.4.0"),
    }
}

/// Every generated file: path relative to the repository root, content.
pub fn generate() -> Vec<(String, String)> {
    let mut out = vec![(ADMX.to_owned(), admx())];
    for lang in Language::ALL {
        out.push((adml(lang), adml_file(lang)));
    }
    out.push((MOBILECONFIG.to_owned(), mobileconfig()));
    out.push((INTUNE_PLIST.to_owned(), intune_plist()));
    out.push((JAMF_SCHEMA.to_owned(), jamf_schema()));
    out.push((LINUX_SCHEMA.to_owned(), linux_schema()));
    out.push((LINUX_EXAMPLE.to_owned(), linux_example()));
    out.push((REFERENCE.to_owned(), reference()));
    out
}

// --- escaping -------------------------------------------------------------

/// XML character data and attribute values (double-quoted).
pub fn xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// A JSON string literal.
fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_owned()).to_string()
}

/// Text inside a Markdown table cell or paragraph: no pipes, no markup.
fn md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '|' | '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

// --- shared facts -----------------------------------------------------------

fn texts_of(key: Key) -> &'static texts::KeyTexts {
    // The test `every_key_has_texts` guarantees the entry; a missing one
    // shows up as this panic in the generator.
    key_texts(key).unwrap_or_else(|| panic!("no admin texts for {}", key.name()))
}

fn enum_label(key: Key, value: &str, lang: Language) -> &'static str {
    enum_text(key, value)
        .unwrap_or_else(|| panic!("no admin text for {}={value}", key.name()))
        .get(lang)
}

/// Section 14: class T keys are computer-only.
pub fn admx_class(spec: &KeySpec) -> &'static str {
    match spec.class {
        Class::TrustExpanding => "Machine",
        Class::Restrictive | Class::Cosmetic => "Both",
    }
}

/// `SUPPORTED_Rustket_0_4_0`.
pub fn supported_id(version: &str) -> String {
    format!("SUPPORTED_Rustket_{}", version.replace('.', "_"))
}

/// The versions keys were introduced in, in registry order without repeats.
fn versions() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for s in specs() {
        if !out.contains(&s.introduced) {
            out.push(s.introduced);
        }
    }
    out
}

pub fn explain_id(key: Key) -> String {
    format!("{}_Explain", key.name())
}

pub fn enum_item_id(key: Key, value: &str) -> String {
    format!("{}_{value}", key.name())
}

/// The ADML explanation: the shared text plus the Group Policy sentence.
pub fn adml_explain(key: Key, lang: Language) -> String {
    let t = texts_of(key).get(lang);
    match t.windows {
        Some(w) => format!("{} {w}", t.explain),
        None => t.explain.to_owned(),
    }
}

/// The Group Policy terms of the shared English text, as the values of a
/// JSON or plist key: Enabled is true, Disabled false, not configured
/// absent. For a key with a value only "Disabled or not configured" (the
/// value removed) has a JSON term; its other Enabled and Disabled texts are
/// left as they are, so the check against Group Policy terms fails.
const JSON_TERMS: [(&str, &str, Option<&str>); 5] = [
    ("Enabled or not configured: ", "true or not set: ", None),
    (
        "Disabled or not configured: ",
        "false or not set: ",
        Some("Not set: "),
    ),
    ("Not configured: ", "Not set: ", Some("Not set: ")),
    ("Enabled: ", "true: ", None),
    ("Disabled: ", "false: ", None),
];

/// `text` in the terms of a JSON or plist value ([`JSON_TERMS`]).
pub fn json_terms(text: &str, is_bool: bool) -> String {
    let mut text = text.to_owned();
    for (gpo, bool_term, value_term) in JSON_TERMS {
        let term = if is_bool { Some(bool_term) } else { value_term };
        if let Some(term) = term {
            text = text.replace(gpo, term);
        }
    }
    text
}

/// The description of a key in the JSON schemas (English): the shared
/// text in their own terms; the Linux schema adds the ownership rule of
/// class T keys. Nothing is added for macOS, whose reader takes every
/// forced value alike.
pub fn json_description(s: &KeySpec, linux: bool) -> String {
    let mut text = json_terms(texts_of(s.key).en.explain, s.value_type == ValueType::Bool);
    if linux && s.class == Class::TrustExpanding {
        text.push_str(" Ignored in a file that fails the ownership check.");
    }
    text
}

// --- ADMX and ADML ----------------------------------------------------------

const ADMX_NS: &str = "http://schemas.microsoft.com/GroupPolicy/2006/07/PolicyDefinitions";
const XSD_NS: &str = r#"xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#;

fn file_revision() -> String {
    format!("{REVISION}.0")
}

fn admx() -> String {
    let rev = file_revision();
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    let _ = writeln!(x, "<!-- {} -->", xml_note());
    let _ = writeln!(
        x,
        "<policyDefinitions {XSD_NS} revision=\"{rev}\" schemaVersion=\"1.0\" xmlns=\"{ADMX_NS}\">"
    );
    x.push_str("  <policyNamespaces>\n");
    let _ = writeln!(
        x,
        "    <target prefix=\"{PREFIX}\" namespace=\"{}\" />",
        xml(&namespace())
    );
    x.push_str("  </policyNamespaces>\n");
    let _ = writeln!(x, "  <resources minRequiredRevision=\"{rev}\" />");
    x.push_str("  <supportedOn>\n    <definitions>\n");
    for v in versions() {
        let id = supported_id(v);
        let _ = writeln!(
            x,
            "      <definition name=\"{id}\" displayName=\"$(string.{id})\" />"
        );
    }
    x.push_str("    </definitions>\n  </supportedOn>\n");
    x.push_str("  <categories>\n");
    let _ = writeln!(
        x,
        "    <category name=\"{CATEGORY}\" displayName=\"$(string.{CATEGORY})\" />"
    );
    x.push_str("  </categories>\n  <policies>\n");
    for s in specs() {
        admx_policy(&mut x, s);
    }
    x.push_str("  </policies>\n</policyDefinitions>\n");
    x
}

fn admx_policy(x: &mut String, s: &KeySpec) {
    let name = s.name;
    let mut attrs = format!(
        "name=\"{name}\" class=\"{}\" displayName=\"$(string.{name})\" explainText=\"$(string.{})\" presentation=\"$(presentation.{name})\" key=\"{}\"",
        admx_class(s),
        explain_id(s.key),
        xml(POLICY_KEY)
    );
    if s.value_type == ValueType::Bool {
        let _ = write!(attrs, " valueName=\"{name}\"");
    }
    let _ = writeln!(x, "    <policy {attrs}>");
    let _ = writeln!(x, "      <parentCategory ref=\"{CATEGORY}\" />");
    let _ = writeln!(
        x,
        "      <supportedOn ref=\"{}\" />",
        supported_id(s.introduced)
    );
    match s.value_type {
        ValueType::Bool => {
            x.push_str(
                "      <enabledValue>\n        <decimal value=\"1\" />\n      </enabledValue>\n",
            );
            x.push_str(
                "      <disabledValue>\n        <decimal value=\"0\" />\n      </disabledValue>\n",
            );
        }
        ValueType::Enum(values) => {
            x.push_str("      <elements>\n");
            let _ = writeln!(
                x,
                "        <enum id=\"{name}\" valueName=\"{name}\" required=\"true\">"
            );
            for v in values {
                let _ = writeln!(
                    x,
                    "          <item displayName=\"$(string.{})\">\n            <value>\n              <string>{}</string>\n            </value>\n          </item>",
                    enum_item_id(s.key, v),
                    xml(v)
                );
            }
            x.push_str("        </enum>\n      </elements>\n");
        }
        ValueType::String => {
            x.push_str("      <elements>\n");
            let _ = writeln!(
                x,
                "        <text id=\"{name}\" valueName=\"{name}\" required=\"true\" />"
            );
            x.push_str("      </elements>\n");
        }
        ValueType::StringList => {
            x.push_str("      <elements>\n");
            let _ = writeln!(
                x,
                "        <list id=\"{name}\" key=\"{}\" valuePrefix=\"\" />",
                xml(&format!("{POLICY_KEY}\\{name}"))
            );
            x.push_str("      </elements>\n");
        }
    }
    x.push_str("    </policy>\n");
}

fn adml_file(lang: Language) -> String {
    let rev = file_revision();
    let (display, description) = texts::adml_header(lang);
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    let _ = writeln!(x, "<!-- {} -->", xml_note());
    let _ = writeln!(
        x,
        "<policyDefinitionResources {XSD_NS} revision=\"{rev}\" schemaVersion=\"1.0\" xmlns=\"{ADMX_NS}\">"
    );
    let _ = writeln!(x, "  <displayName>{}</displayName>", xml(display));
    let _ = writeln!(x, "  <description>{}</description>", xml(description));
    x.push_str("  <resources>\n    <stringTable>\n");
    let string = |x: &mut String, id: &str, text: &str| {
        let _ = writeln!(x, "      <string id=\"{id}\">{}</string>", xml(text));
    };
    for v in versions() {
        string(&mut x, &supported_id(v), &texts::supported_on(lang, v));
    }
    string(&mut x, CATEGORY, texts::category(lang));
    for s in specs() {
        let t = texts_of(s.key).get(lang);
        string(&mut x, s.name, t.name);
        string(&mut x, &explain_id(s.key), &adml_explain(s.key, lang));
        if let ValueType::Enum(values) = s.value_type {
            for v in values {
                string(&mut x, &enum_item_id(s.key, v), enum_label(s.key, v, lang));
            }
        }
    }
    x.push_str("    </stringTable>\n    <presentationTable>\n");
    for s in specs() {
        let name = s.name;
        let label = texts_of(s.key).get(lang).label.map(xml).unwrap_or_default();
        match s.value_type {
            ValueType::Bool => {
                let _ = writeln!(x, "      <presentation id=\"{name}\" />");
            }
            ValueType::Enum(_) => {
                let _ = writeln!(
                    x,
                    "      <presentation id=\"{name}\">\n        <dropdownList refId=\"{name}\" noSort=\"true\">{label}</dropdownList>\n      </presentation>"
                );
            }
            ValueType::String => {
                let _ = writeln!(
                    x,
                    "      <presentation id=\"{name}\">\n        <textBox refId=\"{name}\">\n          <label>{label}</label>\n        </textBox>\n      </presentation>"
                );
            }
            ValueType::StringList => {
                let _ = writeln!(
                    x,
                    "      <presentation id=\"{name}\">\n        <listBox refId=\"{name}\">{label}</listBox>\n      </presentation>"
                );
            }
        }
    }
    x.push_str("    </presentationTable>\n  </resources>\n</policyDefinitionResources>\n");
    x
}

// --- macOS ------------------------------------------------------------------

/// Fixed example identifiers of the configuration profile; replaced before
/// use.
pub const PROFILE_UUID: &str = "8C3D6A0E-0000-4000-8000-000000000001";
pub const PAYLOAD_UUID: &str = "8C3D6A0E-0000-4000-8000-000000000002";
pub const PROFILE_ID: &str = "com.example.rustket.policy";
pub const PAYLOAD_ID: &str = "com.example.rustket.policy.preferences";

/// One `<key>` and its value, indented by `indent` spaces.
fn plist_pair(x: &mut String, indent: usize, key: &str, value: Example) {
    let pad = " ".repeat(indent);
    let _ = writeln!(x, "{pad}<key>{}</key>", xml(key));
    match value {
        Example::Bool(true) => {
            let _ = writeln!(x, "{pad}<true/>");
        }
        Example::Bool(false) => {
            let _ = writeln!(x, "{pad}<false/>");
        }
        Example::Str(s) => {
            let _ = writeln!(x, "{pad}<string>{}</string>", xml(s));
        }
        Example::List(items) => {
            let _ = writeln!(x, "{pad}<array>");
            for i in items {
                let _ = writeln!(x, "{pad}  <string>{}</string>", xml(i));
            }
            let _ = writeln!(x, "{pad}</array>");
        }
    }
}

fn plist_string(x: &mut String, indent: usize, key: &str, value: &'static str) {
    plist_pair(x, indent, key, Example::Str(value));
}

fn plist_integer(x: &mut String, indent: usize, key: &str, value: i64) {
    let pad = " ".repeat(indent);
    let _ = writeln!(
        x,
        "{pad}<key>{}</key>\n{pad}<integer>{value}</integer>",
        xml(key)
    );
}

fn mobileconfig() -> String {
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    let _ = writeln!(
        x,
        "<!-- Example configuration profile for Rustket (forced preferences of {DOMAIN}), generated from the Rustket key registry; {REFERENCE_URL} describes every key. Replace before use: both PayloadUUID values ({PROFILE_UUID}, {PAYLOAD_UUID}), both PayloadIdentifier values ({PROFILE_ID}, {PAYLOAD_ID}) and PayloadOrganization; keep only the keys you mean to enforce, with your values. -->"
    );
    x.push_str("<plist version=\"1.0\">\n<dict>\n");
    x.push_str("  <key>PayloadContent</key>\n  <array>\n    <dict>\n");
    x.push_str("      <key>PayloadContent</key>\n      <dict>\n");
    let _ = writeln!(x, "        <key>{DOMAIN}</key>\n        <dict>");
    x.push_str("          <key>Forced</key>\n          <array>\n            <dict>\n");
    x.push_str("              <key>mcx_preference_settings</key>\n              <dict>\n");
    for s in specs() {
        plist_pair(&mut x, 16, s.name, example(s.key));
    }
    x.push_str("              </dict>\n            </dict>\n          </array>\n        </dict>\n      </dict>\n");
    plist_string(&mut x, 6, "PayloadDisplayName", "Rustket policy");
    plist_string(&mut x, 6, "PayloadIdentifier", PAYLOAD_ID);
    plist_string(
        &mut x,
        6,
        "PayloadType",
        "com.apple.ManagedClient.preferences",
    );
    plist_string(&mut x, 6, "PayloadUUID", PAYLOAD_UUID);
    plist_integer(&mut x, 6, "PayloadVersion", 1);
    x.push_str("    </dict>\n  </array>\n");
    plist_string(
        &mut x,
        2,
        "PayloadDescription",
        "Enterprise policy of Rustket, the native Rocket.Chat desktop client.",
    );
    plist_string(&mut x, 2, "PayloadDisplayName", "Rustket");
    plist_string(&mut x, 2, "PayloadIdentifier", PROFILE_ID);
    plist_string(&mut x, 2, "PayloadOrganization", "Example Corp");
    plist_string(&mut x, 2, "PayloadScope", "System");
    plist_string(&mut x, 2, "PayloadType", "Configuration");
    plist_string(&mut x, 2, "PayloadUUID", PROFILE_UUID);
    plist_integer(&mut x, 2, "PayloadVersion", 1);
    x.push_str("</dict>\n</plist>\n");
    x
}

/// Intune's "Preference file": bare key/value pairs, no plist or dict.
fn intune_plist() -> String {
    let mut x = String::new();
    for s in specs() {
        plist_pair(&mut x, 0, s.name, example(s.key));
    }
    x
}

// --- JSON -------------------------------------------------------------------

/// A JSON value with its object keys in insertion order.
enum Json {
    Bool(bool),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }

    fn str(s: &str) -> Json {
        Json::Str(s.to_owned())
    }

    fn render(&self, out: &mut String, indent: usize) {
        let pad = "  ".repeat(indent + 1);
        let end = "  ".repeat(indent);
        match self {
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Str(s) => out.push_str(&json_str(s)),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&pad);
                    item.render(out, indent + 1);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str(&end);
                out.push(']');
            }
            Json::Obj(pairs) if pairs.is_empty() => out.push_str("{}"),
            Json::Obj(pairs) => {
                out.push_str("{\n");
                for (i, (k, v)) in pairs.iter().enumerate() {
                    out.push_str(&pad);
                    out.push_str(&json_str(k));
                    out.push_str(": ");
                    v.render(out, indent + 1);
                    out.push_str(if i + 1 < pairs.len() { ",\n" } else { "\n" });
                }
                out.push_str(&end);
                out.push('}');
            }
        }
    }

    fn file(&self) -> String {
        let mut out = String::new();
        self.render(&mut out, 0);
        out.push('\n');
        out
    }
}

fn example_json(value: Example) -> Json {
    match value {
        Example::Bool(b) => Json::Bool(b),
        Example::Str(s) => Json::str(s),
        Example::List(items) => Json::Arr(items.iter().map(|s| Json::str(s)).collect()),
    }
}

/// The JSON Schema `type` (and its constraints) of a key's value.
fn schema_type(s: &KeySpec) -> Vec<(&'static str, Json)> {
    match s.value_type {
        ValueType::Bool => vec![("type", Json::str("boolean"))],
        ValueType::Enum(values) => vec![
            ("type", Json::str("string")),
            (
                "enum",
                Json::Arr(values.iter().map(|v| Json::str(v)).collect()),
            ),
        ],
        ValueType::String => vec![("type", Json::str("string"))],
        ValueType::StringList => vec![
            ("type", Json::str("array")),
            ("items", Json::obj(vec![("type", Json::str("string"))])),
        ],
    }
}

fn jamf_schema() -> String {
    let props = specs()
        .iter()
        .map(|s| {
            let t = texts_of(s.key).get(Language::En);
            let mut p = vec![
                ("title", Json::str(t.name)),
                ("description", Json::str(&json_description(s, false))),
            ];
            p.extend(schema_type(s));
            if let ValueType::Enum(values) = s.value_type {
                let titles = values
                    .iter()
                    .map(|v| Json::str(enum_label(s.key, v, Language::En)))
                    .collect();
                p.push((
                    "options",
                    Json::obj(vec![("enum_titles", Json::Arr(titles))]),
                ));
            }
            (s.name.to_owned(), Json::obj(p))
        })
        .collect();
    Json::obj(vec![
        ("title", Json::str("Rustket")),
        (
            "description",
            Json::str(&format!(
                "Enterprise policy of Rustket (preference domain {DOMAIN}). Set only the keys you mean to enforce: a key that is not set changes nothing. Reference: {REFERENCE_URL}"
            )),
        ),
        ("properties", Json::Obj(props)),
    ])
    .file()
}

fn linux_schema() -> String {
    let props = specs()
        .iter()
        .map(|s| {
            let t = texts_of(s.key).get(Language::En);
            let mut p = vec![
                ("title", Json::str(t.name)),
                ("description", Json::str(&json_description(s, true))),
            ];
            p.extend(schema_type(s));
            (s.name.to_owned(), Json::obj(p))
        })
        .collect();
    Json::obj(vec![
        (
            "$schema",
            Json::str("https://json-schema.org/draft/2020-12/schema"),
        ),
        ("title", Json::str("Rustket policy file")),
        (
            "description",
            Json::str(&format!(
                "One policy file of /etc/rustket/policies/managed/. Ownership check: the file and the directory are owned by root and not writable by group or others; a file that fails it is read with limited trust. Set only the keys you mean to enforce. Unknown keys are reported by Rustket, not fatal. Reference: {REFERENCE_URL}"
            )),
        ),
        ("type", Json::str("object")),
        ("properties", Json::Obj(props)),
        ("additionalProperties", Json::Bool(true)),
    ])
    .file()
}

fn linux_example() -> String {
    Json::Obj(
        specs()
            .iter()
            .map(|s| (s.name.to_owned(), example_json(example(s.key))))
            .collect(),
    )
    .file()
}

// --- the reference ----------------------------------------------------------

fn example_text(value: Example) -> String {
    let v = match value {
        Example::Bool(b) => serde_json::Value::Bool(b),
        Example::Str(s) => serde_json::Value::from(s),
        Example::List(items) => items.iter().map(|s| serde_json::Value::from(*s)).collect(),
    };
    v.to_string()
}

fn applies_text(a: &Applicability) -> &'static str {
    match (a.mas, a.needs_self_update) {
        (true, false) => "Windows, macOS and Linux, every build",
        (false, false) => "Windows, macOS and Linux; not the Mac App Store version",
        (_, true) => {
            "builds with the built-in updater only (the release packages); not the Mac App Store version"
        }
    }
}

fn effect_text(s: &KeySpec) -> String {
    match (s.key, s.refresh) {
        (Key::AllowCertificateExceptions, _) => format!(
            "The accept action follows within a few minutes; a signed-in server stops using a stored exception when its session next starts, at the latest at the next start of Rustket (specification section 9.2, {SPEC_URL})."
        ),
        (Key::LaunchAtLogin, _) => format!(
            "At the start of Rustket and when the key changes, once the terms are accepted and no blocking state is in force (specification section 9.6, {SPEC_URL})."
        ),
        (_, RefreshClass::Live) => "While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first).".to_owned(),
        (_, RefreshClass::NextStart) => {
            "At the next start of Rustket; Settings show the forced value at once.".to_owned()
        }
        (_, RefreshClass::NextSignIn) => {
            "At the next sign-in; signed-in sessions are not affected.".to_owned()
        }
    }
}

fn values_text(s: &KeySpec) -> String {
    match s.value_type {
        ValueType::Bool => "true or false (the integers 1 and 0 are accepted)".to_owned(),
        ValueType::Enum(values) => format!(
            "one of {} (case does not matter)",
            values
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ValueType::String => match s.key {
            Key::OrganizationName => "one line of at most 80 characters (trimmed)",
            Key::SupportUrl => "an absolute `https://` URL (trimmed)",
            Key::MinimumVersion => "`MAJOR.MINOR.PATCH`, three non-negative integers (trimmed)",
            _ => "a non-empty string (trimmed)",
        }
        .to_owned(),
        ValueType::StringList => {
            "a list of non-empty strings (a single string is a one-item list)".to_owned()
        }
    }
}

fn invalid_text(s: &KeySpec) -> String {
    match s.strict {
        Some(StrictValue::Bool(b)) => format!("treated as `{b}`"),
        Some(StrictValue::UpdateMode(m)) => format!("treated as `{}`", m.as_str()),
        Some(StrictValue::EmptyList) => {
            "treated as an empty list (managed servers only)".to_owned()
        }
        None => "ignored; lower-priority sources are not consulted".to_owned(),
    }
}

fn windows_text(s: &KeySpec) -> String {
    let place = match s.value_type {
        ValueType::Bool => format!("value `{}`, `REG_DWORD` 1 or 0", s.name),
        ValueType::Enum(_) | ValueType::String => format!("value `{}`, `REG_SZ`", s.name),
        ValueType::StringList => format!(
            "subkey `{}` with `REG_SZ` values named 1, 2, 3, ... (a `REG_MULTI_SZ` value `{}` also works)",
            s.name, s.name
        ),
    };
    let scope = match s.class {
        Class::TrustExpanding => {
            "under `HKLM` only (a value under `HKCU` is ignored); ADMX: computer configuration"
        }
        Class::Restrictive | Class::Cosmetic => {
            "under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration"
        }
    };
    format!("`{POLICY_KEY}`: {place}, {scope}")
}

fn mac_text(s: &KeySpec) -> String {
    let kind = match s.value_type {
        ValueType::Bool => "boolean",
        ValueType::Enum(_) | ValueType::String => "string",
        ValueType::StringList => "array of strings",
    };
    match s.class {
        // The reader has no notion of a profile's scope.
        Class::TrustExpanding => {
            format!(
                "{kind}; a forced value applies whether its profile is for the device or the user"
            )
        }
        Class::Restrictive | Class::Cosmetic => kind.to_owned(),
    }
}

fn linux_text(s: &KeySpec) -> String {
    let kind = match s.value_type {
        ValueType::Bool => "JSON boolean",
        ValueType::Enum(_) | ValueType::String => "JSON string",
        ValueType::StringList => "JSON array of strings",
    };
    match s.class {
        Class::TrustExpanding => {
            format!("{kind}; ignored in a file that fails the ownership check")
        }
        Class::Restrictive | Class::Cosmetic => kind.to_owned(),
    }
}

fn reference() -> String {
    let mut d = String::new();
    d.push_str("# Rustket policy reference\n\n");
    let _ = writeln!(
        d,
        "> Generated in the source repository ({REPO_URL}) from the key registry (`crates/rustket-core/src/policy/registry.rs`) and the admin texts (`crates/rustket-core/tests/admin_templates/texts.rs`), like the templates in `packaging/policy/` there. Do not edit: run `{REGENERATE}` from the root of that repository.\n"
    );
    let _ = write!(
        d,
        "Every key below can be set by an administrator; a key that is not set changes nothing, and a forced setting is never written to the user's configuration (`LaunchAtLogin` and `ManagedServers` say what stays when the policy is removed). Policy is configuration management, not a security boundary: enforcement that must hold belongs to the Rocket.Chat server and the identity provider. The engineering specification is {SPEC_URL}.\n\n"
    );
    d.push_str("## Where policy is read\n\n");
    d.push_str("| System | Source | Templates (`packaging/policy/` in the repository, next to this file in the release archive) |\n|---|---|---|\n");
    let _ = writeln!(
        d,
        "| Windows | registry `HKLM\\{POLICY_KEY}` (computer), then `HKCU\\{POLICY_KEY}` (user) | `rustket.admx`, `en-US/rustket.adml`, `ru-RU/rustket.adml` (namespace `{}`) |",
        namespace()
    );
    let _ = writeln!(
        d,
        "| macOS | forced managed preferences of the domain `{DOMAIN}` | `rustket.mobileconfig`, `rustket-intune-macos.plist`, `rustket-jamf-schema.json` |"
    );
    d.push_str("| Linux | root-owned JSON files `/etc/rustket/policies/managed/*.json` | `policy.schema.json`, `50-example.json` |\n\n");
    d.push_str("Key names are identical on every system and matched exactly (the Windows registry ignores case). An invalid value of a restrictive key applies its strict value instead, so a mistake does not lift the restriction; `MinimumVersion` is the exception (an invalid value is ignored). The explanations use the Group Policy terms: Enabled is `true`, Disabled is `false` (for a key with a value, Disabled removes the value), and Not configured means that the key is absent.\n\n");
    d.push_str("## Keys\n\n| Key | Type | Since | Takes effect |\n|---|---|---|---|\n");
    for s in specs() {
        let effect = match (s.key, s.refresh) {
            (Key::AllowCertificateExceptions | Key::LaunchAtLogin, _) => "see the key",
            (_, r) => r.as_str(),
        };
        let _ = writeln!(
            d,
            "| [`{0}`](#{1}) | {2} | {3} | {effect} |",
            s.name,
            s.name.to_ascii_lowercase(),
            s.value_type.describe(),
            s.introduced
        );
    }
    for s in specs() {
        let t = texts_of(s.key);
        let _ = writeln!(d, "\n## {}\n", s.name);
        let _ = writeln!(d, "**{}**\n", md(t.en.name));
        let _ = writeln!(d, "{}\n", md(t.en.explain));
        if let Some(w) = t.en.windows {
            let _ = writeln!(d, "Group Policy template: {}\n", md(w));
        }
        d.push_str("| | |\n|---|---|\n");
        let _ = writeln!(d, "| Type | {} |", s.value_type.describe());
        let _ = writeln!(d, "| Values | {} |", values_text(s));
        if let ValueType::Enum(values) = s.value_type {
            for v in values {
                let _ = writeln!(d, "| `{v}` | {} |", md(enum_label(s.key, v, Language::En)));
            }
        }
        let _ = writeln!(d, "| Not set | {} |", md(t.not_set));
        let _ = writeln!(d, "| Invalid value | {} |", invalid_text(s));
        let _ = writeln!(d, "| Applies to | {} |", applies_text(&s.applicability));
        let _ = writeln!(d, "| Since | {} |", s.introduced);
        let _ = writeln!(d, "| Takes effect | {} |", effect_text(s));
        let _ = writeln!(d, "| Windows | {} |", windows_text(s));
        let _ = writeln!(d, "| macOS | {} |", mac_text(s));
        let _ = writeln!(d, "| Linux | {} |", linux_text(s));
        let _ = writeln!(d, "| Example | `{}` |", example_text(example(s.key)));
    }
    d
}
