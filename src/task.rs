use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset, Local};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A new task. Agents do not pick it up until the human starts it.
    Draft,
    Todo,
    InProgress,
    Blocked,
    Review,
    Done,
    Failed,
}

impl Status {
    pub const ALL: [Status; 7] = [
        Status::Draft,
        Status::Todo,
        Status::InProgress,
        Status::Blocked,
        Status::Review,
        Status::Done,
        Status::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::Todo => "todo",
            Status::InProgress => "in_progress",
            Status::Blocked => "blocked",
            Status::Review => "review",
            Status::Done => "done",
            Status::Failed => "failed",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Status::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .with_context(|| format!("unknown status `{s}`"))
    }
}

pub const PRIORITY_HIGH: u8 = 1;
pub const PRIORITY_DEFAULT: u8 = 2;
pub const PRIORITY_LOW: u8 = 3;

/// The YAML frontmatter of `task.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub id: u32,
    /// A short title. It can be empty; then the description names the task.
    #[serde(default)]
    pub title: String,
    pub status: Status,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub depends_on: Vec<u32>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub claimed_at: Option<DateTime<FixedOffset>>,
    pub created: DateTime<FixedOffset>,
    pub updated: DateTime<FixedOffset>,
}

fn default_priority() -> u8 {
    PRIORITY_DEFAULT
}

/// A task: the frontmatter and the two content sections of `task.md`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Task {
    #[serde(flatten)]
    pub meta: Meta,
    pub description: String,
    pub acceptance: String,
}

pub fn now() -> DateTime<FixedOffset> {
    Local::now().fixed_offset()
}

impl Task {
    pub fn new(id: u32, title: &str) -> Task {
        let t = now();
        Task {
            meta: Meta {
                id,
                title: title.to_string(),
                status: Status::Draft,
                priority: PRIORITY_DEFAULT,
                depends_on: Vec::new(),
                assignee: None,
                claimed_at: None,
                created: t,
                updated: t,
            },
            description: String::new(),
            acceptance: String::new(),
        }
    }

    pub fn parse(text: &str) -> Result<Task> {
        let text = text.replace("\r\n", "\n");
        let rest = text
            .strip_prefix("---\n")
            .context("task.md must start with a `---` line")?;
        let end = rest
            .find("\n---\n")
            .map(|i| (i, i + 5))
            .or_else(|| rest.strip_suffix("\n---").map(|r| (r.len(), rest.len())))
            .context("task.md has no closing `---` line for the frontmatter")?;
        let meta: Meta =
            serde_yaml::from_str(&rest[..end.0]).context("invalid frontmatter in task.md")?;
        let (description, acceptance) = split_body(&rest[end.1..]);
        let task = Task {
            meta,
            description,
            acceptance,
        };
        task.validate()?;
        Ok(task)
    }

    /// Checks the fields: a title or a description, a known priority, no self-dependency.
    pub fn validate(&self) -> Result<()> {
        let meta = &self.meta;
        if meta.title.trim().is_empty() && self.description.trim().is_empty() {
            bail!("a task needs a title or a description");
        }
        if meta.title.contains('\n') {
            bail!("the title must be one line");
        }
        if !(PRIORITY_HIGH..=PRIORITY_LOW).contains(&meta.priority) {
            bail!(
                "the priority must be {PRIORITY_HIGH} (high), {PRIORITY_DEFAULT} (normal) or {PRIORITY_LOW} (low)"
            );
        }
        if meta.depends_on.contains(&meta.id) {
            bail!("a task cannot depend on itself");
        }
        Ok(())
    }

    /// True if the task has a title that is not empty.
    pub fn has_title(&self) -> bool {
        !self.meta.title.trim().is_empty()
    }

    /// The text that names the task: the title, else the first line of the description.
    pub fn display_title(&self) -> String {
        if self.has_title() {
            self.meta.title.trim().to_string()
        } else {
            first_line(&self.description)
        }
    }

