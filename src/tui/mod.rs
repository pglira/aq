//! The terminal user interface.

mod app;
mod desktop;
mod form;
mod textedit;
mod ui;

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{RecursiveMode, Watcher};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};

use crate::queue::Queue;
use app::App;
pub(crate) use app::Filter;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// The interval of the poll that reloads the queue. File events do not arrive on
/// network mounts and on container volumes.
const POLL: Duration = Duration::from_secs(2);

pub fn run(q: Option<Queue>, project: PathBuf) -> Result<()> {
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    let mut term = setup(enhanced)?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore(enhanced);
        hook(info);
    }));
    let result = event_loop(
        &mut term,
        App::new(q, project, crate::config::default_path()),
        enhanced,
    );
    restore(enhanced)?;
    result
}

fn setup(enhanced: bool) -> Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    if enhanced {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore(enhanced: bool) -> Result<()> {
    let mut out = io::stdout();
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    execute!(
        out,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    Ok(())
}

fn watch(app: &App) -> (Option<notify::RecommendedWatcher>, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    let Some(q) = &app.q else { return (None, rx) };
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    });
    let Ok(mut w) = watcher else {
        return (None, rx);
    };
    let _ = w.watch(&q.root, RecursiveMode::Recursive);
    (Some(w), rx)
}

