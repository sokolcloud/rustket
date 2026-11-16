//! Polls by convention (`docs/polls.md`): a plain message whose first line
//! is `📊 <question>` and whose other lines are options, each starting with
//! a keycap emoji `1️⃣` … `🔟` in order. Votes are reactions with the
//! option's keycap (`:one:` … `:nine:`, `:keycap_ten:` on Rocket.Chat), so
//! other clients show the text with reactions and can vote too.

use rocketchat::models::Message;

use crate::presence::Presence;
use crate::reactions::{Reactor, groups};

/// The first line's marker.
pub const MARK: &str = "📊";
/// The marker as a shortcode (accepted when parsing).
const MARK_SHORTCODE: &str = ":bar_chart:";
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 10;

/// Reaction names of the options (Rocket.Chat's emoji shortcodes).
const REACTIONS: [&str; MAX_OPTIONS] = [
    ":one:",
    ":two:",
    ":three:",
    ":four:",
    ":five:",
    ":six:",
    ":seven:",
    ":eight:",
    ":nine:",
    ":keycap_ten:",
];

/// The options' keycaps as written in the text.
const KEYCAPS: [&str; MAX_OPTIONS] = [
    "1\u{fe0f}\u{20e3}",
    "2\u{fe0f}\u{20e3}",
    "3\u{fe0f}\u{20e3}",
    "4\u{fe0f}\u{20e3}",
    "5\u{fe0f}\u{20e3}",
    "6\u{fe0f}\u{20e3}",
    "7\u{fe0f}\u{20e3}",
    "8\u{fe0f}\u{20e3}",
    "9\u{fe0f}\u{20e3}",
    "\u{1f51f}",
];

/// The reaction that votes for option `i` (0-based).
pub fn reaction(i: usize) -> Option<&'static str> {
    REACTIONS.get(i).copied()
}

/// The keycap of option `i` (0-based).
pub fn keycap(i: usize) -> Option<&'static str> {
    KEYCAPS.get(i).copied()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poll {
    pub question: String,
    /// Option texts, in keycap order.
    pub options: Vec<String>,
}

/// The option number a line starts with, and the rest of the line:
/// a keycap with or without the variation selector, or its shortcode.
fn option_prefix(line: &str) -> Option<(usize, &str)> {
    for i in 0..MAX_OPTIONS {
        let plain_keycap = KEYCAPS[i].replace('\u{fe0f}', "");
        let shortcodes: &[&str] = if i == 9 {
            &[":keycap_ten:", ":ten:"]
        } else {
            &[REACTIONS[i]]
        };
        let prefixes = [KEYCAPS[i], plain_keycap.as_str()]
            .into_iter()
            .chain(shortcodes.iter().copied());
        for p in prefixes {
            if let Some(rest) = line.strip_prefix(p) {
                return Some((i, rest));
            }
        }
    }
    None
}

/// The poll in `text`, if it follows the convention strictly: the marker
/// and a question on the first line, then 2–10 options numbered from 1 in
/// order, each with text; blank lines and CRLF are tolerated, anything
/// else makes it a plain message.
pub fn parse(text: &str) -> Option<Poll> {
    let mut lines = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r').trim())
        .filter(|l| !l.is_empty());
    let first = lines.next()?;
    let rest = first
        .strip_prefix(MARK)
        .or_else(|| first.strip_prefix(MARK_SHORTCODE))?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let question = rest.trim();
    if question.is_empty() {
        return None;
    }
    let mut options = Vec::new();
    for line in lines {
        let (i, rest) = option_prefix(line)?;
        if i != options.len() || !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let text = rest.trim();
        if text.is_empty() {
            return None;
        }
        options.push(text.to_owned());
    }
    (MIN_OPTIONS..=MAX_OPTIONS)
        .contains(&options.len())
        .then(|| Poll {
            question: question.to_owned(),
            options,
        })
}

/// Why a poll cannot be generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollError {
    NoQuestion,
    TooFewOptions,
    TooManyOptions,
}

/// The message text of a poll (empty options are dropped; lines are joined
/// to one, so the text parses back to the same poll).
pub fn generate(question: &str, options: &[String]) -> Result<String, PollError> {
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let question = one_line(question);
    if question.is_empty() {
        return Err(PollError::NoQuestion);
    }
    let options: Vec<String> = options
        .iter()
        .map(|o| one_line(o))
        .filter(|o| !o.is_empty())
        .collect();
    if options.len() < MIN_OPTIONS {
        return Err(PollError::TooFewOptions);
    }
    if options.len() > MAX_OPTIONS {
        return Err(PollError::TooManyOptions);
    }
    let mut out = format!("{MARK} {question}");
    for (i, o) in options.iter().enumerate() {
        out.push('\n');
        out.push_str(KEYCAPS[i]);
        out.push(' ');
        out.push_str(o);
    }
    Ok(out)
}

