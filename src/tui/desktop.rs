//! Desktop notifications about changes of the task statuses.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};

use crate::queue::Loaded;
use crate::task::Status;

/// The status of each task in `tasks`.
pub fn statuses(tasks: &[Loaded]) -> HashMap<u32, Status> {
    tasks
        .iter()
        .map(|l| (l.task.id(), l.task.status()))
        .collect()
}

/// The notification texts for the tasks whose status in `tasks` differs from
/// `before`, for example `aq: #3 Fix cache → review`. Tasks that are not in
/// `before` and tasks in `skip` give no text.
pub fn status_changes(
    before: &HashMap<u32, Status>,
    tasks: &[Loaded],
    skip: &HashSet<u32>,
) -> Vec<String> {
    let mut out = Vec::new();
    for l in tasks {
        let id = l.task.id();
        let now = l.task.status();
        match before.get(&id) {
            Some(old) if *old != now && !skip.contains(&id) => {
                let title = l.task.meta.title.trim();
                out.push(if title.is_empty() {
                    format!("aq: #{id} → {now}")
                } else {
                    format!("aq: #{id} {title} → {now}")
                });
            }
            _ => {}
        }
    }
    out
}

/// Shows `text` as a desktop notification with `notify-send`. The command runs
/// in the background. A missing `notify-send` is ignored.
pub fn send(text: &str) {
    let child = Command::new("notify-send")
        .arg("--app-name=aq")
        .arg(text)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Queue;

    #[test]
    fn changes_list_only_known_and_not_skipped_tasks() {
        let d = tempfile::tempdir().unwrap();
        let q = Queue::init(d.path()).unwrap();
        q.create("Fix cache", |_| {}).unwrap();
        q.create("", |t| t.description = "no title".into()).unwrap();
        q.create("Docs", |_| {}).unwrap();
        let (tasks, _) = q.load(true).unwrap();
        let before = statuses(&tasks);
        let none = HashSet::new();
        assert!(status_changes(&before, &tasks, &none).is_empty());
        for id in [1, 2, 3] {
            q.start(id).unwrap();
            q.claim(id, "w").unwrap();
        }
        q.create("New", |t| t.meta.status = Status::Review).unwrap();
        let (tasks, _) = q.load(true).unwrap();
        assert_eq!(
            status_changes(&before, &tasks, &HashSet::from([3])),
            vec!["aq: #1 Fix cache → in_progress", "aq: #2 → in_progress"]
        );
    }
}
