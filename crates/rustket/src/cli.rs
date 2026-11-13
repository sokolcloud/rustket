//! Command-line flags handled before iced starts (and before the software
//! renderer re-exec): `--help`/`-h` and `--version`/`-V`. Other arguments keep
//! their old meaning: non-flag arguments are links for the single instance,
//! unknown flags are ignored.

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
}

/// `rustket 0.2.4`.
pub fn version_line() -> String {
    format!("{BIN} {VERSION}")
}

pub fn help_text() -> String {
    format!(
        "{version}\n{about}\n\n{usage}\n{links}\n\n{options}\n  -h, --help              {help}\n  -V, --version           {ver}\n\n{env}\n  ICED_BACKEND=tiny-skia  {backend}\n  RUST_LOG=<filter>       {log}\n  RUST_BACKTRACE=1        {bt}\n",
        version = version_line(),
        about = t!("cli.about"),
        usage = t!("cli.usage", bin = BIN),
        links = t!("cli.links"),
        options = t!("cli.options"),
        help = t!("cli.opt_help"),
        ver = t!("cli.opt_version"),
        env = t!("cli.env"),
        backend = t!("cli.env_backend"),
        log = t!("cli.env_log"),
        bt = t!("cli.env_backtrace"),
    )
}

/// Decides from the arguments after the program name. The first of
/// `--help`/`--version` wins; arguments after `--` are never flags.
pub fn parse<I, S>(args: I) -> Action
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    for arg in args {
        match arg.as_ref().to_str() {
            Some("--") => break,
            Some("-h" | "--help") => return Action::Print(help_text()),
            Some("-V" | "--version") => return Action::Print(format!("{}\n", version_line())),
            _ => {}
        }
    }
    Action::Run
}

#[cfg(test)]
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
        assert_eq!(parse(["-h"]), Action::Print(help));
        assert!(matches!(
            parse(["rocketchat://auth?host=x", "--help"]),
            Action::Print(_)
        ));
    }

    #[test]
    fn links_and_other_flags_run() {
        assert_eq!(parse(Vec::<String>::new()), Action::Run);
        assert_eq!(parse(["rocketchat://room?host=a&rid=b"]), Action::Run);
        assert_eq!(parse(["--", "--version"]), Action::Run);
        assert_eq!(parse(["--unknown", "-x"]), Action::Run);
    }
}