/// Votes of one option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionVotes {
    pub reaction: &'static str,
    pub voters: Vec<Reactor>,
    /// The current user voted for it.
    pub mine: bool,
}

/// Votes per option of `poll` from the reactions of `m`.
pub fn tally(poll: &Poll, m: &Message, presence: &Presence, me: Option<&str>) -> Vec<OptionVotes> {
    let groups = groups(m, presence);
    (0..poll.options.len())
        .map(|i| {
            let reaction = REACTIONS[i];
            let voters = groups
                .iter()
                .find(|g| g.emoji == reaction)
                .map(|g| g.users.clone())
                .unwrap_or_default();
            let mine = me.is_some_and(|me| voters.iter().any(|v| v.username == me));
            OptionVotes {
                reaction,
                voters,
                mine,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use rocketchat::models::Reaction;

    use super::*;

    fn opts(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn generate_parse_roundtrip() {
        let text = generate("  Lunch\n today? ", &opts(&["Pizza", " ", "Sushi  bar"])).unwrap();
        assert_eq!(text, "📊 Lunch today?\n1️⃣ Pizza\n2️⃣ Sushi bar");
        let p = parse(&text).unwrap();
        assert_eq!(p.question, "Lunch today?");
        assert_eq!(p.options, ["Pizza", "Sushi bar"]);
        let ten: Vec<String> = (1..=10).map(|i| format!("o{i}")).collect();
        let text = generate("Q", &ten).unwrap();
        assert!(text.ends_with("\n🔟 o10"));
        assert_eq!(parse(&text).unwrap().options, ten);
        assert_eq!(generate(" ", &ten), Err(PollError::NoQuestion));
        assert_eq!(generate("Q", &opts(&["a"])), Err(PollError::TooFewOptions));
        let eleven: Vec<String> = (0..11).map(|i| i.to_string()).collect();
        assert_eq!(generate("Q", &eleven), Err(PollError::TooManyOptions));
    }

    #[test]
    fn parse_is_tolerant_but_strict() {
        let p = parse("📊 Q?\r\n\r\n:one: a\r\n2\u{20e3} b").unwrap();
        assert_eq!(p.options, ["a", "b"]);
        let p = parse(":bar_chart: Q\n:one: a\n:two: b\n\n").unwrap();
        assert_eq!(p.question, "Q");
        let ten = (1..=9)
            .map(|i| format!("{} o", KEYCAPS[i - 1]))
            .chain(["  :keycap_ten: last".into()])
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse(&format!("📊 Q\n{ten}")).unwrap().options.len(), 10);
        for bad in [
            "📊 Q\n1️⃣ a",                  // one option
            "📊 Q\n2️⃣ a\n1️⃣ b",            // out of order
            "📊 Q\n1️⃣ a\n3️⃣ b",            // gap
            "📊 Q\n1️⃣ a\nfree text\n2️⃣ b", // stray line
            "📊 Q\n1️⃣ a\n2️⃣",              // empty option
            "📊 Q\n1️⃣a\n2️⃣ b",             // no space
            "📊\n1️⃣ a\n2️⃣ b",              // no question
            "📊Q\n1️⃣ a\n2️⃣ b",             // marker glued to it
            "Q\n1️⃣ a\n2️⃣ b",               // no marker
            "say 📊 Q\n1️⃣ a\n2️⃣ b",
        ] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn tally_counts_reactions() {
        let p = parse("📊 Q\n1️⃣ a\n2️⃣ b\n3️⃣ c").unwrap();
        let mut reactions = BTreeMap::new();
        reactions.insert(
            ":one:".to_owned(),
            Reaction {
                usernames: vec!["bob".into(), "me".into()],
                names: None,
            },
        );
        reactions.insert(
            ":keycap_ten:".to_owned(),
            Reaction {
                usernames: vec!["x".into()],
                names: None,
            },
        );
        reactions.insert(
            ":three:".to_owned(),
            Reaction {
                usernames: vec!["carol".into()],
                names: None,
            },
        );
        let m = Message {
            reactions: Some(reactions),
            ..Default::default()
        };
        let t = tally(&p, &m, &Presence::default(), Some("me"));
        let counts: Vec<usize> = t.iter().map(|o| o.voters.len()).collect();
        assert_eq!(counts, [2, 0, 1]);
        assert!(t[0].mine && !t[2].mine);
        assert_eq!(t[1].reaction, ":two:");
        assert_eq!(reaction(9), Some(":keycap_ten:"));
        assert_eq!(keycap(0), Some("1️⃣"));
        assert_eq!(reaction(10), None);
    }
}
