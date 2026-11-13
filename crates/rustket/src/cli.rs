//! Command-line flags handled before iced starts (and before the software
//! renderer re-exec): `--help`/`-h`, `--version`/`-V`, `--dump-theme` and
//! `--dump-locale [CODE]`.
//! Other arguments keep their old meaning: non-flag arguments are links for
//! the single instance, unknown flags are ignored.

use rust_i18n::t;

pub const BIN: &str = "rustket";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `main` should do with the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start the application.
    Run,
    /// Print to stdout and exit successfully.
    Print(String),
    /// Print to stderr and exit with a failure status.
    Fail(String),
}

/// `rustket 0.2.5`.
pub fn version_line() -> String {
    format!("{BIN} {VERSION}")
}

pub fn help_text() -> String {
    format!(
        "{version}\n{about}\n\n{usage}\n{links}\n\n{options}\n  -h, --help              {help}\n  -V, --version           {ver}\n      --dump-theme        {dump}\n      --dump-locale [CODE] {dump_locale}\n\n{env}\n  ICED_BACKEND=tiny-skia  {backend}\n  RUST_LOG=<filter>       {log}\n  RUST_BACKTRACE=1        {bt}\n",
        version = version_line(),
        about = t!("cli.about"),
        usage = t!("cli.usage", bin = BIN),
        links = t!("cli.links"),
        options = t!("cli.options"),
        help = t!("cli.opt_help"),
        ver = t!("cli.opt_version"),
        dump = t!("cli.opt_dump_theme"),
        dump_locale = t!("cli.opt_dump_locale"),
        env = t!("cli.env"),
        backend = t!("cli.env_backend"),
        log = t!("cli.env_log"),
        bt = t!("cli.env_backtrace"),
    )
}

/// Decides from the arguments after the program name. The first of
/// `--help`/`--version`/`--dump-theme`/`--dump-locale` wins; arguments after
/// `--` are never flags. `--dump-locale` takes the next argument as its code
/// unless it is a flag or a link (then `en`); an invalid code fails.
pub fn parse<I, S>(args: I) -> Action
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_ref().to_str() {
            Some("--") => break,
            Some("-h" | "--help") => return Action::Print(help_text()),
            Some("-V" | "--version") => return Action::Print(format!("{}\n", version_line())),
            Some("--dump-theme") => {
                return Action::Print(rustket_ui::theme_file::dump_template());
            }
            Some("--dump-locale") => {
                let code = args
                    .peek()
                    .map(|a| a.as_ref().to_string_lossy().into_owned())
                    .filter(|a| !a.starts_with('-') && !a.contains([':', '/']))
                    .unwrap_or_else(|| "en".to_owned());
                return match crate::locales::dump_for_cli(&code) {
                    Ok(dump) => {
                        if let Some(w) = dump.warnings {
                            eprintln!("{w}");
                        }
                        Action::Print(dump.template)
                    }
                    Err(e) => Action::Fail(format!("{e}\n")),
                };
            }
            _ => {}
        }
    }
    Action::Run
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn version_and_help_flags() {
        let v = format!("rustket {}\n", env!("CARGO_PKG_VERSION"));
        assert_eq!(parse(["--version"]), Action::Print(v.clone()));
        assert_eq!(parse(["-V"]), Action::Print(v));
        let Action::Print(help) = parse(["--help"]) else {
            panic!("--help prints");
        };
        assert!(help.starts_with("rustket "));
        assert!(help.contains("--version"));
        assert!(help.contains("RUST_BACKTRACE=1"));
        assert!(help.contains("--dump-theme"));
        assert!(help.contains("--dump-locale [CODE]"));
        assert_eq!(parse(["-h"]), Action::Print(help));
        assert!(matches!(
            parse(["rocketchat://auth?host=x", "--help"]),
            Action::Print(_)
        ));
    }

    #[test]
    fn dump_theme_prints_the_template() {
        let Action::Print(text) = parse(["--dump-theme"]) else {
            panic!("--dump-theme prints");
        };
        assert_eq!(text, rustket_ui::theme_file::dump_template());
        assert_eq!(parse(["--", "--dump-theme"]), Action::Run);
    }

    #[test]
    fn dump_locale_prints_a_template() {
        let en = crate::locales::dump_template("en", None).unwrap();
        assert_eq!(parse(["--dump-locale"]), Action::Print(en.clone()));
        assert_eq!(parse(["--dump-locale", "en"]), Action::Print(en.clone()));
        assert_eq!(
            parse(["--dump-locale", "--help"]),
            Action::Print(en.clone())
        );
        assert_eq!(
            parse(["--dump-locale", "rocketchat://room?host=a"]),
            Action::Print(en)
        );
        assert_eq!(
            parse(["--dump-locale", "ru"]),
            Action::Print(crate::locales::dump_template("ru", None).unwrap())
        );
        let Action::Fail(e) = parse(["--dump-locale", "xx_NOPE"]) else {
            panic!("unknown code fails");
        };
        assert!(e.contains("xx_NOPE") && e.contains("ru"), "{e}");
        for bad in ["pt.BR", "de.toml"] {
            let Action::Fail(e) = parse(["--dump-locale", bad]) else {
                panic!("invalid code {bad} fails");
            };
            assert!(e.contains(bad), "{e}");
        }
        assert_eq!(parse(["--", "--dump-locale"]), Action::Run);
    }

    #[test]
    fn links_and_other_flags_run() {
        assert_eq!(parse(Vec::<String>::new()), Action::Run);
        assert_eq!(parse(["rocketchat://room?host=a&rid=b"]), Action::Run);
        assert_eq!(parse(["--", "--version"]), Action::Run);
        assert_eq!(parse(["--unknown", "-x"]), Action::Run);
    }
}
