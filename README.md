# aq (agent queue)

aq is a task queue for coding agents, with a TUI and a CLI. A human defines the tasks in the TUI. An orchestrator agent (for example a Claude Code session) dispatches each task to a worker subagent. The workers change the tasks only through the aq CLI. Each task has a chat, so that the human and the agents can talk about the task.

## Install

```bash
scripts/install.sh
```

The script builds aq in release mode and copies the binary to `$PREFIX/bin` (default: `~/.local/bin`). Options:

- `--no-build`: install the binary of the last release build.
- `--uninstall`: remove `$PREFIX/bin/aq`.

You can also use `cargo install --path .`, which installs aq to `~/.cargo/bin`.

## Workflow

1. Run `aq init` in the project root. aq makes `.aq/` with `ORCHESTRATOR.md` and `WORKER.md`.
2. Write your rules in the `# Policy` sections of `ORCHESTRATOR.md` and `WORKER.md` (TUI keys `O` and `W`).
3. Start the TUI with `aq` and add tasks (`c`). A new task is in `draft`. The agents do not claim it until you start it with the status menu (`s`) or with `aq start <id>`. The status menu can also delete a draft task.
4. Start the orchestrator in your agent tool with the text of `aq instructions` (TUI key `p` copies it).
5. Answer the questions of the workers in the chat of each task (`i`). An answer sets a `blocked` task back to `todo`.
6. Review the tasks in `review` with the status menu (`s`): approve the task, or reject it. To reject, write the review comment in the chat input and press `Enter`.

## Files

```
.aq/
  ORCHESTRATOR.md        instructions for the orchestrator
  WORKER.md              instructions for the workers
  tasks/0002-fix-cache/
    task.md              YAML frontmatter, ## Description, ## Acceptance
    chat.jsonl           the chat and the events, one JSON object per line
    .read                the read marks of each reader
  archive/               the done tasks after `aq archive`
```

aq searches the current directory and its parents for `.aq/`. Use `--dir` or `$AQ_DIR` to set the queue.

## Statuses

| Transition | Who | Command |
|---|---|---|
| `draft` → `todo` | human | `aq start <id>` |
| `draft` → deleted | human | `aq delete <id>` |
| `todo` → `in_progress` | agent | `aq claim <id> --by <name>` |
| `in_progress` → `review` | agent | `aq submit <id> -m <summary>` |
| `in_progress` → `blocked` | agent | `aq ask <id> -m <question>` |
| `in_progress` → `failed` | agent | `aq fail <id> -m <reason>` |
| `blocked` → `todo` | human | an answer in the chat: `aq say <id> -m <text>` |
| `review` → `done` | human | `aq approve <id>` |
| `review` → `todo` | human | `aq reject <id> [-m <comment>]` |
| `in_progress`, `failed` → `todo` | human | `aq retry <id>` |

`aq add` and the TUI make each new task in `draft`. Only a `draft` task can be deleted; `aq delete` removes its directory, and a new task does not get its id. A task is ready when it is `todo` and all its dependencies are `done`. `aq next --all` shows the ready tasks. `aq wait` waits until a task becomes ready or a task leaves `in_progress`.

Exit codes: `0` success, `1` usage error, `2` conflict, `3` no queue found, `4` timeout of `aq wait`.

## Chat formatting

Messages use Markdown: bold, italic, strikethrough, code, code blocks with syntax colors, lists, quotes and links. Colored text uses the bracketed span of Pandoc: `[text]{.red}`. The colors are `red`, `green`, `yellow`, `blue`, `magenta`, `cyan` and `gray`.

## TUI keys

Press `?` in the TUI for all keys. The navigation uses vim keys (`j`, `k`, `g`, `G`, `h`, `l`, `Ctrl+D`, `Ctrl+U`, `/`). The text fields work like normal text fields: `Enter` sends, `Alt+Enter` or `Shift+Enter` makes a new line, and `Esc` leaves the field. The text fields use the readline keys of a shell: `Ctrl+A` and `Ctrl+E` go to the start and the end of the line, `Ctrl+F` and `Ctrl+B` move by one character, `Alt+F` and `Alt+B` move by one word, and `Ctrl+P` and `Ctrl+N` move up and down. `Ctrl+U` and `Ctrl+K` delete to the start and the end of the line, `Ctrl+D` and `Ctrl+H` delete a character, and `Alt+D` and `Ctrl+W` delete a word. `Ctrl+Y` inserts the deleted text again. `Ctrl+Z` undoes a change. In the task form, `Ctrl+G` opens the description or the acceptance in `$EDITOR`. Drag the right border of the task list with the mouse to change the width of the list. aq saves the width in the settings (`,`). To select text with the mouse, hold `Shift` in most terminals.

## Settings

Press `,` in the TUI to open the settings. Each change saves at once to `$XDG_CONFIG_HOME/aq/config.toml`, or to `~/.config/aq/config.toml` if `$XDG_CONFIG_HOME` is not set. A missing file gives the defaults. If the file is not valid, the TUI shows an error in the footer, uses the defaults, and does not overwrite the file.

```toml
list_width = 40        # the width of the task list in columns; no value: automatic
list_filter = "review" # the filter of the task list at start: open, review, blocked, all, done; no value: open
editor = "nvim -p"     # the editor command; no value: $VISUAL, else $EDITOR, else vi
notifications = true   # desktop notifications of status changes; no value: off
```

In the settings, `j` and `k` select a setting. `h` and `l` change the list width, cycle the list filter, or switch the notifications on and off. `Space` and `Enter` cycle the list filter and switch the notifications. `Enter` edits the list width and the editor. `d` sets the default, and `Esc` closes. The list filter sets the filter of the task list when the TUI starts; `f` changes the filter while the TUI runs. A mouse drag of the list border also saves the list width. The editor setting applies to the TUI and to the CLI (`aq edit`, `aq add --edit`, `aq reject`). If the notifications are on, the TUI runs `notify-send` when an agent or another process changes the status of a task, for example `aq: #3 Fix cache → review`. The TUI sends no notification for the changes that you make in the TUI, and no notification at start. The CLI sends no notifications.

## Tests

```bash
cargo test
```
