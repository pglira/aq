//! `aq wait`: polls the queue and reports the changes that matter to the orchestrator.
//!
//! A consumer is a named reader of the changes. Its cursor in `.aq/consumers/`
//! holds the queue state that it saw last, so that each change reaches the
//! consumer once, also across runs of `aq wait` and `aq hook`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::chat::{HUMAN, Kind};
use crate::error::AqError;
use crate::queue::{LoadResult, Loaded, Queue, ready, write_atomic};
use crate::task::{Status, first_line};

/// The time between two polls of the queue.
const POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A new task.
    Created,
    /// A task directory is gone.
    Deleted,
    /// A status change other than a claim.
    Status,
    /// A `todo` task became ready, and its status did not change.
    Ready,
    /// A chat message of the human.
    Message,
}

/// One change in the queue.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub id: u32,
    pub title: String,
    pub event: EventKind,
    pub from: Option<Status>,
    pub to: Option<Status>,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Event {
    /// The event as one line of text.
    pub fn line(&self) -> String {
        let ready = if self.ready { " (ready)" } else { "" };
        let head = match self.event {
            EventKind::Created => {
                let to = self.to.map_or("?".to_string(), |s| s.to_string());
                format!("created ({to}){ready}")
            }
            EventKind::Deleted => "deleted".to_string(),
            EventKind::Status => {
                let from = self.from.map_or("?".to_string(), |s| s.to_string());
                let to = self.to.map_or("?".to_string(), |s| s.to_string());
                format!("{from} -> {to}{ready}")
            }
            EventKind::Ready => "ready".to_string(),
            EventKind::Message => format!(
                "human message: {}",
                first_line(self.text.as_deref().unwrap_or(""))
            ),
        };
        format!("task {}: {head}  {}", self.id, self.title)
    }
}

/// The state of one task that the events compare.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskState {
    title: String,
    status: Status,
    chat_len: usize,
}

/// The state of the queue at one poll.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    tasks: BTreeMap<u32, TaskState>,
    ready: HashSet<u32>,
    /// The ids of the task directories with a file that does not parse.
    skipped_ids: HashSet<u32>,
}

impl Snapshot {
    pub fn new((tasks, bad): &LoadResult) -> Snapshot {
        Snapshot {
            tasks: tasks.iter().map(|l| (l.task.id(), state(l))).collect(),
            ready: ready(tasks).into_iter().collect(),
            skipped_ids: bad.iter().filter_map(|(p, _)| dir_id(p)).collect(),
        }
    }
}

/// The stored state of a consumer: the queue state that it saw last, and the
/// agent session that reads it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    tasks: BTreeMap<u32, TaskState>,
    ready: BTreeSet<u32>,
}

impl Cursor {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            tasks: self.tasks.clone(),
            ready: self.ready.iter().copied().collect(),
            ..Snapshot::default()
        }
    }
}

/// The changes that [`take`] gives to a consumer, and the task files that do not parse.
pub struct Taken {
    pub events: Vec<Event>,
    pub bad: Vec<(PathBuf, String)>,
}

/// Checks that a consumer name is safe as a file name.
pub fn check_consumer(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(crate::error::usage(
            "a consumer name has only letters, digits, '-' and '_'",
        ))
    }
}

fn consumers_dir(q: &Queue) -> PathBuf {
    q.root.join("consumers")
}

fn cursor_path(q: &Queue, name: &str) -> PathBuf {
    consumers_dir(q).join(format!("{name}.json"))
}

/// Reads the cursor of a consumer. A missing file gives `None`.
fn read_cursor(q: &Queue, name: &str) -> Result<Option<Cursor>> {
    let path = cursor_path(q, name);
    match fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t)
            .map(Some)
            .with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// The agent session that reads the changes of a consumer, if a session is known.
pub fn session(q: &Queue, name: &str) -> Result<Option<String>> {
    Ok(read_cursor(q, name)?.and_then(|c| c.session))
}

