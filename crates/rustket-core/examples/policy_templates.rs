//! Writes the templates for administrators (docs/policy.md section 14) and
//! docs/policies.md under the given directory, the repository root:
//!
//! `cargo run -p rustket-core --example policy_templates -- .`

use std::path::PathBuf;
use std::process::ExitCode;

#[path = "../tests/admin_templates/mod.rs"]
#[allow(dead_code)]
mod admin_templates;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(root), None) = (args.next(), args.next()) else {
        eprintln!("usage: policy_templates <repository root>");
        return ExitCode::from(2);
    };
    let root = PathBuf::from(root);
    for (rel, content) in admin_templates::generate() {
        let path = root.join(&rel);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, content));
        if let Err(e) = written {
            eprintln!("{}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("{rel}");
    }
    ExitCode::SUCCESS
}
