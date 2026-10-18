//! `aq hook`: a Claude Code hook that gives the queue changes to the orchestrator
//! session, and `aq setup-claude`, which installs the hook.
//!
//! The hook acts only in the session that the cursor of the consumer
//! `orchestrator` names, and only outside of subagents. Other sessions and the
//! workers get no output.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::chat::ORCHESTRATOR;
use crate::error::AqError;
use crate::queue::{Queue, write_atomic};
use crate::templates;
use crate::wait::{self, Event};

/// The command that the installed hooks run.
pub const COMMAND: &str = "aq hook";

/// The hook events that [`install`] adds.
const EVENTS: [&str; 4] = ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"];

/// The fields of the hook input that the hook reads.
#[derive(Deserialize)]
struct Input {
    session_id: Option<String>,
    hook_event_name: String,
    /// Set when the hook fires in a subagent.
    agent_id: Option<String>,
    cwd: Option<PathBuf>,
}

/// The text that tells the orchestrator about the changes.
fn changes_text(events: &[Event]) -> String {
    let mut text = String::from("Changes in the aq queue:\n");
    for e in events {
        text.push_str(&format!("\n- {}", e.line()));
    }
    text.push_str("\n\nAct on these changes as `.aq/ORCHESTRATOR.md` says.");
    text
}

fn context_output(event: &str, text: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": text,
        }
    })
}

fn print_json(v: &Value) -> Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "{v}")?;
    out.flush()?;
    Ok(())
}

/// Runs the hook. It reads the hook input on stdin and writes the hook output on stdout.
pub fn run(dir: Option<&Path>, cwd: &Path) -> Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let input: Input = serde_json::from_str(&raw).context("parse the hook input")?;
    if input.agent_id.is_some() {
        return Ok(());
    }
    let Some(session) = input.session_id else {
        return Ok(());
    };
    let start = input.cwd.unwrap_or_else(|| cwd.to_path_buf());
    let q = match Queue::find(&start, dir) {
        Ok(q) => q,
        Err(e) if matches!(e.downcast_ref(), Some(AqError::NoQueue)) => return Ok(()),
        Err(e) => return Err(e),
    };
    if wait::session(&q, ORCHESTRATOR)?.as_deref() != Some(session.as_str()) {
        return Ok(());
    }
    let event = input.hook_event_name.as_str();
    match event {
        "SessionStart" => {
            wait::take(&q, ORCHESTRATOR, None, |events| {
                let mut text = format!(
                    "You are the orchestrator of the aq queue `{}`. {}",
                    q.root.display(),
                    templates::orchestrator_start()
                );
                if !events.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&changes_text(events));
                }
                print_json(&context_output(event, &text))
            })?;
        }
        "UserPromptSubmit" | "PostToolUse" => {
            wait::take(&q, ORCHESTRATOR, None, |events| {
                print_json(&context_output(event, &changes_text(events)))
            })?;
        }
        "Stop" => {
            wait::take(&q, ORCHESTRATOR, None, |events| {
                print_json(&json!({"decision": "block", "reason": changes_text(events)}))
            })?;
        }
        _ => {}
    }
    Ok(())
}

/// True if the hook entries of one event already run [`COMMAND`].
fn has_command(entries: &[Value]) -> bool {
    entries.iter().any(|m| {
        m["hooks"]
            .as_array()
            .is_some_and(|hs| hs.iter().any(|h| h["command"] == COMMAND))
    })
}

/// Adds the aq hooks to the Claude Code settings file `path`. The other
/// settings stay. Returns the events that got a new hook.
pub fn install(path: &Path) -> Result<Vec<&'static str>> {
    let mut settings: Value = match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => json!({}),
        Ok(t) => serde_json::from_str(&t).with_context(|| format!("parse {}", path.display()))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let root = settings
        .as_object_mut()
        .with_context(|| format!("{}: the settings are not a JSON object", path.display()))?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| format!("{}: `hooks` is not a JSON object", path.display()))?;
    let mut added = Vec::new();
    for event in EVENTS {
        let entries = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("{}: `hooks.{event}` is not a list", path.display()))?;
        if has_command(entries) {
            continue;
        }
        entries.push(json!({"hooks": [{"type": "command", "command": COMMAND}]}));
        added.push(event);
    }
    if !added.is_empty() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomic(
            path,
            &format!("{}\n", serde_json::to_string_pretty(&settings)?),
        )?;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_keeps_settings_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"model": "x", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "other"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(install(&path).unwrap(), EVENTS.to_vec());
        assert!(install(&path).unwrap().is_empty());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["model"], "x");
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "other");
        assert_eq!(stop[1]["hooks"][0]["command"], COMMAND);
    }
}
