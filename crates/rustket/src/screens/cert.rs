//! Display data for an untrusted certificate (fingerprint, subject CN/O).

use rocketchat::CertificateInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertView {
    pub fingerprint: String,
    pub subject: Option<String>,
    pub issuer: Option<String>,
}

impl CertView {
    pub fn new(info: &CertificateInfo) -> Self {
        let (subject, issuer) = names(&info.der).unwrap_or((None, None));
        Self {
            fingerprint: info.fingerprint_hex(),
            subject,
            issuer,
        }
    }
}

/// One DER TLV: (tag, content, rest).
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first & 0x80 == 0 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n]
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | *b as usize);
        (len, &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// Longest certificate name attribute shown, in characters.
const NAME_MAX: usize = 64;

/// Invisible format characters: bidi marks and controls, zero-width characters,
/// soft hyphen, BOM, interlinear annotation and tag characters.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// A name attribute of an unverified certificate for display: invisible
/// characters removed, control characters and whitespace runs (line breaks
/// included) collapsed to one space, at most [`NAME_MAX`] characters, quoted.
/// Returns `None` when nothing visible is left.
fn neutralize(raw: &str) -> Option<String> {
    let mut clean = String::new();
    let mut len = 0usize;
    let mut gap = false;
    let mut truncated = false;
    for c in raw.chars() {
        if is_invisible(c) {
            continue;
        }
        if c.is_control() || c.is_whitespace() {
            gap = len > 0;
            continue;
        }
        // One slot is kept for the space that precedes this character.
        if len + usize::from(gap) >= NAME_MAX {
            truncated = true;
            break;
        }
        if gap {
            clean.push(' ');
            len += 1;
            gap = false;
        }
        // The delimiters stay unambiguous: no curly quotes inside the value.
        clean.push(match c {
            '“' | '”' | '„' | '‟' => '"',
            c => c,
        });
        len += 1;
    }
    if clean.is_empty() {
        return None;
    }
    if truncated {
        clean.push('…');
    }
    Some(format!("“{clean}”"))
}

/// Text of a DER string value; BMPString (0x1e) is UTF-16BE, the rest is read
/// as UTF-8 (a superset of PrintableString and IA5String).
fn string_value(tag: u8, value: &[u8]) -> String {
    if tag == 0x1e {
        let units: Vec<u16> = value
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_be_bytes(*b))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(value).into_owned()
    }
}

/// `CN` (or `O`) of an X.501 Name, neutralized for display.
fn name_text(name: &[u8]) -> Option<String> {
    const CN: &[u8] = &[0x55, 0x04, 0x03];
    const O: &[u8] = &[0x55, 0x04, 0x0a];
    let (mut cn, mut o) = (None, None);
    let mut sets = name;
    while let Some((_, set, rest)) = tlv(sets) {
        sets = rest;
        let mut attrs = set;
        while let Some((_, attr, rest)) = tlv(attrs) {
            attrs = rest;
            let Some((0x06, oid, after)) = tlv(attr) else {
                continue;
            };
            let Some((tag, value, _)) = tlv(after) else {
                continue;
            };
            if oid == CN {
                cn = neutralize(&string_value(tag, value));
            } else if oid == O {
                o = neutralize(&string_value(tag, value));
            }
        }
    }
    match (cn, o) {
        (Some(cn), Some(o)) => Some(format!("{cn} ({o})")),
        (cn, o) => cn.or(o),
    }
}

