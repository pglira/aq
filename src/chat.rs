use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::task::now;

pub const HUMAN: &str = "human";
pub const AQ: &str = "aq";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Message,
    Log,
    Question,
    Review,
    Event,
}

impl Kind {
    /// The name of the kind, as in `chat.jsonl`.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::Log => "log",
            Kind::Question => "question",
            Kind::Review => "review",
            Kind::Event => "event",
        }
    }
}

/// One line of `chat.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub ts: DateTime<FixedOffset>,
    pub from: String,
    pub kind: Kind,
    pub text: String,
}

impl Entry {
    pub fn new(from: &str, kind: Kind, text: &str) -> Entry {
        Entry {
            ts: now(),
            from: from.to_string(),
            kind,
            text: text.to_string(),
        }
    }
}

/// Reads all entries. Lines that do not parse are skipped, so that one bad line
/// does not hide the rest of the chat.
pub fn read(path: &Path) -> Result<Vec<Entry>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// Appends one entry with a single `write` call. The caller holds the queue lock.
pub fn append(path: &Path, entry: &Entry) -> Result<()> {
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// The read marks of a task: the number of chat entries that each reader has seen.
pub fn read_marks(path: &Path) -> BTreeMap<String, usize> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn set_read_mark(path: &Path, reader: &str, count: usize) -> Result<()> {
    let mut marks = read_marks(path);
    if marks.get(reader) == Some(&count) {
        return Ok(());
    }
    marks.insert(reader.to_string(), count);
    crate::queue::write_atomic(path, &serde_json::to_string(&marks)?)
}

/// The entries after the read mark of `reader` that other authors wrote.
/// Events do not count.
pub fn unread<'a>(
    entries: &'a [Entry],
    marks: &BTreeMap<String, usize>,
    reader: &'a str,
) -> impl Iterator<Item = &'a Entry> {
    let seen = marks.get(reader).copied().unwrap_or(0);
    entries
        .iter()
        .skip(seen)
        .filter(move |e| e.from != reader && e.kind != Kind::Event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.jsonl");
        let line = r#"{"ts":"2026-10-18T18:16:00+02:00","from":"w","kind":"event","text":"submitted: x","galley":"r/01"}"#;
        fs::write(&path, format!("{line}\n")).unwrap();
        let entries = read(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "submitted: x");
    }

    #[test]
    fn kind_names_match_serde() {
        for k in [
            Kind::Message,
            Kind::Log,
            Kind::Question,
            Kind::Review,
            Kind::Event,
        ] {
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
    }
}
