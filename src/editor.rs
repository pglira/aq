use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Command;
use std::sync::RwLock;

use anyhow::{Context, Result, bail};

use crate::config;
use crate::queue::Queue;
use crate::task::Task;

/// The editor command of the config file. `None` until the first use, or until
/// [`set_command`] sets it.
static CONFIGURED: RwLock<Option<String>> = RwLock::new(None);

/// Sets the editor command of the config file. An empty command uses the
/// environment.
pub fn set_command(cmd: &str) {
    if let Ok(mut c) = CONFIGURED.write() {
        *c = Some(cmd.trim().to_string());
    }
}

/// The editor command of the config file. The first call reads the config file;
/// an invalid file gives a warning on stderr and an empty command.
fn configured() -> String {
    if let Ok(c) = CONFIGURED.read()
        && let Some(cmd) = c.as_ref()
    {
        return cmd.clone();
    }
    let cmd = match config::default_path().map(|p| config::load(&p)) {
        Some(Ok(cfg)) => cfg.editor,
        Some(Err(e)) => {
            eprintln!("aq: warning: {e:#}");
            String::new()
        }
        None => String::new(),
    };
    set_command(&cmd);
    cmd
}

/// The editor command: `configured` if it is not empty, else `visual`, else
/// `editor`, else `vi`.
pub fn resolve(configured: &str, visual: Option<String>, editor: Option<String>) -> String {
    [Some(configured.to_string()), visual, editor]
        .into_iter()
        .flatten()
        .map(|e| e.trim().to_string())
        .find(|e| !e.is_empty())
        .unwrap_or_else(|| "vi".into())
}

/// The editor command from the config file and the environment.
pub fn command() -> String {
    resolve(
        &configured(),
        std::env::var("VISUAL").ok(),
        std::env::var("EDITOR").ok(),
    )
}

/// Opens `path` in the editor of the config file, else in `$VISUAL`, else in
/// `$EDITOR`, else in `vi`, and waits for the editor.
pub fn run(path: &Path) -> Result<()> {
    let editor = command();
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .with_context(|| format!("start the editor `{editor}`"))?;
    if !status.success() {
        bail!("the editor `{editor}` exited with {status}");
    }
    Ok(())
}

/// Edits `text` in the editor, in a temporary file with the extension `ext`.
pub fn edit_text(text: &str, ext: &str) -> Result<String> {
    let tmp = tempfile::Builder::new()
        .prefix("aq-")
        .suffix(ext)
        .tempfile()?;
    fs::write(tmp.path(), text)?;
    run(tmp.path())?;
    Ok(fs::read_to_string(tmp.path())?)
}

/// The result of one edit of `task.md` in the editor.
pub enum TaskEdit {
    Saved,
    Unchanged,
    Invalid { text: String, error: String },
}

/// Edits a copy of `task.md` in the editor. A valid result changes the content fields
/// of the task under the queue lock. The status and the claim come from the queue.
pub fn edit_task(q: &Queue, id: u32, start: Option<String>) -> Result<TaskEdit> {
    let cur = q.get(id)?;
    let original = cur.task.render();
    let before = start.unwrap_or_else(|| original.clone());
    let after = edit_text(&before, ".md")?;
    if after == original {
        return Ok(TaskEdit::Unchanged);
    }
    match Task::parse(&after) {
        Ok(mut edited) => {
            edited.meta.id = id;
            q.update_content(&edited)?;
            Ok(TaskEdit::Saved)
        }
        Err(e) => Ok(TaskEdit::Invalid {
            text: after,
            error: format!("{e:#}"),
        }),
    }
}

/// Edits a task from the CLI. On an invalid file, asks to edit again or to discard.
pub fn edit_task_cli(q: &Queue, id: u32) -> Result<()> {
    let mut start = None;
    loop {
        match edit_task(q, id, start.take())? {
            TaskEdit::Saved => {
                println!("saved task {id}");
                return Ok(());
            }
            TaskEdit::Unchanged => return Ok(()),
            TaskEdit::Invalid { text, error } => {
                eprintln!("the task is not valid: {error}");
                eprint!("[e]dit again or [d]iscard? ");
                io::stderr().flush()?;
                let mut answer = String::new();
                io::stdin().lock().read_line(&mut answer)?;
                if answer.trim().starts_with('d') {
                    println!("discarded");
                    return Ok(());
                }
                start = Some(text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_order() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(resolve("nvim", s("code -w"), s("nano")), "nvim");
        assert_eq!(resolve(" ", s("code -w"), s("nano")), "code -w");
        assert_eq!(resolve("", s(""), s("nano")), "nano");
        assert_eq!(resolve("", None, s(" ")), "vi");
    }
}
