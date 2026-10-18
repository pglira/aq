//! The settings of one queue in `.aq/config.toml`.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::error::conflict;

/// The name of the file in the `.aq` directory.
pub const FILE: &str = "config.toml";

/// The key of the check command.
const CHECK: &str = "check";
/// The key of the push policy.
const PUSH: &str = "push";

/// The text of a new `.aq/config.toml`.
pub const TEMPLATE: &str = r#"# The settings of this queue.

# A shell command that `aq submit` runs in the project directory.
# The submit fails (exit code 2) if the command fails. `aq submit --no-check` skips it.
# check = "cargo test && cargo clippy --all-targets -- -D warnings"

# When the orchestrator pushes the work to the remote: "never" or "after_approve".
push = "never"
"#;

/// When the orchestrator pushes the work to a git remote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PushPolicy {
    /// The orchestrator never pushes.
    #[default]
    Never,
    /// The orchestrator pushes the branch of a task after the human approves it.
    AfterApprove,
}

impl PushPolicy {
    fn parse(s: &str) -> Option<PushPolicy> {
        match s {
            "never" => Some(PushPolicy::Never),
            "after_approve" => Some(PushPolicy::AfterApprove),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueConfig {
    /// The shell command that `aq submit` runs. `None` runs no check.
    pub check: Option<String>,
    pub push: PushPolicy,
}

/// Reads the config file. A missing file gives the defaults.
pub fn load(path: &Path) -> Result<QueueConfig> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(QueueConfig::default()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    parse(&text).with_context(|| format!("config file {}", path.display()))
}

fn parse(text: &str) -> Result<QueueConfig> {
    let t: toml::Table = text.parse()?;
    let check = match t.get(CHECK) {
        None => None,
        Some(v) => {
            let s = v
                .as_str()
                .with_context(|| format!("`{CHECK}` must be a string"))?;
            Some(s.trim().to_string()).filter(|s| !s.is_empty())
        }
    };
    let push = match t.get(PUSH) {
        None => PushPolicy::default(),
        Some(v) => v
            .as_str()
            .and_then(PushPolicy::parse)
            .with_context(|| format!("`{PUSH}` must be \"never\" or \"after_approve\""))?,
    };
    Ok(QueueConfig { check, push })
}

/// Runs the check command with `sh -c` in `dir`. The output of the command goes
/// to the output of the process. A failed command gives a conflict error.
pub fn run_check(cmd: &str, dir: &Path) -> Result<()> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("run the check `{cmd}`"))?;
    if status.success() {
        Ok(())
    } else {
        Err(conflict(format!(
            "the check failed ({status}): {cmd}\nfix the problem, or skip the check with --no-check"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        assert_eq!(parse("").unwrap(), QueueConfig::default());
        assert_eq!(load(Path::new("/nonexistent/x.toml")).unwrap().check, None);
    }

    #[test]
    fn template_parses() {
        let c = parse(TEMPLATE).unwrap();
        assert_eq!(c.check, None);
        assert_eq!(c.push, PushPolicy::Never);
    }

    #[test]
    fn values() {
        let c = parse("check = \"true\"\npush = \"after_approve\"").unwrap();
        assert_eq!(c.check.as_deref(), Some("true"));
        assert_eq!(c.push, PushPolicy::AfterApprove);
        assert_eq!(parse("check = \"  \"").unwrap().check, None);
        assert!(parse("push = \"always\"").is_err());
        assert!(parse("check = 3").is_err());
    }

    #[test]
    fn check_result() {
        let d = tempfile::tempdir().unwrap();
        run_check("true", d.path()).unwrap();
        let err = run_check("exit 3", d.path()).unwrap_err();
        let e = err.downcast_ref::<crate::error::AqError>().unwrap();
        assert_eq!(e.exit_code(), 2);
        run_check("test -d .", d.path()).unwrap();
    }
}