fn event_loop(term: &mut Term, mut app: App, enhanced: bool) -> Result<()> {
    let (mut _watcher, mut rx) = watch(&app);
    let mut last_reload = Instant::now();
    while !app.quit {
        term.draw(|f| ui::draw(f, &mut app))?;
        if let Some(job) = app.job.take() {
            restore(enhanced)?;
            app.finish_job(job);
            *term = setup(enhanced)?;
            term.clear()?;
            continue;
        }
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    let had = app.q.is_some();
                    app.on_key(k);
                    if !had && app.q.is_some() {
                        (_watcher, rx) = watch(&app);
                    }
                }
                Event::Paste(text) => app.on_paste(&text),
                Event::Mouse(m) => app.on_mouse(m),
                _ => {}
            }
        }
        let mut changed = false;
        while rx.try_recv().is_ok() {
            changed = true;
        }
        if changed || last_reload.elapsed() >= POLL {
            app.reload();
            last_reload = Instant::now();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::chat::{HUMAN, Kind};
    use crate::task::Status;

    fn screen(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
        t.draw(|f| ui::draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn typ(app: &mut App, text: &str) {
        for c in text.chars() {
            key(app, KeyCode::Char(c));
        }
    }

    fn demo() -> (tempfile::TempDir, Queue) {
        let d = tempfile::tempdir().unwrap();
        let q = Queue::init(d.path()).unwrap();
        q.create("Add login", |t| {
            t.description = "Add a **login** page.".into()
        })
        .unwrap();
        q.create("Fix cache", |t| t.meta.depends_on = vec![1])
            .unwrap();
        q.start(1).unwrap();
        q.start(2).unwrap();
        q.claim(1, "worker-1").unwrap();
        q.ask(1, "Use OAuth or [passwords]{.red}?").unwrap();
        (d, q)
    }

    #[test]
    fn chat_answer_and_views() {
        let (d, q) = demo();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        let s = screen(&mut app);
        assert!(s.contains("Add login"), "{s}");
        assert!(s.contains("todo (dep)"), "{s}");
        assert!(s.contains("Use OAuth or passwords?"), "{s}");
        assert!(s.contains("1-2 view"), "{s}");
        println!("{s}");

        key(&mut app, KeyCode::Char('i'));
        typ(&mut app, "OAuth please");
        key(&mut app, KeyCode::Enter);
        assert_eq!(q.get(1).unwrap().task.status(), Status::Todo);
        let s = screen(&mut app);
        assert!(s.contains("OAuth please"), "{s}");

        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('2'));
        let s = screen(&mut app);
        assert!(s.contains("Description"), "{s}");
        assert!(s.contains("Add a login page."), "{s}");

        key(&mut app, KeyCode::Char(']'));
        let s = screen(&mut app);
        assert!(s.contains("OAuth please"), "{s}");
        println!("{s}");

        key(&mut app, KeyCode::Char('3'));
        let s = screen(&mut app);
        assert!(s.contains("OAuth please"), "{s}");
    }

    #[test]
    fn view_tabs_in_border() {
        use ratatui::style::{Color, Modifier};
        let (d, q) = demo();
        let mut app = App::new(Some(q), d.path().to_path_buf(), None);
        let s = screen(&mut app);
        let top = s.lines().next().unwrap();
        assert!(top.contains("┐┌ 1 Chat │ 2 Details ─"), "{s}");
        assert!(s.lines().nth(1).unwrap().contains("│#1 Add login"), "{s}");
        let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
        let mut style_of = |app: &mut App, word: &str| {
            t.draw(|f| ui::draw(f, app)).unwrap();
            let buf = t.backend().buffer();
            let row: String = (0..buf.area.width).map(|x| buf[(x, 0)].symbol()).collect();
            let col = row[..row.find(word).unwrap()].chars().count() as u16;
            buf[(col, 0)].style()
        };
        let chat = style_of(&mut app, "Chat");
        assert_eq!(chat.fg, Some(Color::Cyan));
        assert!(chat.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style_of(&mut app, "Details").fg, Some(Color::DarkGray));
        key(&mut app, KeyCode::Char('2'));
        assert_eq!(style_of(&mut app, "Details").fg, Some(Color::Cyan));
        assert_eq!(style_of(&mut app, "Chat").fg, Some(Color::DarkGray));
    }

    #[test]
    fn chat_wraps_in_box() {
        let (d, q) = demo();
        q.log(1, &"word ".repeat(30)).unwrap();
        q.say(1, HUMAN, Kind::Message, "Use **OAuth**.").unwrap();
        q.claim(1, "worker-1").unwrap();
        q.submit(1, "Login page with OAuth.").unwrap();
        let mut app = App::new(Some(q), d.path().to_path_buf(), None);
        let s = screen(&mut app);
        let bars = s.lines().filter(|l| l.contains("│ word")).count();
        assert!(bars >= 3, "{s}");
        assert!(s.contains("· worker-1 claimed"), "{s}");
        assert!(s.contains("worker-1  submitted"), "{s}");
        assert!(s.contains("│ Login page with OAuth."), "{s}");
        assert!(s.contains("┌─ you"), "{s}");
        assert!(
            s.lines()
                .any(|l| l.contains("│ └─") && l.trim_end().ends_with("┘ │")),
            "{s}"
        );
        let right = |l: &str| l.contains("│ word") && l.trim_end().ends_with(" │");
        assert!(s.lines().filter(|l| right(l)).count() >= 3, "{s}");
    }

    #[test]
    fn form_creates_task() {
        let (d, q) = demo();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        key(&mut app, KeyCode::Char('c'));
        typ(&mut app, "Some *docs*");
        key(&mut app, KeyCode::BackTab);
        typ(&mut app, "Write docs");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Char('1'));
        key(&mut app, KeyCode::Tab);
        typ(&mut app, "1");
        let s = screen(&mut app);
        assert!(s.contains("new task"), "{s}");
        println!("{s}");
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.overlay.is_none());
        let t = q.get(3).unwrap().task;
        assert_eq!(t.meta.title, "Write docs");
        assert_eq!(t.meta.priority, 1);
        assert_eq!(t.meta.depends_on, vec![1]);
        assert_eq!(t.description, "Some *docs*");
    }

    #[test]
    fn form_creates_task_without_title() {
        let (d, q) = demo();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        key(&mut app, KeyCode::Char('c'));
        typ(&mut app, "Only a description");
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.overlay.is_none());
        let l = q.get(3).unwrap();
        assert!(!l.task.has_title());
        assert_eq!(l.dir_name(), "0003-only-a-description");
        let s = screen(&mut app);
        assert!(s.contains("Only a description"), "{s}");
    }

    #[test]
    fn drag_list_border() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let (d, q) = demo();
        let mut app = App::new(Some(q), d.path().to_path_buf(), None);
        screen(&mut app);
        let border = app.list_drawn - 1;
        let mouse = |kind, column| MouseEvent {
            kind,
            column,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), border));
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 59));
        app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 59));
        screen(&mut app);
        assert_eq!(app.list_drawn, 60);
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 20));
        assert_eq!(app.config.list_width, Some(60));
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 59));
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 200));
        screen(&mut app);
        assert_eq!(app.list_drawn, 110 - 20);
    }

    fn click(app: &mut App, column: u16, row: u16) {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.on_mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            });
        }
    }

    fn wheel(app: &mut App, down: bool, column: u16, row: u16) {
        use ratatui::crossterm::event::{MouseEvent, MouseEventKind};
        let kind = if down {
            MouseEventKind::ScrollDown
        } else {
            MouseEventKind::ScrollUp
        };
        app.on_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    /// The column and the row of the first cell of `text` on the screen.
    fn find(s: &str, text: &str) -> (u16, u16) {
        for (y, line) in s.lines().enumerate() {
            if let Some(i) = line.find(text) {
                return (line[..i].chars().count() as u16, y as u16);
            }
        }
        panic!("`{text}` is not on the screen:\n{s}");
    }

    #[test]
    fn mouse_drag_selects_chat_text() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let (d, q) = demo();
        let mut app = App::new(Some(q), d.path().to_path_buf(), None);
        let s = screen(&mut app);
        let (x, y) = find(&s, "Use OAuth");
        let mouse = |app: &mut App, kind, column, row| {
            app.on_mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        // From the author in the top border of the box to "OAuth" in the text.
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            x - 2,
            y - 1,
        );
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x + 8, y);
        assert_eq!(app.focus, app::Focus::View);
        let s = screen(&mut app);
        let text = app.selection_text().unwrap();
        let (head, body) = text.split_once('\n').unwrap();
        assert!(head.starts_with("worker-1  question  "), "{text}\n{s}");
        assert!(!head.contains(['─', '┐']), "{text}");
        assert_eq!(body, "Use OAuth");
        let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
        t.draw(|f| ui::draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer();
        assert!(
            buf[(x, y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            !buf[(x + 9, y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        // A drag beyond the chat stops at its border.
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 200, y);
        let text = app.selection_text().unwrap();
        assert!(text.ends_with("Use OAuth or passwords?"), "{text}");
        // A key clears the selection.
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.text_sel, None);
        // A click without a drag selects nothing.
        click(&mut app, x, y);
        assert_eq!(app.text_sel, None);
    }

    #[test]
    fn mouse_selects_task_tab_and_input() {
        let (d, q) = demo();
        let mut app = App::new(Some(q), d.path().to_path_buf(), None);
        let s = screen(&mut app);
        let (x, y) = find(&s, "Fix cache");
        click(&mut app, x, y);
        assert_eq!(app.selected_id(), Some(2));
        assert_eq!(app.focus, app::Focus::List);

        let s = screen(&mut app);
        let (x, y) = find(&s, "2 Details");
        click(&mut app, x + 3, y);
        assert_eq!(app.view, app::View::Details);
        assert_eq!(app.focus, app::Focus::View);
        let s = screen(&mut app);
        assert!(s.contains("Description"), "{s}");

        let (x, y) = find(&s, "1 Chat");
        click(&mut app, x, y);
        assert_eq!(app.view, app::View::Chat);

        key(&mut app, KeyCode::Char('h'));
        let s = screen(&mut app);
        let (x, y) = find(&s, " i: write ");
        click(&mut app, x + 2, y + 1);
        assert_eq!(app.focus, app::Focus::Input);
        typ(&mut app, "hi");
        assert_eq!(app.input.lines()[0], "hi");

        // A click in the chat above the input focuses the view.
        click(&mut app, x + 2, y - 3);
        assert_eq!(app.focus, app::Focus::View);
        // A click below the last task focuses the list and keeps the selection.
        click(&mut app, 3, 20);
        assert_eq!(app.focus, app::Focus::List);
        assert_eq!(app.selected_id(), Some(2));
    }

    #[test]
    fn mouse_wheel_scrolls_pane_under_pointer() {
        let (d, q) = demo();
        let mut app = App::new(Some(q), d.path().to_path_buf(), None);
        screen(&mut app);
        assert_eq!(app.selected_id(), Some(1));
        wheel(&mut app, true, 3, 5);
        assert_eq!(app.selected_id(), Some(2));
        wheel(&mut app, false, 3, 5);
        assert_eq!(app.selected_id(), Some(1));

        let right = app.list_drawn + 10;
        assert_eq!(app.chat_sel, None);
        wheel(&mut app, false, right, 5);
        assert_eq!(app.selected_id(), Some(1));
        assert!(app.chat_sel.is_some());
        wheel(&mut app, true, right, 5);
        assert_eq!(app.chat_sel, None);

        key(&mut app, KeyCode::Char('2'));
        screen(&mut app);
        wheel(&mut app, true, right, 5);
        assert_eq!(app.details_scroll, 3);
        wheel(&mut app, false, right, 5);
        assert_eq!(app.details_scroll, 0);
        assert_eq!(app.focus, app::Focus::List);
    }

    #[test]
    fn mouse_in_menu_and_settings() {
        let (d, q) = demo();
        q.create("Draft task", |_| {}).unwrap();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        key(&mut app, KeyCode::Char('G'));
        key(&mut app, KeyCode::Char('s'));
        let s = screen(&mut app);
        let (x, y) = find(&s, "d  delete");
        click(&mut app, x, y);
        assert!(matches!(
            app.overlay,
            Some(app::Overlay::Menu { sel: 1, .. })
        ));
        click(&mut app, x, y);
        assert!(matches!(
            app.overlay,
            Some(app::Overlay::ConfirmDelete { id: 3 })
        ));
        screen(&mut app);
        click(&mut app, 0, 0);
        assert!(app.overlay.is_none());
        assert!(q.get(3).is_ok());

        key(&mut app, KeyCode::Char('s'));
        let s = screen(&mut app);
        let (x, y) = find(&s, "s  start");
        click(&mut app, x, y);
        assert!(app.overlay.is_none());
        assert_eq!(q.get(3).unwrap().task.status(), Status::Todo);

        key(&mut app, KeyCode::Char(','));
        let s = screen(&mut app);
        let (x, y) = find(&s, "Notifications");
        assert!(!app.config.notifications);
        click(&mut app, x, y);
        assert!(matches!(
            app.overlay,
            Some(app::Overlay::Settings {
                sel: app::Setting::Notifications,
                ..
            })
        ));
        assert!(!app.config.notifications);
        click(&mut app, x, y);
        assert!(app.config.notifications);
        screen(&mut app);
        click(&mut app, 0, 0);
        assert!(app.overlay.is_none());
    }

    thread_local! {
        static SENT: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn record(text: &str) {
        SENT.with(|s| s.borrow_mut().push(text.to_string()));
    }

    fn sent() -> Vec<String> {
        SENT.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }

    #[test]
    fn notifies_status_changes_of_others_only() {
        let (d, q) = demo();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        app.notifier = record;
        app.config.notifications = true;
        app.reload();
        assert!(sent().is_empty());
        key(&mut app, KeyCode::Char('i'));
        typ(&mut app, "OAuth");
        key(&mut app, KeyCode::Enter);
        assert_eq!(q.get(1).unwrap().task.status(), Status::Todo);
        app.reload();
        assert!(sent().is_empty());
        q.claim(1, "worker-1").unwrap();
        app.reload();
        assert_eq!(sent(), vec!["aq: #1 Add login → in_progress"]);
        app.reload();
        assert!(sent().is_empty());
        app.config.notifications = false;
        q.submit(1, "done").unwrap();
        app.reload();
        assert!(sent().is_empty());
    }

    #[test]
    fn cancel_reject() {
        let (d, q) = demo();
        q.say(1, HUMAN, Kind::Message, "ok").unwrap();
        q.claim(1, "worker-1").unwrap();
        q.submit(1, "done").unwrap();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        key(&mut app, KeyCode::Char('s'));
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Enter);
        typ(&mut app, "Not this");
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.reject, None);
        key(&mut app, KeyCode::Char('i'));
        key(&mut app, KeyCode::Enter);
        let l = q.get(1).unwrap();
        assert_eq!(l.task.status(), Status::Review);
        assert_eq!(l.chat.last().unwrap().kind, Kind::Message);
    }

    #[test]
    fn menu_starts_draft() {
        let (d, q) = demo();
        q.create("Draft task", |_| {}).unwrap();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        app.filter = Filter::Open;
        let s = screen(&mut app);
        assert!(s.contains("Draft task"), "{s}");
        key(&mut app, KeyCode::Char('G'));
        key(&mut app, KeyCode::Char('s'));
        let s = screen(&mut app);
        assert!(s.contains("s  start"), "{s}");
        assert!(s.contains("d  delete"), "{s}");
        assert!(s.contains("→ todo"), "{s}");
        assert!(s.contains(" status of task 3 "), "{s}");
        key(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        let l = q.get(3).unwrap();
        assert_eq!(l.task.status(), Status::Todo);
        assert_eq!(l.chat.last().unwrap().text, "started");
    }

    #[test]
    fn menu_deletes_draft_after_confirmation() {
        let (d, q) = demo();
        q.create("Draft one", |_| {}).unwrap();
        q.create("Draft two", |_| {}).unwrap();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        app.filter = Filter::Open;
        app.reload();
        key(&mut app, KeyCode::Char('G'));
        key(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected_id(), Some(3));
        key(&mut app, KeyCode::Char('s'));
        key(&mut app, KeyCode::Char('j'));
        let s = screen(&mut app);
        assert!(s.contains("d  delete"), "{s}");
        key(&mut app, KeyCode::Enter);
        let s = screen(&mut app);
        assert!(s.contains("Delete the draft task 3?"), "{s}");
        key(&mut app, KeyCode::Char('n'));
        assert!(app.overlay.is_none());
        assert!(q.get(3).is_ok());

        key(&mut app, KeyCode::Char('s'));
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('y'));
        assert!(app.overlay.is_none());
        assert!(q.get(3).is_err());
        assert_eq!(app.selected_id(), Some(4));
    }

    #[test]
    fn menu_key_runs_action() {
        let (d, q) = demo();
        q.say(1, HUMAN, Kind::Message, "ok").unwrap();
        q.claim(1, "worker-1").unwrap();
        q.submit(1, "done").unwrap();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        key(&mut app, KeyCode::Char('s'));
        let s = screen(&mut app);
        assert!(s.contains("a  approve"), "{s}");
        assert!(s.contains("r  reject with a comment"), "{s}");
        assert!(s.contains("→ done"), "{s}");
        key(&mut app, KeyCode::Char('a'));
        assert!(app.overlay.is_none());
        assert_eq!(q.get(1).unwrap().task.status(), Status::Done);
    }

    #[test]
    fn reject_from_review() {
        let (d, q) = demo();
        q.say(1, HUMAN, Kind::Message, "ok").unwrap();
        q.claim(1, "worker-1").unwrap();
        q.submit(1, "done").unwrap();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        key(&mut app, KeyCode::Char('s'));
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        assert_eq!(app.reject, Some(1));
        typ(&mut app, "Add a test");
        let s = screen(&mut app);
        assert!(
            s.contains("review comment (Enter rejects, Esc cancels)"),
            "{s}"
        );
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.reject, None);
        let l = q.get(1).unwrap();
        assert_eq!(l.task.status(), Status::Todo);
        assert_eq!(l.chat.last().unwrap().kind, Kind::Review);
        key(&mut app, KeyCode::Char('?'));
        let s = screen(&mut app);
        assert!(s.contains("help"), "{s}");
    }

    #[test]
    fn settings_overlay_saves_at_once() {
        let (d, q) = demo();
        let cfg = d.path().join("xdg/aq/config.toml");
        let mut app = App::new(Some(q), d.path().to_path_buf(), Some(cfg.clone()));
        screen(&mut app);
        let auto = app.list_drawn;
        key(&mut app, KeyCode::Char(','));
        let s = screen(&mut app);
        assert!(s.contains("List width"), "{s}");
        assert!(s.contains(&format!("auto ({auto} columns)")), "{s}");
        key(&mut app, KeyCode::Char('l'));
        assert_eq!(app.config.list_width, Some(auto + 1));
        let saved = crate::config::load(&cfg).unwrap();
        assert_eq!(saved.list_width, Some(auto + 1));
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Char('l'));
        assert_eq!(
            crate::config::load(&cfg).unwrap().list_filter,
            Filter::Review
        );
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char(' '));
        assert_eq!(crate::config::load(&cfg).unwrap().list_filter, Filter::All);
        key(&mut app, KeyCode::Char('h'));
        assert_eq!(
            crate::config::load(&cfg).unwrap().list_filter,
            Filter::Blocked
        );
        let s = screen(&mut app);
        assert!(s.contains("List filter"), "{s}");
        assert!(s.contains("blocked"), "{s}");
        assert_eq!(app.filter, Filter::Open);
        key(&mut app, KeyCode::Char('d'));
        assert_eq!(crate::config::load(&cfg).unwrap().list_filter, Filter::Open);
        key(&mut app, KeyCode::Char('h'));
        assert_eq!(crate::config::load(&cfg).unwrap().list_filter, Filter::Done);
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Enter);
        typ(&mut app, "nvim -p");
        key(&mut app, KeyCode::Enter);
        let saved = crate::config::load(&cfg).unwrap();
        assert_eq!(saved.editor, "nvim -p");
        let s = screen(&mut app);
        assert!(s.contains("nvim -p"), "{s}");
        key(&mut app, KeyCode::Char('d'));
        assert_eq!(crate::config::load(&cfg).unwrap().editor, "");
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Enter);
        assert!(crate::config::load(&cfg).unwrap().notifications);
        key(&mut app, KeyCode::Char('d'));
        assert!(!crate::config::load(&cfg).unwrap().notifications);
        key(&mut app, KeyCode::Char('k'));
        key(&mut app, KeyCode::Char('k'));
        key(&mut app, KeyCode::Char('k'));
        key(&mut app, KeyCode::Enter);
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        typ(&mut app, "40");
        key(&mut app, KeyCode::Enter);
        assert_eq!(crate::config::load(&cfg).unwrap().list_width, Some(40));
        key(&mut app, KeyCode::Esc);
        assert!(app.overlay.is_none());
        screen(&mut app);
        assert_eq!(app.list_drawn, 40);

        let again = App::new(None, d.path().to_path_buf(), Some(cfg));
        assert_eq!(again.config.list_width, Some(40));
        assert_eq!(again.filter, Filter::Done);
    }

    #[test]
    fn list_filter_setting_sets_the_start_filter() {
        let (d, q) = demo();
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "list_filter = \"review\"\n").unwrap();
        let mut app = App::new(Some(q), d.path().to_path_buf(), Some(cfg.clone()));
        assert_eq!(app.filter, Filter::Review);
        assert!(
            app.visible
                .iter()
                .all(|i| app.tasks[*i].task.status() == Status::Review)
        );
        key(&mut app, KeyCode::Char(','));
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.filter, Filter::Review);

        std::fs::write(&cfg, "list_filter = \"todo\"\n").unwrap();
        let mut app = App::new(None, d.path().to_path_buf(), Some(cfg));
        assert_eq!(app.filter, Filter::Open);
        let s = screen(&mut app);
        assert!(s.contains("list_filter"), "{s}");
    }

    #[test]
    fn drag_saves_list_width() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let (d, q) = demo();
        let cfg = d.path().join("config.toml");
        let mut app = App::new(Some(q), d.path().to_path_buf(), Some(cfg.clone()));
        screen(&mut app);
        let border = app.list_drawn - 1;
        let mouse = |kind, column| MouseEvent {
            kind,
            column,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), border));
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 49));
        assert!(!cfg.exists());
        app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 49));
        assert_eq!(crate::config::load(&cfg).unwrap().list_width, Some(50));
    }

    #[test]
    fn invalid_config_shows_error_and_keeps_file() {
        let (d, q) = demo();
        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "list_width = [").unwrap();
        let mut app = App::new(Some(q), d.path().to_path_buf(), Some(cfg.clone()));
        assert_eq!(app.config, crate::config::Config::default());
        let s = screen(&mut app);
        assert!(s.contains("config file"), "{s}");
        key(&mut app, KeyCode::Char(','));
        key(&mut app, KeyCode::Char('l'));
        let s = screen(&mut app);
        assert!(s.contains("not saved"), "{s}");
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "list_width = [");
    }

    #[test]
    fn help_lines_do_not_wrap_and_scroll() {
        let (d, q) = demo();
        let mut app = App::new(Some(q), d.path().to_path_buf(), None);
        key(&mut app, KeyCode::Char('?'));
        let s = screen(&mut app);
        assert!(
            s.contains("drag the list border    change the list width"),
            "{s}"
        );
        assert!(
            s.contains("mouse wheel             scroll the list or the view under the pointer"),
            "{s}"
        );
        assert!(s.contains("Navigation"), "{s}");
        assert!(!s.contains("Colored text"), "{s}");
        for _ in 0..100 {
            key(&mut app, KeyCode::Char('j'));
        }
        let s = screen(&mut app);
        assert!(
            s.contains("leave the input; keep the draft; cancel a rejection"),
            "{s}"
        );
        assert!(
            s.contains("Chat messages use Markdown. Colored text: [text]{.red}"),
            "{s}"
        );
        assert!(!s.contains("Navigation"), "{s}");
        key(&mut app, KeyCode::Char('k'));
        assert!(matches!(app.overlay, Some(app::Overlay::Help { scroll }) if scroll > 0));
    }

    #[test]
    fn deps_key_sets_dependencies() {
        let (d, q) = demo();
        let mut app = App::new(Some(q.clone()), d.path().to_path_buf(), None);
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Char('D'));
        assert!(matches!(
            app.overlay,
            Some(app::Overlay::Deps { id: 2, .. })
        ));
        let s = screen(&mut app);
        assert!(s.contains("task 2 depends on"), "{s}");
        key(&mut app, KeyCode::Backspace);
        typ(&mut app, "x");
        key(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_some());
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        assert!(q.get(2).unwrap().task.meta.depends_on.is_empty());
    }

    #[test]
    fn unread_mail_shows_for_todo_tasks() {
        let (d, q) = demo();
        q.say(2, "orchestrator", crate::chat::Kind::Message, "Question?")
            .unwrap();
        let mut app = App::new(Some(q), d.path().to_path_buf(), None);
        let s = screen(&mut app);
        assert!(
            s.lines()
                .any(|l| l.contains("Fix cache") && l.contains('✉')),
            "{s}"
        );
    }
}
