//! The state of the TUI and the handling of keys.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tui_textarea::TextArea;

use super::desktop;
use super::form::{Field, Form, FormResult};
use super::textedit::{edit_key, new_area, paste, text_of};
use crate::chat::{self, HUMAN, Kind};
use crate::config::{self, Config};
use crate::queue::{Loaded, Queue, join_ids, waits_for_deps};
use crate::task::{Status, Task, parse_ids};
use crate::templates;

/// The smallest width of the task list.
pub const LIST_MIN: u16 = 12;
/// The smallest width of the right pane.
pub const RIGHT_MIN: u16 = 20;

/// Limit a width of the task list so that both panes stay usable in a main
/// area of width `total`.
pub fn clamp_list_width(w: u16, total: u16) -> u16 {
    let max = total.saturating_sub(RIGHT_MIN).max(LIST_MIN);
    w.clamp(LIST_MIN, max).min(total)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Details,
}

impl View {
    pub const ALL: [View; 2] = [View::Chat, View::Details];

    pub fn title(self) -> &'static str {
        match self {
            View::Chat => "Chat",
            View::Details => "Details",
        }
    }

    /// The other view.
    fn toggle(self) -> View {
        match self {
            View::Chat => View::Details,
            View::Details => View::Chat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    View,
    Input,
}

/// The set of tasks that the task list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// All tasks that are not done.
    #[default]
    Open,
    Review,
    Blocked,
    All,
    Done,
}

impl Filter {
    pub const CYCLE: [Filter; 5] = [
        Filter::Open,
        Filter::Review,
        Filter::Blocked,
        Filter::All,
        Filter::Done,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Filter::Open => "open",
            Filter::Review => "review",
            Filter::Blocked => "blocked",
            Filter::All => "all",
            Filter::Done => "done",
        }
    }

    /// The filter with the name `name`, as `name()` gives it.
    pub fn from_name(name: &str) -> Option<Filter> {
        Filter::CYCLE.into_iter().find(|f| f.name() == name)
    }

    fn matches(self, s: Status) -> bool {
        match self {
            Filter::Open => s != Status::Done,
            Filter::Review => s == Status::Review,
            Filter::Blocked => s == Status::Blocked,
            Filter::All => true,
            Filter::Done => s == Status::Done,
        }
    }

    fn next(self) -> Filter {
        let i = Filter::CYCLE.iter().position(|f| *f == self).unwrap_or(0);
        Filter::CYCLE[(i + 1) % Filter::CYCLE.len()]
    }

    fn prev(self) -> Filter {
        let n = Filter::CYCLE.len();
        let i = Filter::CYCLE.iter().position(|f| *f == self).unwrap_or(0);
        Filter::CYCLE[(i + n - 1) % n]
    }
}

/// A human status change that the status menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Start,
    Delete,
    Approve,
    Reject,
    Retry,
    Answer,
}

impl MenuAction {
    /// The key that runs the action while the menu is open.
    pub fn key(self) -> char {
        match self {
            MenuAction::Start => 's',
            MenuAction::Delete => 'd',
            MenuAction::Approve => 'a',
            MenuAction::Reject => 'r',
            MenuAction::Retry => 'r',
            MenuAction::Answer => 'a',
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            MenuAction::Start => "start",
            MenuAction::Delete => "delete",
            MenuAction::Approve => "approve",
            MenuAction::Reject => "reject with a comment",
            MenuAction::Retry => "retry",
            MenuAction::Answer => "answer in the chat",
        }
    }

    /// The result of the action: the new status, or the removal of the task.
    pub fn target(self) -> &'static str {
        match self {
            MenuAction::Delete => "remove",
            MenuAction::Approve => "→ done",
            _ => "→ todo",
        }
    }
}

pub enum Overlay {
    /// Key help; `scroll` is the first visible line.
    Help {
        scroll: u16,
    },
    Form(Box<Form>),
    Menu {
        id: u32,
        actions: Vec<MenuAction>,
        sel: usize,
    },
    Search {
        input: Box<TextArea<'static>>,
    },
    /// The one-line edit of the dependencies of task `id`.
    Deps {
        id: u32,
        input: Box<TextArea<'static>>,
    },
    InvalidEdit {
        id: u32,
        text: String,
        error: String,
    },
    ConfirmArchive,
    ConfirmDelete {
        id: u32,
    },
    Settings {
        sel: Setting,
        /// The one-line edit of the value of the selected setting.
        edit: Option<Box<TextArea<'static>>>,
    },
}

/// A row of the settings overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    ListWidth,
    ListFilter,
    Editor,
    Notifications,
}

impl Setting {
    pub const ALL: [Setting; 4] = [
        Setting::ListWidth,
        Setting::ListFilter,
        Setting::Editor,
        Setting::Notifications,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Setting::ListWidth => "List width",
            Setting::ListFilter => "List filter",
            Setting::Editor => "Editor",
            Setting::Notifications => "Notifications",
        }
    }

    fn step(self, d: isize) -> Setting {
        let i = Setting::ALL.iter().position(|s| *s == self).unwrap_or(0) as isize;
        let n = Setting::ALL.len() as isize;
        Setting::ALL[(i + d).clamp(0, n - 1) as usize]
    }
}

/// The screen areas of the last frame that take mouse clicks. An empty
/// `Rect` is an area that the frame does not show.
#[derive(Debug, Clone, Default)]
pub struct Hits {
    /// The rows of the task list, inside its border.
    pub list: Rect,
    /// The index into `visible` of the task in the first row of the list.
    pub list_top: usize,
    /// The tabs in the top border of the right pane.
    pub tabs: Vec<(Rect, View)>,
    /// The right pane, with its border.
    pub right: Rect,
    /// The chat input, with its border.
    pub input: Rect,
    /// The box of the overlay.
    pub overlay: Rect,
    /// The rows of the status menu or of the settings, one row for each item.
    pub rows: Rect,
    /// The text of the chat or of the details, which a mouse drag selects.
    pub text: Rect,
}

