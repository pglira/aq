//! Key handling for text fields: a normal text field, not a vim-like one.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::{CursorMove, TextArea};

/// Applies an editing key to `ta`. Returns false if the key is not an editing key.
/// `multiline` allows new lines with `Shift+Enter` and `Alt+Enter`.
pub fn edit_key(ta: &mut TextArea<'_>, key: KeyEvent, multiline: bool) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('z') if ctrl => {
            ta.undo();
        }
        KeyCode::Char('x') if ctrl => {
            ta.cut();
        }
        KeyCode::Char('c') if ctrl => ta.copy(),
        // Readline keys, as in a shell. Ctrl+U, Ctrl+K, Ctrl+W and Alt+D keep the
        // deleted text, and Ctrl+Y inserts it again.
        KeyCode::Char('w') if ctrl => {
            ta.delete_word();
        }
        KeyCode::Char('y') if ctrl => {
            ta.paste();
        }
        KeyCode::Char('u') if ctrl => {
            ta.delete_line_by_head();
        }
        KeyCode::Char('k') if ctrl => {
            ta.delete_line_by_end();
        }
        KeyCode::Char('d') if ctrl => {
            if !(ta.is_selecting() && ta.cut()) {
                ta.delete_next_char();
            }
        }
        KeyCode::Char('h') if ctrl => {
            if !(ta.is_selecting() && ta.cut()) {
                ta.delete_char();
            }
        }
        KeyCode::Char('d') if alt => {
            ta.delete_next_word();
        }
        KeyCode::Char('a') if ctrl => move_plain(ta, CursorMove::Head),
        KeyCode::Char('e') if ctrl => move_plain(ta, CursorMove::End),
        KeyCode::Char('b') if ctrl => move_plain(ta, CursorMove::Back),
        KeyCode::Char('f') if ctrl => move_plain(ta, CursorMove::Forward),
        KeyCode::Char('p') if ctrl => move_plain(ta, CursorMove::Up),
        KeyCode::Char('n') if ctrl => move_plain(ta, CursorMove::Down),
        KeyCode::Char('b') if alt => move_plain(ta, CursorMove::WordBack),
        KeyCode::Char('f') if alt => move_plain(ta, CursorMove::WordForward),
        KeyCode::Char(c) if !ctrl && !alt => {
            if ta.is_selecting() {
                ta.cut();
            }
            ta.insert_char(c);
        }
        KeyCode::Enter if multiline && (shift || alt) => {
            if ta.is_selecting() {
                ta.cut();
            }
            ta.insert_newline();
        }
        KeyCode::Backspace if ctrl || alt => {
            ta.delete_word();
        }
        KeyCode::Backspace => {
            if !(ta.is_selecting() && ta.cut()) {
                ta.delete_char();
            }
        }
        KeyCode::Delete => {
            if !(ta.is_selecting() && ta.cut()) {
                ta.delete_next_char();
            }
        }
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End => {
            if shift {
                if !ta.is_selecting() {
                    ta.start_selection();
                }
            } else {
                ta.cancel_selection();
            }
            let m = match key.code {
                KeyCode::Left if ctrl => CursorMove::WordBack,
                KeyCode::Right if ctrl => CursorMove::WordForward,
                KeyCode::Left => CursorMove::Back,
                KeyCode::Right => CursorMove::Forward,
                KeyCode::Up => CursorMove::Up,
                KeyCode::Down => CursorMove::Down,
                KeyCode::Home if ctrl => CursorMove::Top,
                KeyCode::End if ctrl => CursorMove::Bottom,
                KeyCode::Home => CursorMove::Head,
                _ => CursorMove::End,
            };
            ta.move_cursor(m);
        }
        _ => return false,
    }
    true
}

/// Moves the cursor and ends the selection.
fn move_plain(ta: &mut TextArea<'_>, m: CursorMove) {
    ta.cancel_selection();
    ta.move_cursor(m);
}

/// Inserts pasted text. A single-line field gets the text with spaces for new lines.
pub fn paste(ta: &mut TextArea<'_>, text: &str, multiline: bool) {
    if ta.is_selecting() {
        ta.cut();
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        ta.insert_str(text);
    } else {
        ta.insert_str(text.replace('\n', " "));
    }
}

pub fn text_of(ta: &TextArea<'_>) -> String {
    ta.lines().join("\n")
}

pub fn new_area(text: &str) -> TextArea<'static> {
    let lines: Vec<String> = if text.is_empty() {
        vec![String::new()]
    } else {
        text.lines().map(str::to_string).collect()
    };
    let mut ta = TextArea::new(lines);
    ta.set_cursor_line_style(ratatui::style::Style::default());
    ta.move_cursor(CursorMove::Bottom);
    ta.move_cursor(CursorMove::End);
    ta
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), m)
    }

    #[test]
    fn emacs_kill_keys() {
        let mut ta = new_area("hello world");
        ta.move_cursor(CursorMove::Head);
        ta.move_cursor(CursorMove::WordForward);
        assert!(edit_key(&mut ta, key('k', KeyModifiers::CONTROL), false));
        assert_eq!(text_of(&ta), "hello ");
        assert!(edit_key(&mut ta, key('u', KeyModifiers::CONTROL), false));
        assert_eq!(text_of(&ta), "");
    }

    #[test]
    fn emacs_move_and_delete_keys() {
        let mut ta = new_area("ab cd");
        ta.move_cursor(CursorMove::Head);
        edit_key(&mut ta, key('f', KeyModifiers::CONTROL), false);
        edit_key(&mut ta, key('d', KeyModifiers::CONTROL), false);
        assert_eq!(text_of(&ta), "a cd");
        edit_key(&mut ta, key('f', KeyModifiers::ALT), false);
        edit_key(&mut ta, key('h', KeyModifiers::CONTROL), false);
        assert_eq!(text_of(&ta), "acd");
        edit_key(&mut ta, key('d', KeyModifiers::ALT), false);
        assert_eq!(text_of(&ta), "a");
    }

    #[test]
    fn readline_line_keys_and_yank() {
        let mut ta = new_area("ab cd");
        assert!(edit_key(&mut ta, key('a', KeyModifiers::CONTROL), false));
        edit_key(&mut ta, key('f', KeyModifiers::CONTROL), false);
        edit_key(&mut ta, key('f', KeyModifiers::CONTROL), false);
        edit_key(&mut ta, key('b', KeyModifiers::CONTROL), false);
        edit_key(&mut ta, key('k', KeyModifiers::CONTROL), false);
        assert_eq!(text_of(&ta), "a");
        edit_key(&mut ta, key('a', KeyModifiers::CONTROL), false);
        assert!(edit_key(&mut ta, key('y', KeyModifiers::CONTROL), false));
        assert_eq!(text_of(&ta), "b cda");
        assert!(edit_key(&mut ta, key('e', KeyModifiers::CONTROL), false));
        edit_key(&mut ta, key('x', KeyModifiers::NONE), false);
        assert_eq!(text_of(&ta), "b cdax");
    }
}
