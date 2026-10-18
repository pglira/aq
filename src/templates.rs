//! The templates of the instruction files and the start texts for the agents.

use std::path::Path;

pub const BEGIN: &str = "<!-- aq:begin (aq writes this block; do not edit) -->";
pub const END: &str = "<!-- aq:end -->";

const ORCHESTRATOR_BLOCK: &str = r#"# How to orchestrate the aq queue

You are the orchestrator of an aq task queue. aq is a CLI. Run it from the project directory.

- You do not work on tasks yourself. You dispatch each task to a worker subagent.
- Use `aq next --all --json` to find the tasks that are ready.
- A new task is in `draft`. A draft task is not ready. Only the human starts it (`draft` → `todo`). Never start or dispatch a draft task.
- Before you dispatch a task, read its unread human messages. `aq next` shows their count as `(N unread)`, and `--json` as `unread`. Read them with `aq chat <id> --unread --by orchestrator`.
- For each ready task, start a subagent. Give it the text of `aq instructions --worker <id>` as its full prompt. The text has all the context that the worker needs. The rules of the project are in the policy section of WORKER.md, not in the prompt.
- Choose the model for each worker: a cheaper model for a small task, a stronger model for a large or difficult task.
- Run the subagents in the background when your tool supports it, so that you can dispatch more tasks.
- Use `aq wait --consumer orchestrator` to wait for a change in the queue. It prints the changes: a new task, a deleted task, a status change (for example a start, an approval, a rejection or a task that left `in_progress`), a task that became ready, or a chat message of the human. aq keeps a cursor for the consumer, so no change is lost between two runs, and a run prints the earlier changes at once. Always use `--consumer orchestrator`.
- Run `aq wait --consumer orchestrator` in the background when your tool supports it, so that you stay free for the human. When it exits, act on the changes and run it again. If it times out (exit code 4), run it again. If your tool can stream the output lines of a background command (for example the Monitor tool of Claude Code), you can run `aq wait --follow --consumer orchestrator` instead: it prints one line per change and does not exit.
- The first `aq wait --consumer orchestrator` also registers your session for `aq hook`, if your tool sets `$CLAUDE_CODE_SESSION_ID`. If the project has the aq hooks (`aq setup-claude`), aq adds the changes to your context after tool calls and prompts, and does not let you stop while changes are unread. A change reaches you once: through the hook or through `aq wait`, not through both.
- Use `aq list` to see all tasks, and `aq show <id> --brief` to see one task with its last chat entries. Use `aq show <id>` for the full chat.
- A task can have no title. Then `aq list` shows the first line of the description and `(no title)`. The worker sets the title after the claim.
- Never set a task to done. A human reviews each task.
- Do not change the status of a task yourself. The workers and the human do that.
- Read the push policy in `.aq/config.toml` (key `push`). With `never`, or with no key, never push to a remote. With `after_approve`, push the branch of a task only after the human approves the task (`done`). Never push before that.
- `aq submit` runs the check command of `.aq/config.toml` (key `check`). If the check fails, the submit fails with exit code 2, and the worker must fix the problem.

## Answers of the human

A worker asks the human with `aq ask` and stops. The task is `blocked`. The answer of the human sets the task back to `todo`. If your agent tool can continue a finished subagent, continue the worker that asked: tell it to claim the task again and to read `aq chat <id> --unread`. Else start a new worker with `aq instructions --worker <id>`.

## Git worktrees

Decide for each task if its worker needs an isolated git worktree. Use a worktree when tasks that run at the same time can change the same files.

- Start the worker in its own git worktree, on its own branch. Use the worktree option of your agent tool if it has one.
- The queue is not in the worktree. The worker runs each aq command with `AQ_DIR` set to the queue of the project directory. The text of `aq instructions --worker <id>` gives the path.
- The worker commits its work on its branch. It does not merge the branch.
- After the human approves the task (status `done`), merge the branch into the main branch, and remove the worktree.

Exit codes of aq: 0 success, 1 usage error, 2 conflict (for example, the task has another status), 3 no queue found, 4 timeout of `aq wait`."#;

const ORCHESTRATOR_POLICY: &str = r#"# Policy

<!-- Write your rules here. Examples: -->
- Use at most 3 workers at the same time.
- Stop when no task is todo, in_progress, blocked or review.
"#;

const WORKER_BLOCK: &str = r#"# How to work on an aq task

You are a worker subagent. You work on one task of an aq task queue. aq is a CLI. Run it from the project directory.

1. Claim the task: `aq claim <id> --by <your name>`. If the claim fails with exit code 2, stop. Another worker has the task.
2. Read the task: `aq show <id> --brief`. It shows the fields, the description and the last chat entries. If it shows earlier entries, read the full chat with `aq show <id>`. Follow the review comments and the messages of the human in the chat.
   If the task has no title (`aq show` prints `(no title)`), set a short title from the description: `aq title <id> -m "<title>"`. Use at most 20 characters. Start the title with a capital letter.