/// A text selection with the mouse, from the cell `anchor` to the cell `head`.
/// The selection includes both cells, and the rows between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSel {
    pub anchor: Position,
    pub head: Position,
}

impl TextSel {
    /// The first and the last cell of the selection, in reading order.
    fn ordered(self) -> (Position, Position) {
        let (a, b) = (self.anchor, self.head);
        if (a.y, a.x) <= (b.y, b.x) {
            (a, b)
        } else {
            (b, a)
        }
    }

    pub fn contains(self, p: Position) -> bool {
        let (a, b) = self.ordered();
        (a.y, a.x) <= (p.y, p.x) && (p.y, p.x) <= (b.y, b.x)
    }
}

/// The position in `r` that is nearest to `p`.
fn clamp_to(p: Position, r: Rect) -> Position {
    Position::new(
        p.x.min(r.right().saturating_sub(1)).max(r.x),
        p.y.min(r.bottom().saturating_sub(1)).max(r.y),
    )
}

/// The text of the cells `from..to` of a row of the chat. The borders of a
/// message box and the space after its left border are not text.
fn row_text(cells: &[String], from: usize, to: usize) -> String {
    let is_box = |s: &str| {
        s.chars()
            .next()
            .is_some_and(|c| ('\u{2500}'..='\u{257f}').contains(&c))
    };
    let mut keep = vec![true; cells.len()];
    let edge = matches!(
        cells.first().map(String::as_str),
        Some("┌" | "┏" | "└" | "┗")
    );
    if edge {
        for (k, c) in keep.iter_mut().zip(cells) {
            *k = !is_box(c);
        }
    } else if matches!(cells.first().map(String::as_str), Some("│" | "┃")) {
        keep[0] = false;
        if cells.get(1).is_some_and(|c| c == " ") {
            keep[1] = false;
        }
        if let Some(last) = cells.iter().rposition(|c| !c.trim().is_empty())
            && last > 0
            && is_box(&cells[last])
        {
            keep[last] = false;
        }
    }
    let text: String = (from..to.min(cells.len()))
        .filter(|&i| keep[i])
        .map(|i| cells[i].as_str())
        .collect();
    if edge {
        text.trim().to_string()
    } else {
        text.trim_end().to_string()
    }
}

/// Work that needs the terminal, and runs between two frames.
pub enum Job {
    EditTask { id: u32, start: Option<String> },
    EditFile(PathBuf),
    FormField(Box<Form>, Field),
}

pub struct App {
    pub q: Option<Queue>,
    pub project: PathBuf,
    pub tasks: Vec<Loaded>,
    pub bad: Vec<(PathBuf, String)>,
    /// The read marks of each task.
    pub marks: HashMap<u32, BTreeMap<String, usize>>,
    pub filter: Filter,
    /// Indices into `tasks` of the tasks that the filter shows.
    pub visible: Vec<usize>,
    pub sel: usize,
    pub view: View,
    pub focus: Focus,
    /// The selected chat entry. `None` follows the newest entry.
    pub chat_sel: Option<usize>,
    pub details_scroll: u16,
    pub input: TextArea<'static>,
    drafts: HashMap<u32, Vec<String>>,
    draft_id: Option<u32>,
    /// The task in review whose rejection the chat input writes. The next sent
    /// text becomes the review comment, and the task goes back to todo.
    pub reject: Option<u32>,
    pub overlay: Option<Overlay>,
    pub job: Option<Job>,
    pub message: Option<(String, bool, Instant)>,
    search: String,
    clipboard: Option<arboard::Clipboard>,
    pub page: u16,
    /// The user settings. `config.list_width` is the width of the task list;
    /// `None` uses the automatic width.
    pub config: Config,
    /// The config file. `None` keeps the settings in memory only.
    pub config_path: Option<PathBuf>,
    /// False if the config file was not valid at the last read. aq does not
    /// overwrite an invalid file.
    config_ok: bool,
    /// The area of the task list and the right pane, as drawn last.
    pub main_area: Rect,
    /// The drawn width of the task list.
    pub list_drawn: u16,
    /// The areas of the last frame that take mouse clicks.
    pub hits: Hits,
    /// True while the user drags the border of the task list.
    dragging: bool,
    /// The text selection in the chat or in the details.
    pub text_sel: Option<TextSel>,
    /// True while the user drags a text selection.
    selecting: bool,
    /// The symbols of the cells of `hits.text`, one row for each screen row, as
    /// drawn last. A cell under a wide character has an empty symbol.
    pub text_cells: Vec<Vec<String>>,
    /// The status of each task at the last reload. `None` before the first load.
    statuses: Option<HashMap<u32, Status>>,
    /// The tasks whose status the human changed in this TUI. The next reload
    /// sends no notification for them.
    own_changes: HashSet<u32>,
    /// The function that shows a desktop notification.
    pub notifier: fn(&str),
    pub quit: bool,
}

