mod chat;
mod config;
mod editor;
mod error;
mod markdown;
mod queue;
mod task;
mod templates;
mod tui;

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::chat::{Entry, HUMAN, Kind};
use crate::error::{AqError, usage};
use crate::queue::{Loaded, Queue, join_ids, ready, waits_for_deps};
use crate::task::{PRIORITY_HIGH, PRIORITY_LOW, Status, parse_ids};

/// Writes one line to stdout. A write error, for example a closed pipe, returns
/// from the function with the error.
macro_rules! out {
    ($($arg:tt)*) => {
        writeln!(io::stdout(), $($arg)*)?
    };
}

#[derive(Parser)]
#[command(
    name = "aq",
    version,
    about = "Agent queue: tasks for coding agents, with a TUI"
)]
struct Cli {
    /// The project directory or its `.aq` directory. Default: `$AQ_DIR`, else search upward.
    #[arg(long, global = true)]
    dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Make `.aq/` with ORCHESTRATOR.md and WORKER.md.
    Init {
        /// Write the fixed blocks of ORCHESTRATOR.md and WORKER.md again. The policy sections stay.
        #[arg(long)]
        update_agent: bool,
    },
    /// Make a task. It needs a title or a description.
    Add {
        /// A short title. Without it, an agent sets the title later with `aq title`.
        title: Option<String>,
        /// 1 (high), 2 (normal) or 3 (low).
        #[arg(short, long, default_value_t = 2)]
        priority: u8,
        /// Ids of the tasks that must be done first.
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        after: Vec<String>,
        /// The description.
        #[arg(short = 'm', long)]
        description: Option<String>,
        /// Open the task in $EDITOR.
        #[arg(short, long)]
        edit: bool,
    },
    /// Edit a task in $EDITOR.
    Edit { id: u32 },
    /// Set the short title of a task.
    Title {
        id: u32,
        #[arg(short, long)]
        message: String,
    },
    /// Show the tasks.
    List {
        #[arg(long)]
        status: Option<Status>,
        /// Also show the archived tasks.
        #[arg(long)]
        archived: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show the ready task with the highest priority.
    Next {
        /// Show all ready tasks.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show a task and its chat.
    Show {
        id: u32,
        #[arg(long)]
        json: bool,
    },
    /// Wait until a task becomes ready or a task leaves in_progress.
    Wait {
        /// Seconds. Exit code 4 on timeout.
        #[arg(long, default_value_t = 540)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// Claim a todo task.
    Claim {
        id: u32,
        #[arg(long)]
        by: String,
    },
    /// Show the chat of a task.
    Chat {
        id: u32,
        /// Show only the messages after the read mark of the reader.
        #[arg(long)]
        unread: bool,
        /// The reader. Default: the assignee of the task, else `human`.
        #[arg(long)]
        by: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Write a chat message. A message of the human answers the question of a blocked task.
    Say {
        id: u32,
        #[arg(short, long)]
        message: String,
        /// The author.
        #[arg(long, default_value = HUMAN)]
        by: String,
    },
    /// Write a progress note as the assignee.
    Log {
        id: u32,
        #[arg(short, long)]
        message: String,
    },
    /// Ask the human a question. The task goes to blocked.
    Ask {
        id: u32,
        #[arg(short, long)]
        message: String,
    },
    /// Finish the work. The task goes to review.
    Submit {
        id: u32,
        #[arg(short, long)]
        message: String,
    },
    /// Give up the task. The task goes to failed.
    Fail {
        id: u32,
        #[arg(short, long)]
        message: String,
    },
    /// Accept a task in review. The task goes to done.
    Approve { id: u32 },
    /// Reject a task in review with a comment. The task goes to todo.
    Reject {
        id: u32,
        /// The comment. Without it, $EDITOR opens.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Reset a task in in_progress or failed to todo.
    Retry { id: u32 },
    /// Move the done tasks to .aq/archive/.
    Archive,
    /// Print the start text for the orchestrator or for a worker.
    Instructions {
        #[arg(long)]
        worker: Option<u32>,
    },
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let code = if e.use_stderr() { 1 } else { 0 };
            let _ = e.print();
            std::process::exit(code);
        }
    };
    if let Err(e) = run(cli) {
        // A closed stdout, as in `aq show 1 | head`, is not an error.
        if is_broken_pipe(&e) {
            std::process::exit(0);
        }
        eprintln!("aq: {e:#}");
        let code = e.downcast_ref::<AqError>().map_or(1, AqError::exit_code);
        std::process::exit(code);
    }
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
    })
}

/// The project directory: `--dir` without a trailing `.aq`, else `cwd`.
fn project_dir(dir: Option<&Path>, cwd: &Path) -> PathBuf {
    match dir {
        Some(d) if d.file_name().is_some_and(|n| n == queue::AQ_DIR) => {
            d.parent().unwrap_or(d).to_path_buf()
        }
        Some(d) => d.to_path_buf(),
        None => cwd.to_path_buf(),
    }
}

fn run(cli: Cli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let find = || Queue::find(&cwd, cli.dir.as_deref());
    let project = project_dir(cli.dir.as_deref(), &cwd);
    let Some(cmd) = cli.cmd else {
        let q = match find() {
            Ok(q) => Some(q),
            Err(e) if matches!(e.downcast_ref(), Some(AqError::NoQueue)) => None,
            Err(e) => return Err(e),
        };
        return tui::run(q, project);
    };
    match cmd {
        Cmd::Init { update_agent: true } => {
            let q = find()?;
            q.update_instructions()?;
            out!("updated the aq blocks in {}", q.root.display());
        }
        Cmd::Init {
            update_agent: false,
        } => {
            let q = Queue::init(&project)?;
            out!("initialized {}", q.root.display());
        }
        Cmd::Add {
            title,
            priority,
            after,
            description,
            edit,
        } => {
            let q = find()?;
            check_priority(priority)?;
            let deps = parse_ids(&after.join(","))?;
            let title = title.unwrap_or_default();
            if title.trim().is_empty() && description.as_deref().is_none_or(|d| d.trim().is_empty())
            {
                return Err(usage("give a title or a description (-m)"));
            }
            let l = q.create(title.trim(), |t| {
                t.meta.priority = priority;
                t.meta.depends_on = deps;
                t.description = description.unwrap_or_default();
            })?;
            out!("added task {}: {}", l.task.id(), l.task.display_title());
            if edit {
                editor::edit_task_cli(&q, l.task.id())?;
            }
        }
        Cmd::Edit { id } => editor::edit_task_cli(&find()?, id)?,
        Cmd::Title { id, message } => {
            let l = find()?.set_title(id, &message)?;
            out!("task {id}: {}", l.task.meta.title);
        }
        Cmd::List {
            status,
            archived,
            json,
        } => {
            let q = find()?;
            let (tasks, bad) = q.load(true)?;
            let shown: Vec<&Loaded> = tasks
                .iter()
                .filter(|l| archived || !l.archived)
                .filter(|l| status.is_none_or(|s| l.task.status() == s))
                .collect();
            if json {
                let out: Vec<_> = shown.iter().map(|l| TaskJson::new(l, &tasks)).collect();
                out!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                for l in shown {
                    out!("{}", list_line(l, &tasks));
                }
            }
            for (path, err) in bad {
                eprintln!("aq: skipped {}: {err}", path.display());
            }
        }
        Cmd::Next { all, json } => {
            let q = find()?;
            let (tasks, _) = q.load(true)?;
            let mut ids = ready(&tasks);
            if !all {
                ids.truncate(1);
            }
            let shown: Vec<&Loaded> = ids
                .iter()
                .filter_map(|id| tasks.iter().find(|l| l.task.id() == *id))
                .collect();
            if json {
                let out: Vec<_> = shown.iter().map(|l| TaskJson::new(l, &tasks)).collect();
                out!("{}", serde_json::to_string_pretty(&out)?);
            } else if shown.is_empty() {
                out!("no task is ready");
            } else {
                for l in shown {
                    out!("{}", list_line(l, &tasks));
                }
            }
        }
        Cmd::Show { id, json } => {
            let q = find()?;
            let l = q.get(id)?;
            let (tasks, _) = q.load(true)?;
            if json {
                #[derive(Serialize)]
                struct Show<'a> {
                    #[serde(flatten)]
                    task: TaskJson<'a>,
                    chat: &'a [Entry],
                }
                let out = Show {
                    task: TaskJson::new(&l, &tasks),
                    chat: &l.chat,
                };
                out!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                print_show(&l, &tasks)?;
            }
        }
        Cmd::Wait { timeout, json } => wait(&find()?, Duration::from_secs(timeout), json)?,
        Cmd::Claim { id, by } => {
            let l = find()?.claim(id, &by)?;
            out!("{by} claimed task {id}: {}", l.task.display_title());
        }
        Cmd::Chat {
            id,
            unread,
            by,
            json,
        } => {
            let q = find()?;
            let l = q.get(id)?;
            let reader = by
                .or_else(|| l.task.meta.assignee.clone())
                .unwrap_or_else(|| HUMAN.into());
            let shown: Vec<&Entry> = if unread {
                let marks = chat::read_marks(&l.marks_path());
                chat::unread(&l.chat, &marks, &reader).collect()
            } else {
                l.chat.iter().collect()
            };
            if json {
                out!("{}", serde_json::to_string_pretty(&shown)?);
            } else if shown.is_empty() && unread {
                out!("no unread messages");
            } else {
                for e in shown {
                    print_entry(e)?;
                }
            }
            let _lock = q.lock()?;
            chat::set_read_mark(&l.marks_path(), &reader, l.chat.len())?;
        }
        Cmd::Say { id, message, by } => {
            let l = find()?.say(id, &by, Kind::Message, &message)?;
            out!("sent; task {id} is {}", l.task.status());
        }
        Cmd::Log { id, message } => {
            find()?.log(id, &message)?;
        }
        Cmd::Ask { id, message } => {
            find()?.ask(id, &message)?;
            out!("task {id} is blocked until the human answers; stop now");
        }
        Cmd::Submit { id, message } => {
            find()?.submit(id, &message)?;
            out!("task {id} is in review");
        }
        Cmd::Fail { id, message } => {
            find()?.fail(id, &message)?;
            out!("task {id} failed");
        }
        Cmd::Approve { id } => {
            find()?.approve(id)?;
            out!("task {id} is done");
        }
        Cmd::Reject { id, message } => {
            let q = find()?;
            let comment = match message {
                Some(m) => m,
                None => editor::edit_text("", ".md")?,
            };
            if comment.trim().is_empty() {
                return Err(usage("the review comment is empty"));
            }
            q.reject(id, comment.trim())?;
            out!("task {id} is todo again");
        }
        Cmd::Retry { id } => {
            find()?.retry(id)?;
            out!("task {id} is todo again");
        }
        Cmd::Archive => {
            let ids = find()?.archive()?;
            if ids.is_empty() {
                out!("no done tasks");
            } else {
                out!("archived {}", join_ids(&ids));
            }
        }
        Cmd::Instructions { worker } => match worker {
            Some(id) => {
                find()?.get(id)?;
                out!("{}", templates::worker_start(id));
            }
            None => out!("{}", templates::orchestrator_start()),
        },
    }
    Ok(())
}

