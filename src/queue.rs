use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::chat::{self, AQ, Entry, HUMAN, Kind};
use crate::error::{AqError, conflict, usage};
use crate::task::{Status, Task, now, slug};
use crate::templates;

pub const AQ_DIR: &str = ".aq";

/// A task with its directory and its chat, as loaded from disk.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub task: Task,
    pub dir: PathBuf,
    pub chat: Vec<Entry>,
    pub archived: bool,
}

impl Loaded {
    pub fn dir_name(&self) -> String {
        self.dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn chat_path(&self) -> PathBuf {
        self.dir.join("chat.jsonl")
    }

    pub fn marks_path(&self) -> PathBuf {
        self.dir.join(".read")
    }
}

#[derive(Debug, Clone)]
pub struct Queue {
    /// The `.aq` directory.
    pub root: PathBuf,
}

/// The tasks that load, and the task files that do not parse, with the error.
pub type LoadResult = (Vec<Loaded>, Vec<(PathBuf, String)>);

/// An exclusive lock on `.aq/.lock`. The lock is released on drop.
pub struct Lock(#[allow(dead_code)] File);

impl Queue {
    /// Finds the queue: `$AQ_DIR`, else `.aq/` in `start` or one of its parents.
    pub fn find(start: &Path, explicit: Option<&Path>) -> Result<Queue> {
        if let Some(dir) = explicit
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("AQ_DIR").map(PathBuf::from))
        {
            let root = if dir.file_name().is_some_and(|n| n == AQ_DIR) {
                dir
            } else {
                dir.join(AQ_DIR)
            };
            if root.is_dir() {
                return Queue::open(root);
            }
            return Err(AqError::NoQueue.into());
        }
        let mut cur = Some(start);
        while let Some(dir) = cur {
            let root = dir.join(AQ_DIR);
            if root.join("tasks").is_dir() {
                return Queue::open(root);
            }
            cur = dir.parent();
        }
        Err(AqError::NoQueue.into())
    }

    fn open(root: PathBuf) -> Result<Queue> {
        let root = root.canonicalize().unwrap_or(root);
        Ok(Queue { root })
    }

    /// Makes `.aq/` in `project`. Files that exist are not changed.
    pub fn init(project: &Path) -> Result<Queue> {
        let root = project.join(AQ_DIR);
        fs::create_dir_all(root.join("tasks"))?;
        fs::create_dir_all(root.join("archive"))?;
        for (name, text) in [
            ("ORCHESTRATOR.md", templates::orchestrator_template()),
            ("WORKER.md", templates::worker_template()),
        ] {
            let path = root.join(name);
            if !path.exists() {
                fs::write(&path, text)?;
            }
        }
        Queue::open(root)
    }

    pub fn tasks_dir(&self) -> PathBuf {
        self.root.join("tasks")
    }

    pub fn archive_dir(&self) -> PathBuf {
        self.root.join("archive")
    }

    pub fn orchestrator_md(&self) -> PathBuf {
        self.root.join("ORCHESTRATOR.md")
    }

    pub fn worker_md(&self) -> PathBuf {
        self.root.join("WORKER.md")
    }

    pub fn lock(&self) -> Result<Lock> {
        let f = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".lock"))?;
        f.lock().context("lock .aq/.lock")?;
        Ok(Lock(f))
    }

    /// Loads all tasks in `tasks/`, and in `archive/` if `archived` is true.
    /// Tasks that do not parse are returned in the second list, with the error.
    pub fn load(&self, archived: bool) -> Result<LoadResult> {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        let mut dirs = vec![(self.tasks_dir(), false)];
        if archived {
            dirs.push((self.archive_dir(), true));
        }
        for (base, is_archive) in dirs {
            let Ok(rd) = fs::read_dir(&base) else {
                continue;
            };
            for ent in rd.flatten() {
                let dir = ent.path();
                let file = dir.join("task.md");
                if !file.is_file() {
                    continue;
                }
                match load_dir(&dir, is_archive) {
                    Ok(l) => ok.push(l),
                    Err(e) => bad.push((file, format!("{e:#}"))),
                }
            }
        }
        ok.sort_by_key(|l| l.task.id());
        Ok((ok, bad))
    }

    pub fn get(&self, id: u32) -> Result<Loaded> {
        for (base, archived) in [(self.tasks_dir(), false), (self.archive_dir(), true)] {
            if let Some(dir) = find_dir(&base, id) {
                return load_dir(&dir, archived);
            }
        }
        Err(usage(format!("task {id} not found")))
    }

    fn next_id(&self) -> u32 {
        let mut max = 0;
        for base in [self.tasks_dir(), self.archive_dir()] {
            let Ok(rd) = fs::read_dir(base) else { continue };
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                if let Some(n) = name.split('-').next().and_then(|n| n.parse::<u32>().ok()) {
                    max = max.max(n);
                }
            }
        }
        max + 1
    }

    /// Makes a new task. `edit` changes the task before it is written; the id is set.
    /// The directory name is `<id>-<slug>`, with the slug from the title, else from
    /// the first line of the description. The name does not change afterward.
    pub fn create(&self, title: &str, edit: impl FnOnce(&mut Task)) -> Result<Loaded> {
        let _lock = self.lock()?;
        let id = self.next_id();
        let mut task = Task::new(id, title);
        edit(&mut task);
        task.meta.id = id;
        task.validate().map_err(|e| usage(format!("{e:#}")))?;
        self.check_deps(&task)?;
        let dir = self
            .tasks_dir()
            .join(format!("{id:04}-{}", slug(&task.display_title())));
        fs::create_dir_all(&dir)?;
        write_atomic(&dir.join("task.md"), &task.render())?;
        chat::append(
            &dir.join("chat.jsonl"),
            &Entry::new(AQ, Kind::Event, "created"),
        )?;
        load_dir(&dir, false)
    }

    fn check_deps(&self, task: &Task) -> Result<()> {
        for dep in &task.meta.depends_on {
            if find_dir(&self.tasks_dir(), *dep).is_none()
                && find_dir(&self.archive_dir(), *dep).is_none()
            {
                return Err(usage(format!("dependency {dep} does not exist")));
            }
        }
        Ok(())
    }

    /// Writes the content and the human fields of a task: title, priority, dependencies,
    /// description and acceptance. The status and the claim do not change.
    pub fn update_content(&self, edited: &Task) -> Result<()> {
        let _lock = self.lock()?;
        let cur = self.get(edited.id())?;
        let mut task = cur.task.clone();
        task.meta.title = edited.meta.title.clone();
        task.meta.priority = edited.meta.priority;
        task.meta.depends_on = edited.meta.depends_on.clone();
        task.description = edited.description.clone();
        task.acceptance = edited.acceptance.clone();
        if task == cur.task {
            return Ok(());
        }
        task.validate().map_err(|e| usage(format!("{e:#}")))?;
        self.check_deps(&task)?;
        task.meta.updated = now();
        write_atomic(&cur.dir.join("task.md"), &task.render())
    }

    /// Sets the title of a task. The status, the claim and the directory name do not change.
    pub fn set_title(&self, id: u32, title: &str) -> Result<Loaded> {
        let title = title.trim();
        if title.is_empty() {
            return Err(usage("the title is empty"));
        }
        if title.contains('\n') {
            return Err(usage("the title must be one line"));
        }
        let by = self.assignee(id)?;
        let _lock = self.lock()?;
        let cur = self.get(id)?;
        let mut task = cur.task.clone();
        if task.meta.title == title {
            return Ok(cur);
        }
        task.meta.title = title.to_string();
        task.meta.updated = now();
        write_atomic(&cur.dir.join("task.md"), &task.render())?;
        chat::append(
            &cur.chat_path(),
            &Entry::new(&by, Kind::Event, &format!("title: {title}")),
        )?;
        load_dir(&cur.dir, cur.archived)
    }

    /// Runs one status change under the lock: reads the task again, checks the
    /// transition, writes the task and appends the chat entry.
    fn transition(
        &self,
        id: u32,
        from: &[Status],
        to: Status,
        entry: Entry,
        change: impl FnOnce(&mut Task),
    ) -> Result<Loaded> {
        let _lock = self.lock()?;
        let cur = self.get(id)?;
        if cur.archived {
            return Err(conflict(format!("task {id} is archived")));
        }
        let st = cur.task.status();
        if !from.contains(&st) {
            let allowed: Vec<_> = from.iter().map(|s| s.as_str()).collect();
            return Err(conflict(format!(
                "task {id} is {st}; this change needs status {}",
                allowed.join(" or ")
            )));
        }
        let mut task = cur.task.clone();
        task.meta.status = to;
        if to == Status::Todo {
            task.meta.assignee = None;
            task.meta.claimed_at = None;
        }
        change(&mut task);
        task.meta.updated = now();
        write_atomic(&cur.dir.join("task.md"), &task.render())?;
        chat::append(&cur.chat_path(), &entry)?;
        load_dir(&cur.dir, false)
    }

    pub fn claim(&self, id: u32, by: &str) -> Result<Loaded> {
        let deps = self.open_deps(id)?;
        if !deps.is_empty() {
            return Err(conflict(format!(
                "task {id} waits for task(s) {}",
                join_ids(&deps)
            )));
        }
        let by = by.to_string();
        self.transition(
            id,
            &[Status::Todo],
            Status::InProgress,
            Entry::new(&by, Kind::Event, "claimed"),
            |t| {
                t.meta.assignee = Some(by.clone());
                t.meta.claimed_at = Some(now());
            },
        )
    }

    pub fn submit(&self, id: u32, msg: &str) -> Result<Loaded> {
        let by = self.assignee(id)?;
        let e = Entry::new(&by, Kind::Event, &format!("submitted: {msg}"));
        self.transition(id, &[Status::InProgress], Status::Review, e, |_| {})
    }

    pub fn ask(&self, id: u32, question: &str) -> Result<Loaded> {
        let by = self.assignee(id)?;
        self.transition(
            id,
            &[Status::InProgress],
            Status::Blocked,
            Entry::new(&by, Kind::Question, question),
            |_| {},
        )
    }

    pub fn fail(&self, id: u32, reason: &str) -> Result<Loaded> {
        let by = self.assignee(id)?;
        self.transition(
            id,
            &[Status::InProgress],
            Status::Failed,
            Entry::new(&by, Kind::Event, &format!("failed: {reason}")),
            |_| {},
        )
    }

    pub fn approve(&self, id: u32) -> Result<Loaded> {
        self.transition(
            id,
            &[Status::Review],
            Status::Done,
            Entry::new(HUMAN, Kind::Event, "approved"),
            |_| {},
        )
    }

    pub fn reject(&self, id: u32, comment: &str) -> Result<Loaded> {
        self.transition(
            id,
            &[Status::Review],
            Status::Todo,
            Entry::new(HUMAN, Kind::Review, comment),
            |_| {},
        )
    }

    pub fn retry(&self, id: u32) -> Result<Loaded> {
        self.transition(
            id,
            &[Status::InProgress, Status::Failed],
            Status::Todo,
            Entry::new(HUMAN, Kind::Event, "retry"),
            |_| {},
        )
    }

    /// Writes a chat message. A message of the human to a `blocked` task is the
    /// answer to the question, and the task goes to `todo`.
    pub fn say(&self, id: u32, from: &str, kind: Kind, text: &str) -> Result<Loaded> {
        if from == HUMAN && kind == Kind::Message {
            let cur = self.get(id)?;
            if cur.task.status() == Status::Blocked {
                return self.transition(
                    id,
                    &[Status::Blocked],
                    Status::Todo,
                    Entry::new(HUMAN, Kind::Message, text),
                    |_| {},
                );
            }
        }
        let _lock = self.lock()?;
        let cur = self.get(id)?;
        chat::append(&cur.chat_path(), &Entry::new(from, kind, text))?;
        load_dir(&cur.dir, cur.archived)
    }

    /// Writes a log entry as the assignee of the task.
    pub fn log(&self, id: u32, text: &str) -> Result<Loaded> {
        let by = self.assignee(id)?;
        self.say(id, &by, Kind::Log, text)
    }

    fn assignee(&self, id: u32) -> Result<String> {
        Ok(self
            .get(id)?
            .task
            .meta
            .assignee
            .unwrap_or_else(|| "agent".into()))
    }

    /// The dependencies of a task that are not `done`.
    pub fn open_deps(&self, id: u32) -> Result<Vec<u32>> {
        let t = self.get(id)?;
        let mut open = Vec::new();
        for dep in t.task.meta.depends_on {
            match self.get(dep) {
                Ok(d) if d.task.status() == Status::Done => {}
                _ => open.push(dep),
            }
        }
        Ok(open)
    }

    /// Moves the directories of all `done` tasks to `archive/`.
    pub fn archive(&self) -> Result<Vec<u32>> {
        let _lock = self.lock()?;
        let (tasks, _) = self.load(false)?;
        fs::create_dir_all(self.archive_dir())?;
        let mut moved = Vec::new();
        for l in tasks.iter().filter(|l| l.task.status() == Status::Done) {
            fs::rename(&l.dir, self.archive_dir().join(l.dir_name()))?;
            moved.push(l.task.id());
        }
        Ok(moved)
    }

    /// Writes the fixed blocks of `ORCHESTRATOR.md` and `WORKER.md` again.
    pub fn update_instructions(&self) -> Result<()> {
        for (path, block) in [
            (self.orchestrator_md(), templates::orchestrator_block()),
            (self.worker_md(), templates::worker_block()),
        ] {
            let old = fs::read_to_string(&path).unwrap_or_default();
            write_atomic(&path, &templates::replace_block(&old, &block))?;
        }
        Ok(())
    }
}