impl App {
    /// Makes the app. The settings come from the file `config_path`; `None` uses
    /// the defaults and saves nothing.
    pub fn new(q: Option<Queue>, project: PathBuf, config_path: Option<PathBuf>) -> App {
        let mut app = App {
            q,
            project,
            tasks: Vec::new(),
            bad: Vec::new(),
            marks: HashMap::new(),
            filter: Filter::Open,
            visible: Vec::new(),
            sel: 0,
            view: View::Chat,
            focus: Focus::List,
            chat_sel: None,
            details_scroll: 0,
            input: new_area(""),
            drafts: HashMap::new(),
            draft_id: None,
            reject: None,
            overlay: None,
            job: None,
            message: None,
            search: String::new(),
            clipboard: None,
            page: 10,
            config: Config::default(),
            config_path,
            config_ok: true,
            main_area: Rect::default(),
            list_drawn: 0,
            hits: Hits::default(),
            dragging: false,
            text_sel: None,
            selecting: false,
            text_cells: Vec::new(),
            statuses: None,
            own_changes: HashSet::new(),
            notifier: desktop::send,
            quit: false,
        };
        app.load_config();
        app.filter = app.config.list_filter;
        app.reload();
        app
    }

    /// Reads the config file. An invalid file keeps the current settings and
    /// shows an error.
    fn load_config(&mut self) {
        let Some(path) = &self.config_path else {
            return;
        };
        match config::load(path) {
            Ok(cfg) => {
                self.config = cfg;
                self.config_ok = true;
                crate::editor::set_command(&self.config.editor);
            }
            Err(e) => {
                self.config_ok = false;
                self.error(format!("{e:#}"));
            }
        }
    }

    /// Writes the settings to the config file.
    fn save_config(&mut self) {
        crate::editor::set_command(&self.config.editor);
        let Some(path) = self.config_path.clone() else {
            return;
        };
        if !self.config_ok {
            self.error(format!(
                "not saved: fix the config file {} first",
                path.display()
            ));
            return;
        }
        let r = config::save(&path, &self.config);
        if self.report(r).is_some() {
            self.info(format!("saved the settings to {}", path.display()));
        }
    }

    /// Sets the width of the task list and saves it.
    fn set_list_width(&mut self, w: Option<u16>) {
        self.config.list_width = w.map(|w| w.max(LIST_MIN));
        self.save_config();
    }