fn check_priority(p: u8) -> Result<()> {
    if (PRIORITY_HIGH..=PRIORITY_LOW).contains(&p) {
        Ok(())
    } else {
        Err(usage("the priority must be 1, 2 or 3"))
    }
}

#[derive(Serialize)]
struct TaskJson<'a> {
    #[serde(flatten)]
    task: &'a task::Task,
    ready: bool,
    archived: bool,
}

impl<'a> TaskJson<'a> {
    fn new(l: &'a Loaded, all: &[Loaded]) -> Self {
        TaskJson {
            task: &l.task,
            ready: l.task.status() == Status::Todo && !waits_for_deps(l, all),
            archived: l.archived,
        }
    }
}

fn list_line(l: &Loaded, all: &[Loaded]) -> String {
    let m = &l.task.meta;
    let mut line = format!(
        "{:>4}  {:<11}  p{}  {}",
        m.id,
        m.status.as_str(),
        m.priority,
        l.task.display_title()
    );
    if !l.task.has_title() {
        line.push_str("  (no title)");
    }
    if !m.depends_on.is_empty() {
        line.push_str(&format!("  [after {}]", join_ids(&m.depends_on)));
    }
    if waits_for_deps(l, all) {
        line.push_str("  (waits)");
    }
    if let Some(a) = &m.assignee {
        line.push_str(&format!("  ({a})"));
    }
    if l.archived {
        line.push_str("  archived");
    }
    line
}

