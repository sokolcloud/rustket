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

/// `CN` (or `O`) of an X.501 Name.
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
            let Some((_, value, _)) = tlv(after) else {
                continue;
            };
            let s = String::from_utf8_lossy(value).into_owned();
            if oid == CN {
                cn = Some(s);
            } else if oid == O {
                o = Some(s);
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

    #[test]
    fn parses_minimal_tbs() {
        // SEQ { SEQ(tbs) { [0]{INT 2}, INT 1, SEQ{}, Name(issuer), SEQ{}, Name(subject) } }
        fn wrap(tag: u8, c: &[u8]) -> Vec<u8> {
            let mut v = vec![tag, c.len() as u8];
            v.extend_from_slice(c);
            v
        }
        let name = |cn: &str| {
            let attr = [wrap(0x06, &[0x55, 0x04, 0x03]), wrap(0x0c, cn.as_bytes())].concat();
            wrap(0x30, &wrap(0x31, &wrap(0x30, &attr)))
        };
        let tbs = [
            wrap(0xa0, &wrap(0x02, &[2])),
            wrap(0x02, &[1]),
            wrap(0x30, &[]),
            name("Issuer CA"),
            wrap(0x30, &[]),
            name("chat.local"),
        ]
        .concat();
        let der = wrap(0x30, &wrap(0x30, &tbs));
        let v = CertView::new(&CertificateInfo::from_der(&der));
        assert_eq!(v.subject.as_deref(), Some("chat.local"));
        assert_eq!(v.issuer.as_deref(), Some("Issuer CA"));
    }
}
