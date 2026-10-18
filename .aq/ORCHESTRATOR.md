<!-- aq:begin (aq writes this block; do not edit) -->
# How to orchestrate the aq queue

You are the orchestrator of an aq task queue. aq is a CLI. Run it from the project directory.

- You do not work on tasks yourself. You dispatch each task to a worker subagent.
- Use `aq next --all --json` to find the tasks that are ready.
- A new task is in `draft`. A draft task is not ready. Only the human starts it (`draft` → `todo`). Never start or dispatch a draft task.
- For each ready task, start a subagent. Give it the text of `aq instructions --worker <id>` as its prompt.
- Run the subagents in the background when your tool supports it, so that you can dispatch more tasks.
- Use `aq wait` to wait for a change in the queue. It prints the changes: a task became ready, or a task left `in_progress`. If it times out (exit code 4), run it again.
- Use `aq list` to see all tasks, and `aq show <id>` to see one task with its chat.
- A task can have no title. Then `aq list` shows the first line of the description and `(no title)`. The worker sets the title after the claim.
- Never set a task to done. A human reviews each task.
- Do not change the status of a task yourself. The workers and the human do that.

Exit codes of aq: 0 success, 1 usage error, 2 conflict (for example, the task has another status), 3 no queue found, 4 timeout of `aq wait`.
<!-- aq:end -->

# Policy

<!-- Write your rules here. Examples: -->
- Use at most 3 workers at the same time.
- Stop when no task is todo, in_progress, blocked or review.