fn print_show(l: &Loaded, all: &[Loaded]) -> io::Result<()> {
    let m = &l.task.meta;
    if l.task.has_title() {
        out!("# {} {}", m.id, m.title);
    } else {
        out!("# {} (no title)", m.id);
    }
    out!("status: {}  priority: {}", m.status, m.priority);
    if !m.depends_on.is_empty() {
        let deps: Vec<String> = m
            .depends_on
            .iter()
            .map(|d| match all.iter().find(|t| t.task.id() == *d) {
                Some(t) => format!("{d} ({})", t.task.status()),
                None => format!("{d} (missing)"),
            })
            .collect();
        out!("depends on: {}", deps.join(", "));
    }
    if let Some(a) = &m.assignee {
        out!("assignee: {a}");
    }
    out!("\n## Description\n\n{}", l.task.description);
    if !l.task.acceptance.is_empty() {
        out!("\n## Acceptance\n\n{}", l.task.acceptance);
    }
    out!("\n## Chat\n");
    for e in &l.chat {
        print_entry(e)?;
    }
    Ok(())
}

fn print_entry(e: &Entry) -> io::Result<()> {
    let kind = match e.kind {
        Kind::Message => String::new(),
        k => format!(" [{}]", k.as_str()),
    };
    let ts = e.ts.format("%Y-%m-%d %H:%M");
    let mut lines = e.text.lines();
    out!("{ts}  {}{kind}: {}", e.from, lines.next().unwrap_or(""));
    for line in lines {
        out!("    {line}");
    }
    Ok(())
}