/// The ids of the `todo` tasks in `tasks/` whose dependencies are all `done`, by
/// priority and id. `tasks` must include the archived tasks, so that an archived
/// dependency counts as `done`.
pub fn ready(tasks: &[Loaded]) -> Vec<u32> {
    let mut ready: Vec<&Loaded> = tasks
        .iter()
        .filter(|l| !l.archived && l.task.status() == Status::Todo && !waits_for_deps(l, tasks))
        .collect();
    ready.sort_by_key(|l| (l.task.meta.priority, l.task.id()));
    ready.iter().map(|l| l.task.id()).collect()
}

/// True if the task is `todo` and a dependency is not `done`. A dependency that is
/// not in `tasks` is not `done`.
pub fn waits_for_deps(l: &Loaded, tasks: &[Loaded]) -> bool {
    l.task.status() == Status::Todo
        && l.task.meta.depends_on.iter().any(|d| {
            tasks
                .iter()
                .find(|t| t.task.id() == *d)
                .is_none_or(|t| t.task.status() != Status::Done)
        })
}

pub fn join_ids(ids: &[u32]) -> String {
    ids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn find_dir(base: &Path, id: u32) -> Option<PathBuf> {
    let prefix = format!("{id:04}-");
    fs::read_dir(base).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        (name.starts_with(&prefix) && e.path().join("task.md").is_file()).then(|| e.path())
    })
}