/// Gives the changes since the cursor of the consumer `name` to `emit`, and then
/// moves the cursor to the current state. If `emit` fails, the cursor stays, so
/// that the next call gives the same changes again. A consumer without a cursor
/// starts at the current state, with no changes. `session`, if given, becomes
/// the session of the consumer.
pub fn take(
    q: &Queue,
    name: &str,
    session: Option<&str>,
    emit: impl FnOnce(&[Event]) -> Result<()>,
) -> Result<Taken> {
    fs::create_dir_all(consumers_dir(q))?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(consumers_dir(q).join(format!("{name}.lock")))?;
    lock.lock().context("lock the cursor")?;
    let old = read_cursor(q, name)?;
    let loaded = q.load(true)?;
    let mut now = Snapshot::new(&loaded);
    let events = match &old {
        Some(c) => diff(&c.snapshot(), &loaded, &mut now),
        None => Vec::new(),
    };
    if !events.is_empty() {
        emit(&events)?;
    }
    let session = session
        .map(str::to_string)
        .or_else(|| old.as_ref().and_then(|c| c.session.clone()));
    let cursor = Cursor {
        session,
        tasks: now.tasks,
        ready: now.ready.into_iter().collect(),
    };
    let text = serde_json::to_string(&cursor)?;
    if old
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?
        .as_ref()
        != Some(&text)
    {
        write_atomic(&cursor_path(q, name), &format!("{text}\n"))?;
    }
    Ok(Taken {
        events,
        bad: loaded.1,
    })
}

fn state(l: &Loaded) -> TaskState {
    TaskState {
        title: l.task.display_title(),
        status: l.task.status(),
        chat_len: l.chat.len(),
    }
}

/// The id in the name of the task directory of a task file, as in `0007-fix/task.md`.
fn dir_id(file: &std::path::Path) -> Option<u32> {
    let name = file.parent()?.file_name()?.to_string_lossy().into_owned();
    name.split('-').next()?.parse().ok()
}

/// The events between two polls. `now` is the snapshot of `loaded`.
/// A task with a file that does not parse makes no event; it keeps its old state.
pub fn diff(old: &Snapshot, loaded: &LoadResult, now: &mut Snapshot) -> Vec<Event> {
    let (tasks, _) = loaded;
    let mut events = Vec::new();
    for l in tasks {
        let id = l.task.id();
        let cur = state(l);
        let is_ready = now.ready.contains(&id);
        let seen_chat = match old.tasks.get(&id) {
            None => {
                events.push(Event {
                    id,
                    title: cur.title.clone(),
                    event: EventKind::Created,
                    from: None,
                    to: Some(cur.status),
                    ready: is_ready,
                    text: None,
                });
                0
            }
            Some(prev) => {
                let claimed = prev.status == Status::Todo && cur.status == Status::InProgress;
                if prev.status != cur.status && !claimed {
                    events.push(Event {
                        id,
                        title: cur.title.clone(),
                        event: EventKind::Status,
                        from: Some(prev.status),
                        to: Some(cur.status),
                        ready: is_ready,
                        text: None,
                    });
                } else if is_ready && !old.ready.contains(&id) {
                    events.push(Event {
                        id,
                        title: cur.title.clone(),
                        event: EventKind::Ready,
                        from: Some(prev.status),
                        to: Some(cur.status),
                        ready: true,
                        text: None,
                    });
                }
                prev.chat_len
            }
        };
        for e in l.chat.iter().skip(seen_chat) {
            if e.from == HUMAN && e.kind == Kind::Message {
                events.push(Event {
                    id,
                    title: cur.title.clone(),
                    event: EventKind::Message,
                    from: None,
                    to: Some(cur.status),
                    ready: is_ready,
                    text: Some(e.text.clone()),
                });
            }
        }
    }
    for (id, prev) in &old.tasks {
        if now.tasks.contains_key(id) {
            continue;
        }
        if now.skipped_ids.contains(id) {
            now.tasks.insert(*id, prev.clone());
            continue;
        }
        events.push(Event {
            id: *id,
            title: prev.title.clone(),
            event: EventKind::Deleted,
            from: Some(prev.status),
            to: None,
            ready: false,
            text: None,
        });
    }
    events
}