#[derive(Serialize)]
struct Change {
    id: u32,
    title: String,
    from: Option<Status>,
    to: Status,
    ready: bool,
}

/// Polls the queue until a task becomes ready or a task leaves `in_progress`.
fn wait(q: &Queue, timeout: Duration, json: bool) -> Result<()> {
    let (start, _) = q.load(true)?;
    let before: HashMap<u32, Status> = start
        .iter()
        .map(|l| (l.task.id(), l.task.status()))
        .collect();
    let ready_before: HashSet<u32> = ready(&start).into_iter().collect();
    let deadline = Instant::now() + timeout;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let (tasks, _) = q.load(true)?;
        let ready_now: HashSet<u32> = ready(&tasks).into_iter().collect();
        let mut changes = Vec::new();
        for l in &tasks {
            let id = l.task.id();
            let old = before.get(&id).copied();
            let new_ready = ready_now.contains(&id) && !ready_before.contains(&id);
            let left_progress =
                old == Some(Status::InProgress) && l.task.status() != Status::InProgress;
            if new_ready || left_progress {
                changes.push(Change {
                    id,
                    title: l.task.display_title(),
                    from: old,
                    to: l.task.status(),
                    ready: ready_now.contains(&id),
                });
            }
        }
        if !changes.is_empty() {
            if json {
                out!("{}", serde_json::to_string_pretty(&changes)?);
            } else {
                for c in changes {
                    let from = c.from.map_or("new".to_string(), |s| s.to_string());
                    let ready = if c.ready { " (ready)" } else { "" };
                    out!("task {}: {from} -> {}{ready}  {}", c.id, c.to, c.title);
                }
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(AqError::Timeout.into());
        }
    }
}