/// (subject, issuer) of a DER certificate.
fn names(der: &[u8]) -> Option<(Option<String>, Option<String>)> {
    let (_, cert, _) = tlv(der)?;
    let (_, tbs, _) = tlv(cert)?;
    let mut fields = Vec::new();
    let mut rest = tbs;
    while let Some((tag, content, r)) = tlv(rest) {
        fields.push((tag, content));
        rest = r;
    }
    // Skip the optional explicit [0] version.
    let start = usize::from(fields.first().is_some_and(|(t, _)| *t == 0xa0));
    // serial, signature, issuer, validity, subject
    let issuer = fields.get(start + 2).and_then(|(_, c)| name_text(c));
    let subject = fields.get(start + 4).and_then(|(_, c)| name_text(c));
    Some((subject, issuer))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_tolerated() {
        let v = CertView::new(&CertificateInfo::from_der(b"not a certificate"));
        assert_eq!(v.subject, None);
        assert!(v.fingerprint.contains(':'));
    }

    /// DER TLV with a short or long-form length.
    fn wrap(tag: u8, c: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        match c.len() {
            n if n < 0x80 => v.push(n as u8),
            n if n < 0x100 => v.extend_from_slice(&[0x81, n as u8]),
            n => v.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]),
        }
        v.extend_from_slice(c);
        v
    }

    fn attr(oid: &[u8], tag: u8, value: &[u8]) -> Vec<u8> {
        wrap(
            0x31,
            &wrap(0x30, &[wrap(0x06, oid), wrap(tag, value)].concat()),
        )
    }

    fn cn_name(cn: &str) -> Vec<u8> {
        wrap(0x30, &attr(&[0x55, 0x04, 0x03], 0x0c, cn.as_bytes()))
    }

    // SEQ { SEQ(tbs) { [0]{INT 2}, INT 1, SEQ{}, Name(issuer), SEQ{}, Name(subject) } }
    fn cert(issuer: &[u8], subject: &[u8]) -> CertificateInfo {
        let tbs = [
            wrap(0xa0, &wrap(0x02, &[2])),
            wrap(0x02, &[1]),
            wrap(0x30, &[]),
            issuer.to_vec(),
            wrap(0x30, &[]),
            subject.to_vec(),
        ]
        .concat();
        CertificateInfo::from_der(&wrap(0x30, &wrap(0x30, &tbs)))
    }

    #[test]
    fn parses_minimal_tbs() {
        let v = CertView::new(&cert(&cn_name("Issuer CA"), &cn_name("chat.local")));
        assert_eq!(v.subject.as_deref(), Some("“chat.local”"));
        assert_eq!(v.issuer.as_deref(), Some("“Issuer CA”"));
    }

    #[test]
    fn cn_and_o_are_quoted_separately() {
        let name = wrap(
            0x30,
            &[
                attr(&[0x55, 0x04, 0x0a], 0x0c, b"Corp"),
                attr(&[0x55, 0x04, 0x03], 0x0c, b"chat.corp.com (Other Inc)"),
            ]
            .concat(),
        );
        let v = CertView::new(&cert(&cn_name("ca"), &name));
        assert_eq!(
            v.subject.as_deref(),
            Some("“chat.corp.com (Other Inc)” (“Corp”)")
        );
    }

    #[test]
    fn fake_dialog_lines_stay_on_one_line() {
        let cn = "chat.corp.com\n\nIssued by\r\nCorp Root CA\n\nSHA-256 fingerprint\n\
                  AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11\n\
                  Only trust it if the fingerprint matches";
        let v = CertView::new(&cert(&cn_name("ca"), &cn_name(cn)));
        let subject = v.subject.unwrap();
        assert!(!subject.chars().any(|c| c.is_control()));
        assert!(subject.starts_with("“chat.corp.com Issued by Corp Root CA SHA-256 fingerprint "));
        assert!(subject.ends_with("…”"));
        // Quotes, NAME_MAX characters and the ellipsis.
        assert_eq!(subject.chars().count(), NAME_MAX + 3);
    }

    #[test]
    fn neutralize_strips_bidi_and_invisible() {
        assert_eq!(
            neutralize("\u{202E}moc.live\u{202C}\u{2066}x\u{2069}\u{200F}\u{061C}").as_deref(),
            Some("“moc.livex”")
        );
        assert_eq!(
            neutralize("a\u{200B}b\u{FEFF}c\u{00AD}d\u{E0041}").as_deref(),
            Some("“abcd”")
        );
    }

    #[test]
    fn neutralize_collapses_breaks_and_whitespace() {
        assert_eq!(
            neutralize(" a\tb\u{2028}c\u{2029}d\u{0085}e\u{00A0}\u{3000} f\0g\u{1b}[1m ")
                .as_deref(),
            Some("“a b c d e f g [1m”")
        );
        assert_eq!(neutralize(" \n\u{200B}\u{202E}\t"), None);
        assert_eq!(neutralize(""), None);
    }

    #[test]
    fn neutralize_caps_length() {
        let exact = "a".repeat(NAME_MAX);
        assert_eq!(neutralize(&exact), Some(format!("“{exact}”")));
        let long = "a".repeat(NAME_MAX + 1);
        assert_eq!(neutralize(&long), Some(format!("“{exact}…”")));
        // No trailing space before the ellipsis.
        let spaced = format!("{} b", "a".repeat(NAME_MAX - 1));
        assert_eq!(
            neutralize(&spaced),
            Some(format!("“{}…”", "a".repeat(NAME_MAX - 1)))
        );
        // Trailing whitespace alone is not a truncation.
        assert_eq!(
            neutralize(&format!("{exact}  \n")),
            Some(format!("“{exact}”"))
        );
    }

    #[test]
    fn neutralize_keeps_delimiters_unambiguous() {
        assert_eq!(
            neutralize("a.com” (“Trusted CA").as_deref(),
            Some("“a.com\" (\"Trusted CA”")
        );
    }

    #[test]
    fn empty_names_are_dropped() {
        let v = CertView::new(&cert(&cn_name("\u{202E}\n"), &cn_name("  ")));
        assert_eq!(v.subject, None);
        assert_eq!(v.issuer, None);
    }

    #[test]
    fn bmp_string_is_decoded() {
        let utf16: Vec<u8> = "chat\nlocal"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        let name = wrap(0x30, &attr(&[0x55, 0x04, 0x03], 0x1e, &utf16));
        let v = CertView::new(&cert(&cn_name("ca"), &name));
        assert_eq!(v.subject.as_deref(), Some("“chat local”"));
    }
}