/// Writes the task files that do not parse to stderr.
pub fn report_skipped(bad: &[(PathBuf, String)]) {
    for (path, err) in bad {
        eprintln!("aq: skipped {}: {err}", path.display());
    }
}

/// Writes the events to stdout. With `follow`, one line or one JSON object per event.
fn print_events(events: &[Event], json: bool, follow: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    if json && !follow {
        writeln!(out, "{}", serde_json::to_string_pretty(events)?)?;
    } else {
        for e in events {
            if json {
                writeln!(out, "{}", serde_json::to_string(e)?)?;
            } else {
                writeln!(out, "{}", e.line())?;
            }
        }
    }
    out.flush()?;
    Ok(())
}

/// Polls the queue and prints the events. Without `follow`, it returns after the
/// first poll with events, or with a timeout error. With `follow`, it prints one
/// line per event and does not return.
///
/// Without `consumer`, the events are the changes after the start of the call.
/// With `consumer`, the events are the changes after the cursor of the consumer,
/// and the first poll is immediate. `session` becomes the session of the consumer.
pub fn wait(
    q: &Queue,
    timeout: Duration,
    json: bool,
    follow: bool,
    consumer: Option<&str>,
    session: Option<&str>,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut seen_bad = BTreeSet::new();
    let mut report_new = |bad: &[(PathBuf, String)]| {
        let new: Vec<_> = bad
            .iter()
            .filter(|(p, _)| !seen_bad.contains(p))
            .cloned()
            .collect();
        report_skipped(&new);
        seen_bad = bad.iter().map(|(p, _)| p.clone()).collect();
    };
    if let Some(name) = consumer {
        loop {
            let taken = take(q, name, session, |ev| print_events(ev, json, follow))?;
            report_new(&taken.bad);
            if !follow && !taken.events.is_empty() {
                return Ok(());
            }
            if !follow && Instant::now() >= deadline {
                return Err(AqError::Timeout.into());
            }
            std::thread::sleep(POLL);
        }
    }
    let first = q.load(true)?;
    report_new(&first.1);
    let mut snap = Snapshot::new(&first);
    loop {
        std::thread::sleep(POLL);
        let loaded = q.load(true)?;
        report_new(&loaded.1);
        let mut now = Snapshot::new(&loaded);
        let events = diff(&snap, &loaded, &mut now);
        snap = now;
        if !events.is_empty() {
            print_events(&events, json, follow)?;
            if !follow {
                return Ok(());
            }
        }
        if !follow && Instant::now() >= deadline {
            return Err(AqError::Timeout.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn queue() -> (tempfile::TempDir, Queue) {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::init(dir.path()).unwrap();
        (dir, q)
    }

    /// The events since `snap`; `snap` becomes the new state.
    fn step(q: &Queue, snap: &mut Snapshot) -> Vec<(u32, EventKind)> {
        let loaded = q.load(true).unwrap();
        let mut now = Snapshot::new(&loaded);
        let events = diff(snap, &loaded, &mut now);
        *snap = now;
        events.iter().map(|e| (e.id, e.event)).collect()
    }

    #[test]
    fn events_of_the_human_and_the_workers() {
        let (_d, q) = queue();
        let mut snap = Snapshot::new(&q.load(true).unwrap());
        q.create("A", |_| {}).unwrap();
        assert_eq!(step(&q, &mut snap), vec![(1, EventKind::Created)]);
        q.start(1).unwrap();
        assert_eq!(step(&q, &mut snap), vec![(1, EventKind::Status)]);
        q.claim(1, "w").unwrap();
        assert!(step(&q, &mut snap).is_empty());
        q.say(1, HUMAN, Kind::Message, "Hello").unwrap();
        q.log(1, "progress").unwrap();
        assert_eq!(step(&q, &mut snap), vec![(1, EventKind::Message)]);
        q.submit(1, "x").unwrap();
        assert_eq!(step(&q, &mut snap), vec![(1, EventKind::Status)]);
        q.reject(1, "again").unwrap();
        assert_eq!(step(&q, &mut snap), vec![(1, EventKind::Status)]);
        q.claim(1, "w").unwrap();
        q.submit(1, "x").unwrap();
        step(&q, &mut snap);
        q.approve(1).unwrap();
        assert_eq!(step(&q, &mut snap), vec![(1, EventKind::Status)]);
        q.create("B", |_| {}).unwrap();
        step(&q, &mut snap);
        q.delete(2).unwrap();
        assert_eq!(step(&q, &mut snap), vec![(2, EventKind::Deleted)]);
    }

    #[test]
    fn dependency_done_makes_ready() {
        let (_d, q) = queue();
        q.create("A", |_| {}).unwrap();
        q.create("B", |t| t.meta.depends_on = vec![1]).unwrap();
        q.start(1).unwrap();
        q.start(2).unwrap();
        q.claim(1, "w").unwrap();
        q.submit(1, "x").unwrap();
        let mut snap = Snapshot::new(&q.load(true).unwrap());
        q.approve(1).unwrap();
        assert_eq!(
            step(&q, &mut snap),
            vec![(1, EventKind::Status), (2, EventKind::Ready)]
        );
    }

    #[test]
    fn unparsable_task_is_not_deleted() {
        let (_d, q) = queue();
        let l = q.create("A", |_| {}).unwrap();
        let mut snap = Snapshot::new(&q.load(true).unwrap());
        let file = l.dir.join("task.md");
        let text = fs::read_to_string(&file).unwrap();
        fs::write(&file, "---\nid: [\n---\n").unwrap();
        assert!(step(&q, &mut snap).is_empty());
        fs::write(&file, text).unwrap();
        assert!(step(&q, &mut snap).is_empty());
    }

    /// The kinds of the events that `take` gives to the consumer `c`.
    fn take_kinds(q: &Queue, session: Option<&str>) -> Vec<(u32, EventKind)> {
        let mut seen = Vec::new();
        take(q, "c", session, |ev| {
            seen = ev.iter().map(|e| (e.id, e.event)).collect();
            Ok(())
        })
        .unwrap();
        seen
    }

    #[test]
    fn consumer_gets_each_change_once_across_calls() {
        let (_d, q) = queue();
        assert!(take_kinds(&q, Some("s1")).is_empty());
        assert_eq!(session(&q, "c").unwrap().as_deref(), Some("s1"));
        q.create("A", |_| {}).unwrap();
        q.start(1).unwrap();
        q.say(1, HUMAN, Kind::Message, "Hello").unwrap();
        assert_eq!(
            take_kinds(&q, None),
            vec![(1, EventKind::Created), (1, EventKind::Message)]
        );
        assert!(take_kinds(&q, None).is_empty());
        assert_eq!(session(&q, "c").unwrap().as_deref(), Some("s1"));
        q.claim(1, "w").unwrap();
        assert!(take_kinds(&q, None).is_empty());
        q.submit(1, "x").unwrap();
        let mut from = None;
        take(&q, "c", None, |ev| {
            from = ev[0].from;
            Ok(())
        })
        .unwrap();
        assert_eq!(from, Some(Status::InProgress));
    }

    #[test]
    fn failed_emit_keeps_the_cursor() {
        let (_d, q) = queue();
        take_kinds(&q, None);
        q.create("A", |_| {}).unwrap();
        assert!(take(&q, "c", None, |_| anyhow::bail!("closed")).is_err());
        assert_eq!(take_kinds(&q, None), vec![(1, EventKind::Created)]);
    }
}
