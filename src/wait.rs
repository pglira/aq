//! `aq wait`: polls the queue and reports the changes that matter to the orchestrator.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;

use crate::chat::{HUMAN, Kind};
use crate::error::AqError;
use crate::queue::{LoadResult, Loaded, Queue, ready};
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
#[derive(Debug, Clone)]
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
    /// The task files that do not parse.
    skipped: BTreeSet<PathBuf>,
    /// The ids of the task directories with a file that does not parse.
    skipped_ids: HashSet<u32>,
}

impl Snapshot {
    pub fn new((tasks, bad): &LoadResult) -> Snapshot {
        Snapshot {
            tasks: tasks.iter().map(|l| (l.task.id(), state(l))).collect(),
            ready: ready(tasks).into_iter().collect(),
            skipped: bad.iter().map(|(p, _)| p.clone()).collect(),
            skipped_ids: bad.iter().filter_map(|(p, _)| dir_id(p)).collect(),
        }
    }
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
        match old.tasks.get(&id) {
            None => events.push(Event {
                id,
                title: cur.title.clone(),
                event: EventKind::Created,
                from: None,
                to: Some(cur.status),
                ready: is_ready,
                text: None,
            }),
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
                for e in l.chat.iter().skip(prev.chat_len) {
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

/// Polls the queue and prints the events. Without `follow`, it returns after the
/// first poll with events, or with a timeout error. With `follow`, it prints one
/// line per event and does not return.
pub fn wait(q: &Queue, timeout: Duration, json: bool, follow: bool) -> Result<()> {
    let first = q.load(true)?;
    report_skipped(&first.1);
    let mut snap = Snapshot::new(&first);
    let deadline = Instant::now() + timeout;
    loop {
        std::thread::sleep(POLL);
        let loaded = q.load(true)?;
        let new_bad: Vec<_> = loaded
            .1
            .iter()
            .filter(|(p, _)| !snap.skipped.contains(p))
            .cloned()
            .collect();
        report_skipped(&new_bad);
        let mut now = Snapshot::new(&loaded);
        let events = diff(&snap, &loaded, &mut now);
        snap = now;
        let mut out = io::stdout().lock();
        if follow {
            for e in &events {
                if json {
                    writeln!(out, "{}", serde_json::to_string(e)?)?;
                } else {
                    writeln!(out, "{}", e.line())?;
                }
            }
            out.flush()?;
            continue;
        }
        if !events.is_empty() {
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&events)?)?;
            } else {
                for e in &events {
                    writeln!(out, "{}", e.line())?;
                }
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
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
        assert!(snap.skipped.contains(&file));
        fs::write(&file, text).unwrap();
        assert!(step(&q, &mut snap).is_empty());
    }
}
