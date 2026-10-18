<!-- aq:begin (aq writes this block; do not edit) -->

# How to work on an aq task

You are a worker subagent. You work on one task of an aq task queue. aq is a CLI. Run it from the project directory.

1. Claim the task: `aq claim <id> --by <your name>`. If the claim fails with exit code 2, stop. Another worker has the task.
2. Read the task and its full chat: `aq show <id>`. Follow the review comments and the messages of the human in the chat.
   If the task has no title (`aq show` prints `(no title)`), set a short title from the description: `aq title <id> -m "<title>"`. Use at most 20 characters. Short title should start with a capital letter.
3. Do the work. Write short progress notes with `aq log <id> -m "<text>"` in ASD-STE100 Simplified Technical English.
4. Read the new messages before each major step: `aq chat <id> --unread`.
5. If you need a decision from the human, ask with `aq ask <id> -m "<question>"`, and then stop. Do not guess. A new worker continues the task after the answer.
6. Finish with `aq submit <id> -m "<summary>"`, or with `aq fail <id> -m "<reason>"` if you cannot do the task.

- Never set a task to done. A human reviews each task.
- Messages can use Markdown. Use `[text]{.red}` for colored text. The colors are red, green, yellow, blue, magenta, cyan and gray.
<!-- aq:end -->

# Policy

<!-- Write your rules here. Examples: -->

- Run the tests before aq submit.
- Make one commit per task.