fn load_dir(dir: &Path, archived: bool) -> Result<Loaded> {
    let file = dir.join("task.md");
    let text = fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    let task = Task::parse(&text).with_context(|| format!("{}", file.display()))?;
    let chat = chat::read(&dir.join("chat.jsonl"))?;
    Ok(Loaded {
        task,
        dir: dir.to_path_buf(),
        chat,
        archived,
    })
}

/// Writes a file through a temporary file in the same directory and a rename,
/// so that a reader never sees a partial file.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> (tempfile::TempDir, Queue) {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::init(dir.path()).unwrap();
        (dir, q)
    }

    #[test]
    fn lifecycle() {
        let (_d, q) = queue();
        let a = q.create("First", |_| {}).unwrap();
        let b = q.create("Second", |t| t.meta.depends_on = vec![1]).unwrap();
        assert_eq!(a.task.id(), 1);
        assert_eq!(b.task.id(), 2);
        let (tasks, _) = q.load(false).unwrap();
        assert_eq!(ready(&tasks), vec![1]);

        assert!(q.claim(2, "w").is_err());
        q.claim(1, "worker-1").unwrap();
        let err = q.claim(1, "worker-x").unwrap_err();
        assert_eq!(err.downcast_ref::<AqError>().unwrap().exit_code(), 2);

        q.ask(1, "Which one?").unwrap();
        let l = q.say(1, HUMAN, Kind::Message, "This one.").unwrap();
        assert_eq!(l.task.status(), Status::Todo);
        assert_eq!(l.task.meta.assignee, None);

        q.claim(1, "worker-1").unwrap();
        q.submit(1, "done it").unwrap();
        let l = q.reject(1, "again").unwrap();
        assert_eq!(l.task.status(), Status::Todo);
        q.claim(1, "worker-1").unwrap();
        q.submit(1, "done it").unwrap();
        assert!(q.fail(1, "x").is_err());
        q.approve(1).unwrap();

        let (tasks, _) = q.load(false).unwrap();
        assert_eq!(ready(&tasks), vec![2]);
        assert_eq!(q.archive().unwrap(), vec![1]);
        assert!(q.get(1).unwrap().archived);
        let (tasks, _) = q.load(false).unwrap();
        assert_eq!(tasks.len(), 1);
        let (tasks, _) = q.load(true).unwrap();
        assert_eq!(ready(&tasks), vec![2]);
        assert_eq!(q.create("Third", |_| {}).unwrap().task.id(), 3);
    }

    #[test]
    fn failed_dependency_blocks() {
        let (_d, q) = queue();
        q.create("A", |_| {}).unwrap();
        q.create("B", |t| t.meta.depends_on = vec![1]).unwrap();
        q.claim(1, "w").unwrap();
        q.fail(1, "no").unwrap();
        let (tasks, _) = q.load(false).unwrap();
        assert!(ready(&tasks).is_empty());
        q.retry(1).unwrap();
        let (tasks, _) = q.load(false).unwrap();
        assert_eq!(ready(&tasks), vec![1]);
    }

    #[test]
    fn task_without_title() {
        let (_d, q) = queue();
        assert!(q.create("", |_| {}).is_err());
        let l = q
            .create("", |t| t.description = "Fix the cache.\nMore text.".into())
            .unwrap();
        assert_eq!(l.dir_name(), "0001-fix-the-cache");
        assert!(!l.task.has_title());
        let l = q.set_title(1, " Cache fix ").unwrap();
        assert_eq!(l.task.meta.title, "Cache fix");
        assert_eq!(l.dir_name(), "0001-fix-the-cache");
        assert!(q.set_title(1, " ").is_err());
    }

    #[test]
    fn unknown_dependency() {
        let (_d, q) = queue();
        assert!(q.create("A", |t| t.meta.depends_on = vec![7]).is_err());
    }

    #[test]
    fn find_walks_up() {
        let (d, _q) = queue();
        let sub = d.path().join("a/b");
        fs::create_dir_all(&sub).unwrap();
        let q = Queue::find(&sub, None).unwrap();
        assert_eq!(q.root, d.path().canonicalize().unwrap().join(AQ_DIR));
    }
}