    pub fn render(&self) -> String {
        let yaml = serde_yaml::to_string(&self.meta).expect("frontmatter serializes");
        let mut out = format!("---\n{yaml}---\n\n## Description\n\n");
        push_section(&mut out, &self.description);
        out.push_str("\n## Acceptance\n\n");
        push_section(&mut out, &self.acceptance);
        out
    }

    pub fn id(&self) -> u32 {
        self.meta.id
    }

    pub fn status(&self) -> Status {
        self.meta.status
    }
}

fn push_section(out: &mut String, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        out.push_str(text);
        out.push('\n');
    }
}

/// The first line of Markdown text that is not empty, without the leading
/// heading, list and quote marks.
pub fn first_line(text: &str) -> String {
    text.lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '>', '-', '*', '+', ' '])
                .trim()
        })
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Splits the body into the `## Description` and `## Acceptance` sections.
/// Text before the first known heading belongs to the description.
fn split_body(body: &str) -> (String, String) {
    let mut desc = String::new();
    let mut acc = String::new();
    let mut in_acc = false;
    for line in body.lines() {
        match line.trim_end() {
            "## Description" => in_acc = false,
            "## Acceptance" => in_acc = true,
            _ => {
                let target = if in_acc { &mut acc } else { &mut desc };
                target.push_str(line);
                target.push('\n');
            }
        }
    }
    (desc.trim().to_string(), acc.trim().to_string())
}

/// Makes a file-name slug from a title: lower case, ASCII letters and digits, `-` between words.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let mut out: String = out.trim_end_matches('-').chars().take(40).collect();
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "task".into() } else { out }
}

/// Parses a dependency list such as `1, 3 4`.
pub fn parse_ids(text: &str) -> Result<Vec<u32>> {
    let mut ids = Vec::new();
    for part in text.split(|c: char| c == ',' || c.is_whitespace()) {
        let part = part.trim().trim_start_matches('#');
        if part.is_empty() {
            continue;
        }
        let id: u32 = part
            .parse()
            .with_context(|| format!("`{part}` is not a task id"))?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut t = Task::new(2, "Fix cache invalidation");
        t.meta.depends_on = vec![1];
        t.description = "The cache is not cleared.\n\n- a\n- b".into();
        t.acceptance = "- A test.".into();
        let parsed = Task::parse(&t.render()).unwrap();
        assert_eq!(parsed, t);
    }

    #[test]
    fn body_without_headings_is_description() {
        let text = "---\nid: 1\ntitle: x\nstatus: todo\ncreated: 2026-10-18T18:16:00+02:00\nupdated: 2026-10-18T18:16:00+02:00\n---\nhello\n";
        let t = Task::parse(text).unwrap();
        assert_eq!(t.description, "hello");
        assert_eq!(t.meta.priority, PRIORITY_DEFAULT);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let text = "---\nid: 1\ntitle: x\nstatus: todo\ngalley: true\ncreated: 2026-10-18T18:16:00+02:00\nupdated: 2026-10-18T18:16:00+02:00\n---\nhello\n";
        let t = Task::parse(text).unwrap();
        assert_eq!(t.meta.title, "x");
    }

    #[test]
    fn invalid_priority() {
        let mut t = Task::new(1, "x");
        t.meta.priority = 5;
        assert!(Task::parse(&t.render()).is_err());
    }

    #[test]
    fn missing_title_loads() {
        let text = "---\nid: 1\nstatus: todo\ncreated: 2026-10-18T18:16:00+02:00\nupdated: 2026-10-18T18:16:00+02:00\n---\n## Description\n\n## Fix the *cache*\nmore\n";
        let t = Task::parse(text).unwrap();
        assert!(!t.has_title());
        assert_eq!(t.display_title(), "Fix the *cache*");
        let mut t = Task::new(1, "");
        assert!(Task::parse(&t.render()).is_err());
        t.description = "text".into();
        assert_eq!(Task::parse(&t.render()).unwrap(), t);
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Fix cache invalidation!"), "fix-cache-invalidation");
        assert_eq!(slug("  ** "), "task");
    }

    #[test]
    fn ids() {
        assert_eq!(parse_ids("1, #3 4,,3").unwrap(), vec![1, 3, 4]);
        assert!(parse_ids("a").is_err());
    }
}
