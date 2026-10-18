//! The templates of the instruction files and the start texts for the agents.

pub const BEGIN: &str = "<!-- aq:begin (aq writes this block; do not edit) -->";
pub const END: &str = "<!-- aq:end -->";

const ORCHESTRATOR_BLOCK: &str = r#"# How to orchestrate the aq queue

You are the orchestrator of an aq task queue. aq is a CLI. Run it from the project directory.

- You do not work on tasks yourself. You dispatch each task to a worker subagent.
- Use `aq next --all --json` to find the tasks that are ready.
- A new task is in `draft`. A draft task is not ready. Only the human starts it (`draft` → `todo`). Never start or dispatch a draft task.
- For each ready task, start a subagent. Give it the text of `aq instructions --worker <id>` as its prompt.
- Run the subagents in the background when your tool supports it, so that you can dispatch more tasks.
- Use `aq wait` to wait for a change in the queue. It prints the changes: a new task, a deleted task, a status change (for example a start, an approval, a rejection or a task that left `in_progress`), a task that became ready, or a chat message of the human. If it times out (exit code 4), run it again. `aq wait --follow` prints one line per change and does not exit; use it for a background monitor.
- Use `aq list` to see all tasks, and `aq show <id>` to see one task with its chat.
- A task can have no title. Then `aq list` shows the first line of the description and `(no title)`. The worker sets the title after the claim.
- Never set a task to done. A human reviews each task.
- Do not change the status of a task yourself. The workers and the human do that.

Exit codes of aq: 0 success, 1 usage error, 2 conflict (for example, the task has another status), 3 no queue found, 4 timeout of `aq wait`."#;

const ORCHESTRATOR_POLICY: &str = r#"# Policy

<!-- Write your rules here. Examples: -->
- Use at most 3 workers at the same time.
- Stop when no task is todo, in_progress, blocked or review.
"#;

const WORKER_BLOCK: &str = r#"# How to work on an aq task

You are a worker subagent. You work on one task of an aq task queue. aq is a CLI. Run it from the project directory.

1. Claim the task: `aq claim <id> --by <your name>`. If the claim fails with exit code 2, stop. Another worker has the task.
2. Read the task and its full chat: `aq show <id>`. Follow the review comments and the messages of the human in the chat.
   If the task has no title (`aq show` prints `(no title)`), set a short title from the description: `aq title <id> -m "<title>"`. Use at most 20 characters. Start the title with a capital letter.
3. Do the work. Write short progress notes with `aq log <id> -m "<text>"`.
4. Read the new messages before each major step: `aq chat <id> --unread`.
5. If you need a decision from the human, ask with `aq ask <id> -m "<question>"`, and then stop. Do not guess. A new worker continues the task after the answer.
6. Finish with `aq submit <id> -m "<summary>"`, or with `aq fail <id> -m "<reason>"` if you cannot do the task.

- Never set a task to done. A human reviews each task.
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

pub fn worker_start(id: u32) -> String {
    format!(
        "You are {}, a subagent of the aq orchestrator. Read `.aq/WORKER.md` and follow it. Your task is {id}.",
        worker_name(id)
    )
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
    fn replace_without_block() {
        let new = replace_block("# Policy\nmine\n", "B");
        assert_eq!(new, "B\n\n# Policy\nmine\n");
    }
}