    pub fn info(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), false, Instant::now()));
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), true, Instant::now()));
    }

    fn report<T>(&mut self, r: Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.error(format!("{e:#}"));
                None
            }
        }
    }

    pub fn selected(&self) -> Option<&Loaded> {
        self.visible.get(self.sel).map(|i| &self.tasks[*i])
    }

    pub fn selected_id(&self) -> Option<u32> {
        self.selected().map(|l| l.task.id())
    }

    pub fn waits(&self, l: &Loaded) -> bool {
        waits_for_deps(l, &self.tasks)
    }

    pub fn unread(&self, l: &Loaded) -> usize {
        let empty = BTreeMap::new();
        let marks = self.marks.get(&l.task.id()).unwrap_or(&empty);
        chat::unread(&l.chat, marks, HUMAN).count()
    }

    /// Loads the queue again and keeps the selected task. If notifications are
    /// on, sends one notification for each task whose status changed since the
    /// last reload, except for the changes of the human in this TUI.
    pub fn reload(&mut self) {
        let keep = self.selected_id();
        let Some(q) = &self.q else { return };
        match q.load(true) {
            Ok((tasks, bad)) => {
                self.marks = tasks
                    .iter()
                    .map(|l| (l.task.id(), chat::read_marks(&l.marks_path())))
                    .collect();
                if let Some(before) = &self.statuses
                    && self.config.notifications
                {
                    for text in desktop::status_changes(before, &tasks, &self.own_changes) {
                        (self.notifier)(&text);
                    }
                }
                self.own_changes.clear();
                self.statuses = Some(desktop::statuses(&tasks));
                self.tasks = tasks;
                self.bad = bad;
            }
            Err(e) => {
                self.error(format!("{e:#}"));
                return;
            }
        }
        self.refilter(keep);
    }

    fn refilter(&mut self, keep: Option<u32>) {
        self.visible = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.archived && self.filter.matches(l.task.status()))
            .map(|(i, _)| i)
            .collect();
        self.sel = keep
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|i| self.tasks[*i].task.id() == id)
            })
            .unwrap_or_else(|| self.sel.min(self.visible.len().saturating_sub(1)));
        self.on_selection();
    }

    /// Updates the state that depends on the selected task.
    fn on_selection(&mut self) {
        let id = self.selected_id();
        if id != self.draft_id {
            if let Some(old) = self.draft_id {
                let text = self.input.lines().to_vec();
                if text.iter().all(|l| l.is_empty()) {
                    self.drafts.remove(&old);
                } else {
                    self.drafts.insert(old, text);
                }
            }
            let draft = id
                .and_then(|i| self.drafts.get(&i))
                .map(|l| l.join("\n"))
                .unwrap_or_default();
            self.input = new_area(&draft);
            self.draft_id = id;
            self.reject = None;
            self.chat_sel = None;
            self.details_scroll = 0;
        }
        self.mark_read();
    }

    /// Sets the read mark of the human for the selected task, if the chat shows it.
    fn mark_read(&mut self) {
        if self.view != View::Chat {
            return;
        }
        let (Some(q), Some(l)) = (&self.q, self.selected()) else {
            return;
        };
        let id = l.task.id();
        let n = l.chat.len();
        if self.marks.get(&id).and_then(|m| m.get(HUMAN)) == Some(&n) {
            return;
        }
        let path = l.marks_path();
        let r = q
            .lock()
            .and_then(|_lock| chat::set_read_mark(&path, HUMAN, n));
        if self.report(r).is_some() {
            self.marks.entry(id).or_default().insert(HUMAN.into(), n);
        }
    }

    fn select(&mut self, i: usize) {
        self.sel = i.min(self.visible.len().saturating_sub(1));
        self.on_selection();
    }

    fn set_view(&mut self, v: View) {
        self.view = v;
        self.mark_read();
    }

    /// Handles the mouse. A drag of the right border of the task list with the
    /// left button changes the width of the list. A left click selects a task,
    /// a tab, a pane, the chat input or a row of an overlay. The wheel scrolls
    /// the pane under the pointer.
    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.q.is_none() {
            return;
        }
        if let Some(ov) = self.overlay.take() {
            self.overlay = self.on_overlay_mouse(ov, m);
            return;
        }
        let main = self.main_area;
        let border = main.x + self.list_drawn.saturating_sub(1);
        let inside = m.row >= main.y && m.row < main.y + main.height;
        let pos = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.dragging = inside && m.column.abs_diff(border) <= 1;
                self.text_sel = None;
                if !self.dragging {
                    self.on_click(pos);
                    if self.hits.text.contains(pos) {
                        self.text_sel = Some(TextSel {
                            anchor: pos,
                            head: pos,
                        });
                        self.selecting = true;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let w = (m.column + 1).saturating_sub(main.x);
                self.config.list_width = Some(clamp_list_width(w, main.width));
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => {
                let head = clamp_to(pos, self.hits.text);
                if let Some(sel) = &mut self.text_sel {
                    sel.head = head;
                }
            }
            MouseEventKind::Up(MouseButton::Left) if std::mem::take(&mut self.dragging) => {
                self.save_config();
            }
            MouseEventKind::Up(MouseButton::Left) if std::mem::take(&mut self.selecting) => {
                self.copy_selection();
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                self.text_sel = None;
                let d = if m.kind == MouseEventKind::ScrollDown {
                    1
                } else {
                    -1
                };
                if self.hits.list.contains(pos) {
                    self.move_in(Focus::List, d);
                } else if self.hits.right.contains(pos) {
                    let d = if self.view == View::Details { d * 3 } else { d };
                    self.move_in(Focus::View, d);
                }
            }
            _ => {}
        }
    }

    /// The text of the selection, without the borders of the message boxes.
    pub fn selection_text(&self) -> Option<String> {
        let (a, b) = self.text_sel?.ordered();
        let area = self.hits.text;
        let mut lines = Vec::new();
        for (i, cells) in self.text_cells.iter().enumerate() {
            let y = area.y + i as u16;
            if y < a.y || y > b.y {
                continue;
            }
            let from = if y == a.y {
                a.x.saturating_sub(area.x) as usize
            } else {
                0
            };
            let to = if y == b.y {
                b.x.saturating_sub(area.x) as usize + 1
            } else {
                cells.len()
            };
            lines.push(row_text(cells, from, to));
        }
        Some(lines.join("\n"))
    }

    /// Copies the selected text to the clipboard. A selection of one cell is a
    /// click and selects nothing.
    fn copy_selection(&mut self) {
        let Some(sel) = self.text_sel else { return };
        if sel.anchor == sel.head {
            self.text_sel = None;
            return;
        }
        let text = self.selection_text().unwrap_or_default();
        if self.clipboard_set(&text) {
            self.info(format!("copied {} characters", text.chars().count()));
        }
    }

    /// A left click at `pos` without an overlay.
    fn on_click(&mut self, pos: Position) {
        let hits = &self.hits;
        if hits.list.contains(pos) {
            let i = hits.list_top + (pos.y - hits.list.y) as usize;
            self.leave_input();
            self.focus = Focus::List;
            if i < self.visible.len() {
                self.select(i);
            }
        } else if let Some((_, v)) = hits.tabs.iter().find(|(r, _)| r.contains(pos)) {
            let v = *v;
            self.leave_input();
            self.set_view(v);
            self.focus = Focus::View;
        } else if hits.input.contains(pos) {
            self.focus = Focus::Input;
        } else if hits.right.contains(pos) {
            self.leave_input();
            self.focus = Focus::View;
        }
    }

    /// Moves the focus from the chat input to the view and cancels a rejection.
    fn leave_input(&mut self) {
        if self.focus != Focus::Input {
            return;
        }
        if self.reject.take().is_some() {
            self.info("cancelled the rejection");
        }
        self.focus = Focus::View;
    }

    /// Handles the mouse while the overlay `ov` is open, and gives the overlay
    /// that stays open. A left click outside the box closes the help, the
    /// confirmations, the search, the status menu and the settings.
    fn on_overlay_mouse(&mut self, ov: Overlay, m: MouseEvent) -> Option<Overlay> {
        let pos = Position::new(m.column, m.row);
        let row = self
            .hits
            .rows
            .contains(pos)
            .then(|| (pos.y - self.hits.rows.y) as usize);
        let outside = !self.hits.overlay.contains(pos);
        let click = m.kind == MouseEventKind::Down(MouseButton::Left);
        match ov {
            Overlay::Help { scroll } => match m.kind {
                MouseEventKind::ScrollDown => Some(Overlay::Help {
                    scroll: scroll.saturating_add(3),
                }),
                MouseEventKind::ScrollUp => Some(Overlay::Help {
                    scroll: scroll.saturating_sub(3),
                }),
                _ if click && outside => None,
                _ => Some(Overlay::Help { scroll }),
            },
            Overlay::ConfirmArchive
            | Overlay::ConfirmDelete { .. }
            | Overlay::Search { .. }
            | Overlay::Deps { .. }
                if click && outside =>
            {
                None
            }
            Overlay::Menu { id, actions, sel } if click => match row {
                Some(i) if i == sel => {
                    self.run_menu(id, actions[i]);
                    self.overlay.take()
                }
                Some(i) if i < actions.len() => Some(Overlay::Menu {
                    id,
                    actions,
                    sel: i,
                }),
                _ if outside => None,
                _ => Some(Overlay::Menu { id, actions, sel }),
            },
            Overlay::Settings { sel, edit } if click => {
                match (row.and_then(|i| Setting::ALL.get(i)), edit) {
                    (Some(s), None) if *s == sel => {
                        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                        self.on_settings_key(sel, None, enter)
                    }
                    (Some(s), None) => Some(Overlay::Settings {
                        sel: *s,
                        edit: None,
                    }),
                    _ if outside => None,
                    (_, edit) => Some(Overlay::Settings { sel, edit }),
                }
            }
            ov => Some(ov),
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        match &mut self.overlay {
            Some(Overlay::Form(f)) => f.on_paste(text),
            Some(Overlay::Search { input })
            | Some(Overlay::Deps { input, .. })
            | Some(Overlay::Settings {
                edit: Some(input), ..
            }) => paste(input, text, false),
            Some(_) => {}
            None => {
                if self.focus != Focus::Input && self.selected().is_some() {
                    self.view = View::Chat;
                    self.focus = Focus::Input;
                }
                if self.focus == Focus::Input {
                    paste(&mut self.input, text, true);
                }
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.q.is_none() {
            match key.code {
                KeyCode::Char('i') => {
                    let r = Queue::init(&self.project);
                    if let Some(q) = self.report(r) {
                        self.q = Some(q);
                        self.reload();
                        self.info("initialized .aq/");
                    }
                }
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                _ => {}
            }
            return;
        }
        self.text_sel = None;
        if key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.overlay.is_none()
            && self.focus != Focus::Input
        {
            self.quit = true;
            return;
        }
        if let Some(ov) = self.overlay.take() {
            self.overlay = self.on_overlay_key(ov, key);
            return;
        }
        if self.focus == Focus::Input {
            self.on_input_key(key);
            return;
        }
        self.on_nav_key(key);
    }

    fn on_input_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.leave_input(),
            KeyCode::Enter if key.modifiers.is_empty() => match self.reject {
                Some(id) => self.send_reject(id),
                None => self.send(),
            },
            KeyCode::Char('v') if ctrl => {
                if let Some(text) = self.clipboard_get() {
                    paste(&mut self.input, &text, true);
                }
            }
            _ => {
                edit_key(&mut self.input, key, true);
            }
        }
    }

    fn send(&mut self) {
        let text = text_of(&self.input).trim().to_string();
        if text.is_empty() {
            return;
        }
        let (Some(q), Some(id)) = (&self.q, self.selected_id()) else {
            return;
        };
        let r = q.say(id, HUMAN, Kind::Message, &text);
        self.own_changes.insert(id);
        if let Some(l) = self.report(r) {
            self.input = new_area("");
            self.drafts.remove(&id);
            self.chat_sel = None;
            if l.task.status() == Status::Todo
                && l.chat.len() >= 2
                && l.chat[l.chat.len() - 2].kind == Kind::Question
            {
                self.info(format!("answered; task {id} is todo again"));
            }
            self.reload();
        }
    }

    /// Rejects task `id` with the text of the chat input as the review comment.
    fn send_reject(&mut self, id: u32) {
        let text = text_of(&self.input).trim().to_string();
        if text.is_empty() {
            self.error("the review comment is empty");
            return;
        }
        let Some(q) = &self.q else { return };
        let r = q.reject(id, &text);
        self.own_changes.insert(id);
        if self.report(r).is_some() {
            self.input = new_area("");
            self.drafts.remove(&id);
            self.reject = None;
            self.focus = Focus::View;
            self.chat_sel = None;
            self.info(format!("rejected; task {id} is todo again"));
            self.reload();
        }
    }

    fn on_nav_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = (self.page / 2).max(1) as isize;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('d') if ctrl => self.move_by(page),
            KeyCode::Char('u') if ctrl => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page * 2),
            KeyCode::PageUp => self.move_by(-page * 2),
            KeyCode::Char('g') | KeyCode::Home => self.move_by(-100_000),
            KeyCode::Char('G') | KeyCode::End => self.move_by(100_000),
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::List,
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::View,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::List {
                    Focus::View
                } else {
                    Focus::List
                }
            }
            KeyCode::Char('1') => self.set_view(View::Chat),
            KeyCode::Char('2') => self.set_view(View::Details),
            KeyCode::Char('[' | ']') => self.set_view(self.view.toggle()),
            KeyCode::Char('/') => {
                self.overlay = Some(Overlay::Search {
                    input: Box::new(new_area(&self.search)),
                })
            }
            KeyCode::Char('n') => self.search_step(1, false),
            KeyCode::Char('N') => self.search_step(-1, false),
            KeyCode::Char('i') if self.selected().is_some() => {
                self.set_view(View::Chat);
                self.focus = Focus::Input;
            }
            KeyCode::Enter => self.on_enter(),
            KeyCode::Char('c') => self.overlay = Some(Overlay::Form(Box::new(Form::new()))),
            KeyCode::Char('e') => {
                if let Some(l) = self.selected() {
                    self.overlay = Some(Overlay::Form(Box::new(Form::edit(&l.task))));
                }
            }
            KeyCode::Char('E') => {
                if let Some(id) = self.selected_id() {
                    self.job = Some(Job::EditTask { id, start: None });
                }
            }
            KeyCode::Char('D') => {
                if let Some(l) = self.selected() {
                    let input = Box::new(new_area(&join_ids(&l.task.meta.depends_on)));
                    self.overlay = Some(Overlay::Deps {
                        id: l.task.id(),
                        input,
                    });
                }
            }
            KeyCode::Char('s') => self.open_menu(),
            KeyCode::Char('f') => {
                self.filter = self.filter.next();
                let keep = self.selected_id();
                self.refilter(keep);
            }
            KeyCode::Char('x') => {
                if self
                    .tasks
                    .iter()
                    .any(|l| !l.archived && l.task.status() == Status::Done)
                {
                    self.overlay = Some(Overlay::ConfirmArchive);
                } else {
                    self.info("no done tasks to archive");
                }
            }
            KeyCode::Char('p') => {
                let text = templates::orchestrator_start();
                if self.clipboard_set(&text) {
                    self.info("copied the start text for the orchestrator");
                }
            }
            KeyCode::Char('O') => {
                if let Some(q) = &self.q {
                    self.job = Some(Job::EditFile(q.orchestrator_md()));
                }
            }
            KeyCode::Char('W') => {
                if let Some(q) = &self.q {
                    self.job = Some(Job::EditFile(q.worker_md()));
                }
            }
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help { scroll: 0 }),
            KeyCode::Char(',') => {
                self.load_config();
                self.overlay = Some(Overlay::Settings {
                    sel: Setting::ListWidth,
                    edit: None,
                });
            }
            KeyCode::Char('R') | KeyCode::F(5) => {
                self.reload();
                self.info("reloaded");
            }
            _ => {}
        }
    }

    fn move_by(&mut self, d: isize) {
        self.move_in(self.focus, d);
    }

    /// Moves the selection of the list (`Focus::List`) or of the view by `d`.
    fn move_in(&mut self, target: Focus, d: isize) {
        let clamp = |cur: usize, n: usize| -> usize {
            if n == 0 {
                0
            } else {
                (cur as isize + d).clamp(0, n as isize - 1) as usize
            }
        };
        if target == Focus::List {
            let i = clamp(self.sel, self.visible.len());
            self.select(i);
            return;
        }
        match self.view {
            View::Chat => {
                let n = self.selected().map_or(0, |l| l.chat.len());
                let cur = self.chat_sel.unwrap_or(n.saturating_sub(1));
                let next = clamp(cur, n);
                self.chat_sel = if next + 1 >= n && d > 0 {
                    None
                } else {
                    Some(next)
                };
            }
            View::Details => {
                self.details_scroll = (self.details_scroll as isize + d).max(0) as u16;
            }
        }
    }

    fn on_enter(&mut self) {
        if self.focus == Focus::List {
            self.focus = Focus::View;
            return;
        }
        match self.view {
            View::Chat => {
                let Some(l) = self.selected() else { return };
                let Some(i) = self.chat_sel.or(l.chat.len().checked_sub(1)) else {
                    return;
                };
                let rendered = crate::markdown::render(&l.chat[i].text, Default::default());
                match rendered.links.first() {
                    Some(url) => {
                        let r = open::that_detached(url).map_err(anyhow::Error::from);
                        if self.report(r).is_some() {
                            self.info(format!("opened {url}"));
                        }
                    }
                    None => self.info("the message has no link"),
                }
            }
            View::Details => {}
        }
    }

    /// Runs a status change of task `id` and reloads the queue.
    fn change_status(&mut self, id: u32, f: impl FnOnce(&Queue, u32) -> Result<Loaded>) {
        let Some(q) = &self.q else { return };
        let r = f(q, id);
        self.own_changes.insert(id);
        if self.report(r).is_some() {
            self.reload();
        }
    }

    fn open_menu(&mut self) {
        let Some(l) = self.selected() else { return };
        let mut actions = Vec::new();
        match l.task.status() {
            Status::Review => {
                actions.push(MenuAction::Approve);
                actions.push(MenuAction::Reject);
            }
            Status::InProgress | Status::Failed => actions.push(MenuAction::Retry),
            Status::Blocked => actions.push(MenuAction::Answer),
            Status::Draft => {
                actions.push(MenuAction::Start);
                actions.push(MenuAction::Delete);
            }
            Status::Todo | Status::Done => {}
        }
        if actions.is_empty() {
            let st = l.task.status();
            self.info(format!("no human status change for a task in {st}"));
            return;
        }
        self.overlay = Some(Overlay::Menu {
            id: l.task.id(),
            actions,
            sel: 0,
        });
    }

    fn run_menu(&mut self, id: u32, action: MenuAction) {
        match action {
            MenuAction::Start => self.change_status(id, Queue::start),
            MenuAction::Delete => self.overlay = Some(Overlay::ConfirmDelete { id }),
            MenuAction::Approve => self.change_status(id, Queue::approve),
            MenuAction::Reject => {
                self.set_view(View::Chat);
                self.focus = Focus::Input;
                self.reject = Some(id);
            }
            MenuAction::Retry => self.change_status(id, Queue::retry),
            MenuAction::Answer => {
                self.set_view(View::Chat);
                self.focus = Focus::Input;
            }
        }
    }

    /// Selects the next task in direction `d` that matches the search. With
    /// `inclusive`, the selected task is the first candidate.
    fn search_step(&mut self, d: isize, inclusive: bool) {
        if self.search.is_empty() || self.visible.is_empty() {
            return;
        }
        let needle = self.search.to_lowercase();
        let n = self.visible.len() as isize;
        let first = if inclusive { 0 } else { 1 };
        for step in first..first + n {
            let i = (self.sel as isize + d * step).rem_euclid(n) as usize;
            let l = &self.tasks[self.visible[i]];
            let hay = format!(
                "{} {} {} {}",
                l.task.id(),
                l.task.display_title(),
                l.task.description,
                l.task.acceptance
            )
            .to_lowercase();
            if hay.contains(&needle)
                || l.chat
                    .iter()
                    .any(|e| e.text.to_lowercase().contains(&needle))
            {
                self.focus = Focus::List;
                self.select(i);
                return;
            }
        }
        let s = self.search.clone();
        self.info(format!("no task matches `{s}`"));
    }

    fn on_overlay_key(&mut self, ov: Overlay, key: KeyEvent) -> Option<Overlay> {
        match ov {
            Overlay::Help { scroll } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter => None,
                KeyCode::Char('j') | KeyCode::Down => Some(Overlay::Help {
                    scroll: scroll.saturating_add(1),
                }),
                KeyCode::Char('k') | KeyCode::Up => Some(Overlay::Help {
                    scroll: scroll.saturating_sub(1),
                }),
                _ => Some(Overlay::Help { scroll }),
            },
            Overlay::ConfirmArchive => {
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter)
                    && let Some(q) = &self.q
                {
                    let r = q.archive();
                    if let Some(ids) = self.report(r) {
                        self.info(format!("archived {} task(s)", ids.len()));
                        self.reload();
                    }
                }
                None
            }
            Overlay::ConfirmDelete { id } => {
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter)
                    && let Some(q) = &self.q
                {
                    let r = q.delete(id);
                    self.own_changes.insert(id);
                    if self.report(r).is_some() {
                        self.info(format!("deleted task {id}"));
                        self.reload();
                    }
                }
                None
            }
            Overlay::Menu { id, actions, sel } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => None,
                KeyCode::Char('j') | KeyCode::Down => Some(Overlay::Menu {
                    id,
                    sel: (sel + 1).min(actions.len() - 1),
                    actions,
                }),
                KeyCode::Char('k') | KeyCode::Up => Some(Overlay::Menu {
                    id,
                    sel: sel.saturating_sub(1),
                    actions,
                }),
                KeyCode::Enter => {
                    self.run_menu(id, actions[sel]);
                    self.overlay.take()
                }
                KeyCode::Char(c) if actions.iter().any(|a| a.key() == c) => {
                    let a = actions.into_iter().find(|a| a.key() == c).unwrap();
                    self.run_menu(id, a);
                    self.overlay.take()
                }
                _ => Some(Overlay::Menu { id, actions, sel }),
            },
            Overlay::Search { mut input } => match key.code {
                KeyCode::Esc => None,
                KeyCode::Enter => {
                    self.search = text_of(&input).trim().to_string();
                    self.search_step(1, true);
                    None
                }
                _ => {
                    edit_key(&mut input, key, false);
                    Some(Overlay::Search { input })
                }
            },
            Overlay::Deps { id, mut input } => match key.code {
                KeyCode::Esc => None,
                KeyCode::Enter => match self.save_deps(id, &text_of(&input)) {
                    Ok(()) => None,
                    Err(e) => {
                        self.error(e);
                        Some(Overlay::Deps { id, input })
                    }
                },
                _ => {
                    edit_key(&mut input, key, false);
                    Some(Overlay::Deps { id, input })
                }
            },
            Overlay::InvalidEdit { id, text, error } => match key.code {
                KeyCode::Char('e') | KeyCode::Enter => {
                    self.job = Some(Job::EditTask {
                        id,
                        start: Some(text),
                    });
                    None
                }
                KeyCode::Char('d') | KeyCode::Esc => {
                    self.info("discarded the edit");
                    None
                }
                _ => Some(Overlay::InvalidEdit { id, text, error }),
            },
            Overlay::Settings { sel, edit } => self.on_settings_key(sel, edit, key),
            Overlay::Form(mut form) => match form.on_key(key) {
                FormResult::Continue => Some(Overlay::Form(form)),
                FormResult::Cancel => None,
                FormResult::External(field) => {
                    self.job = Some(Job::FormField(form, field));
                    None
                }
                FormResult::Save => match self.save_form(&form) {
                    Ok(()) => None,
                    Err(e) => {
                        form.error = Some(e);
                        Some(Overlay::Form(form))
                    }
                },
            },
        }
    }

    fn on_settings_key(
        &mut self,
        sel: Setting,
        edit: Option<Box<TextArea<'static>>>,
        key: KeyEvent,
    ) -> Option<Overlay> {
        if let Some(mut input) = edit {
            match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let text = text_of(&input).trim().to_string();
                    match sel {
                        Setting::ListWidth if text.is_empty() || text == "auto" => {
                            self.set_list_width(None)
                        }
                        Setting::ListWidth => match text.parse::<u16>() {
                            Ok(w) if w > 0 => self.set_list_width(Some(w)),
                            _ => {
                                self.error(format!("`{text}` is not a number of columns"));
                                return Some(Overlay::Settings {
                                    sel,
                                    edit: Some(input),
                                });
                            }
                        },
                        Setting::Editor => {
                            self.config.editor = text;
                            self.save_config();
                        }
                        Setting::ListFilter | Setting::Notifications => {}
                    }
                }
                _ => {
                    edit_key(&mut input, key, false);
                    return Some(Overlay::Settings {
                        sel,
                        edit: Some(input),
                    });
                }
            }
            return Some(Overlay::Settings { sel, edit: None });
        }
        let width = self.config.list_width.unwrap_or(self.list_drawn);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | ',') => return None,
            KeyCode::Char('j') | KeyCode::Down => {
                return Some(Overlay::Settings {
                    sel: sel.step(1),
                    edit: None,
                });
            }
            KeyCode::Char('k') | KeyCode::Up => {
                return Some(Overlay::Settings {
                    sel: sel.step(-1),
                    edit: None,
                });
            }
            KeyCode::Char('h') | KeyCode::Left if sel == Setting::ListWidth => {
                let w = width.saturating_sub(1);
                self.set_list_width(Some(self.clamp_width(w)));
            }
            KeyCode::Char('l') | KeyCode::Right if sel == Setting::ListWidth => {
                let w = width.saturating_add(1);
                self.set_list_width(Some(self.clamp_width(w)));
            }
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => match sel {
                Setting::ListWidth => self.set_list_width(None),
                Setting::Editor => {
                    self.config.editor.clear();
                    self.save_config();
                }
                Setting::ListFilter => {
                    self.config.list_filter = Filter::default();
                    self.save_config();
                }
                Setting::Notifications => {
                    self.config.notifications = false;
                    self.save_config();
                }
            },
            KeyCode::Char('h') | KeyCode::Left if sel == Setting::ListFilter => {
                self.config.list_filter = self.config.list_filter.prev();
                self.save_config();
            }
            KeyCode::Char('l' | ' ') | KeyCode::Right | KeyCode::Enter
                if sel == Setting::ListFilter =>
            {
                self.config.list_filter = self.config.list_filter.next();
                self.save_config();
            }
            KeyCode::Char('h' | 'l' | ' ') | KeyCode::Left | KeyCode::Right | KeyCode::Enter
                if sel == Setting::Notifications =>
            {
                self.config.notifications = !self.config.notifications;
                self.save_config();
            }
            KeyCode::Enter => {
                let value = match sel {
                    Setting::ListWidth => self
                        .config
                        .list_width
                        .map(|w| w.to_string())
                        .unwrap_or_default(),
                    Setting::Editor => self.config.editor.clone(),
                    Setting::ListFilter | Setting::Notifications => {
                        unreachable!("Enter cycles the filter and toggles the notifications")
                    }
                };
                return Some(Overlay::Settings {
                    sel,
                    edit: Some(Box::new(new_area(&value))),
                });
            }
            _ => {}
        }
        Some(Overlay::Settings { sel, edit: None })
    }

    /// Limits a width of the task list to the drawn main area.
    fn clamp_width(&self, w: u16) -> u16 {
        if self.main_area.width == 0 {
            w.max(LIST_MIN)
        } else {
            clamp_list_width(w, self.main_area.width)
        }
    }

    fn save_form(&mut self, form: &Form) -> Result<(), String> {
        let Some(q) = &self.q else { return Ok(()) };
        let mut probe = Task::new(0, "");
        form.apply(&mut probe)?;
        match form.id {
            None => {
                let l = q
                    .create(&probe.meta.title.clone(), |t| {
                        let _ = form.apply(t);
                    })
                    .map_err(|e| format!("{e:#}"))?;
                let id = l.task.id();
                self.info(format!("added task {id}"));
                self.reload();
                if let Some(i) = self
                    .visible
                    .iter()
                    .position(|i| self.tasks[*i].task.id() == id)
                {
                    self.select(i);
                }
            }
            Some(id) => {
                let mut task = q.get(id).map_err(|e| format!("{e:#}"))?.task;
                form.apply(&mut task)?;
                self.own_changes.insert(id);
                q.update_content(&task).map_err(|e| format!("{e:#}"))?;
                self.info(format!("saved task {id}"));
                self.reload();
            }
        }
        Ok(())
    }

    /// Sets the dependencies of task `id` from a list of ids and reloads the queue.
    fn save_deps(&mut self, id: u32, text: &str) -> Result<(), String> {
        let deps = parse_ids(text).map_err(|e| e.to_string())?;
        let Some(q) = self.q.clone() else {
            return Err("no queue".into());
        };
        let mut task = q.get(id).map_err(|e| format!("{e:#}"))?.task;
        task.meta.depends_on = deps;
        self.own_changes.insert(id);
        q.update_content(&task).map_err(|e| format!("{e:#}"))?;
        self.info(format!("saved the dependencies of task {id}"));
        self.reload();
        Ok(())
    }

    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(c) => self.clipboard = Some(c),
                Err(e) => {
                    self.error(format!("clipboard: {e}"));
                    return None;
                }
            }
        }
        self.clipboard.as_mut()
    }

    fn clipboard_set(&mut self, text: &str) -> bool {
        let r = self.clipboard().map(|c| c.set_text(text.to_string()));
        match r {
            Some(Ok(())) => true,
            Some(Err(e)) => {
                self.error(format!("clipboard: {e}"));
                false
            }
            None => false,
        }
    }

    fn clipboard_get(&mut self) -> Option<String> {
        let r = self.clipboard().map(|c| c.get_text());
        match r {
            Some(Ok(t)) => Some(t),
            Some(Err(e)) => {
                self.error(format!("clipboard: {e}"));
                None
            }
            None => None,
        }
    }

    /// Finishes a job after the editor exits.
    pub fn finish_job(&mut self, job: Job) {
        match job {
            Job::EditTask { id, start } => {
                let Some(q) = &self.q else { return };
                self.own_changes.insert(id);
                let r = crate::editor::edit_task(q, id, start);
                match self.report(r) {
                    Some(crate::editor::TaskEdit::Saved) => self.info(format!("saved task {id}")),
                    Some(crate::editor::TaskEdit::Unchanged) => {}
                    Some(crate::editor::TaskEdit::Invalid { text, error }) => {
                        self.overlay = Some(Overlay::InvalidEdit { id, text, error })
                    }
                    None => {}
                }
            }
            Job::EditFile(path) => {
                let r = crate::editor::run(&path);
                self.report(r);
            }
            Job::FormField(mut form, field) => {
                let text = form.text(field);
                let r = crate::editor::edit_text(&text, ".md");
                if let Some(t) = self.report(r) {
                    form.set_text(field, &t);
                }
                form.field = field;
                self.overlay = Some(Overlay::Form(form));
            }
        }
        self.reload();
    }
}