3. Do the work. Write short progress notes with `aq log <id> -m "<text>"`.
4. Read the new messages before each major step: `aq chat <id> --unread`.
5. If you need a decision from the human, ask with `aq ask <id> -m "<question>"`, and then stop. Do not guess. After the answer, the orchestrator continues you or starts a new worker. If it continues you, claim the task again and read the answer with `aq chat <id> --unread`.
6. Finish with `aq submit <id> -m "<summary>"`, or with `aq fail <id> -m "<reason>"` if you cannot do the task.

- Never set a task to done. A human reviews each task.
- Keep the final reply to the orchestrator short: the status of the task and the commit hash, if you made a commit. Put the details in the aq chat.
- Messages can use Markdown. Use `[text]{.red}` for colored text. The colors are red, green, yellow, blue, magenta, cyan and gray."#;

const WORKER_POLICY: &str = r#"# Policy

<!-- Write your rules here. Examples: -->
- Run the tests before aq submit.
- Make one commit per task.
"#;

pub fn orchestrator_block() -> String {
    format!("{BEGIN}\n{ORCHESTRATOR_BLOCK}\n{END}")
}

pub fn worker_block() -> String {
    format!("{BEGIN}\n{WORKER_BLOCK}\n{END}")
}

pub fn orchestrator_template() -> String {
    format!("{}\n\n{ORCHESTRATOR_POLICY}", orchestrator_block())
}

pub fn worker_template() -> String {
    format!("{}\n\n{WORKER_POLICY}", worker_block())
}

/// Replaces the fixed block in `text`. If `text` has no block, the block goes to the top.
pub fn replace_block(text: &str, block: &str) -> String {
    if let (Some(b), Some(e)) = (text.find(BEGIN), text.find(END))
        && b < e
    {
        return format!("{}{block}{}", &text[..b], &text[e + END.len()..]);
    }
    if text.trim().is_empty() {
        format!("{block}\n")
    } else {
        format!("{block}\n\n{text}")
    }
}

pub fn orchestrator_start() -> String {
    "Read `.aq/ORCHESTRATOR.md` and follow it. Work the aq queue in this repository.".into()
}

pub fn worker_name(id: u32) -> String {
    format!("worker-{id}")
}

/// A task that is in progress, as listed in the start text of a worker.
pub struct Running {
    pub id: u32,
    pub title: String,
    pub assignee: String,
}

/// The start text of a worker: the instruction file, the queue for `AQ_DIR`,
/// and the other tasks in progress.
pub fn worker_start(id: u32, root: &Path, others: &[Running]) -> String {
    let mut text = format!(
        "You are {name}, a subagent of the aq orchestrator. Your task is {id}. \
         Read `{worker_md}` and follow it.\n\n\
         The aq queue is `{root}`. If you work in another directory, for example a git worktree, \
         run each aq command as `AQ_DIR={root} aq <args>`. In a git worktree, commit your work on \
         the branch of the worktree. Do not merge the branch.",
        name = worker_name(id),
        worker_md = root.join("WORKER.md").display(),
        root = root.display(),
    );
    if others.is_empty() {
        text.push_str("\n\nNo other task is in progress.");
    } else {
        text.push_str(
            "\n\nOther tasks in progress. Their workers can change the same files at the same time:\n",
        );
        for r in others {
            text.push_str(&format!("\n- {} {} ({})", r.id, r.title, r.assignee));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_keeps_policy() {
        let old = format!("{BEGIN}\nold\n{END}\n\n# Policy\nmine\n");
        let new = replace_block(&old, &worker_block());
        assert!(new.contains("How to work on an aq task"));
        assert!(new.ends_with("# Policy\nmine\n"));
        assert!(!new.contains("\nold\n"));
    }

    #[test]
    fn worker_start_lists_others() {
        let others = [Running {
            id: 3,
            title: "Fix cache".into(),
            assignee: "worker-3".into(),
        }];
        let text = worker_start(7, Path::new("/p/.aq"), &others);
        assert!(text.starts_with("You are worker-7,"));
        assert!(text.contains("`/p/.aq/WORKER.md`"));
        assert!(text.contains("AQ_DIR=/p/.aq aq <args>"));
        assert!(text.contains("- 3 Fix cache (worker-3)"));
        let alone = worker_start(7, Path::new("/p/.aq"), &[]);
        assert!(alone.contains("No other task is in progress."));
    }

    #[test]
    fn replace_without_block() {
        let new = replace_block("# Policy\nmine\n", "B");
        assert_eq!(new, "B\n\n# Policy\nmine\n");
    }
}
