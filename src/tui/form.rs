//! The form to make or to edit a task.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::TextArea;

use super::textedit::{edit_key, new_area, paste, text_of};
use crate::queue::join_ids;
use crate::task::{PRIORITY_DEFAULT, PRIORITY_HIGH, PRIORITY_LOW, Task, parse_ids};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Description,
    Acceptance,
    Priority,
    Deps,
}

impl Field {
    pub const ALL: [Field; 5] = [
        Field::Title,
        Field::Description,
        Field::Acceptance,
        Field::Priority,
        Field::Deps,
    ];

    pub fn is_multiline(self) -> bool {
        matches!(self, Field::Description | Field::Acceptance)
    }
}

pub struct Form {
    /// The id of the edited task; `None` for a new task.
    pub id: Option<u32>,
    pub title: TextArea<'static>,
    pub priority: u8,
    pub deps: TextArea<'static>,
    pub description: TextArea<'static>,
    pub acceptance: TextArea<'static>,
    pub field: Field,
    pub error: Option<String>,
}

pub enum FormResult {
    Continue,
    Cancel,
    Save,
    /// Edit the field in `$EDITOR`.
    External(Field),
}

impl Form {
    pub fn new() -> Form {
        Form {
            id: None,
            title: new_area(""),
            priority: PRIORITY_DEFAULT,
            deps: new_area(""),
            description: new_area(""),
            acceptance: new_area(""),
            field: Field::Description,
            error: None,
        }
    }

    pub fn edit(task: &Task) -> Form {
        Form {
            id: Some(task.id()),
            title: new_area(&task.meta.title),
            priority: task.meta.priority,
            deps: new_area(&join_ids(&task.meta.depends_on)),
            description: new_area(&task.description),
            acceptance: new_area(&task.acceptance),
            field: Field::Title,
            error: None,
        }
    }

    pub fn area_mut(&mut self, f: Field) -> Option<&mut TextArea<'static>> {
        match f {
            Field::Title => Some(&mut self.title),
            Field::Deps => Some(&mut self.deps),
            Field::Description => Some(&mut self.description),
            Field::Acceptance => Some(&mut self.acceptance),
            Field::Priority => None,
        }
    }

    pub fn set_text(&mut self, f: Field, text: &str) {
        if let Some(ta) = self.area_mut(f) {
            *ta = new_area(text.trim_end());
        }
    }

    pub fn text(&self, f: Field) -> String {
        match f {
            Field::Title => text_of(&self.title),
            Field::Deps => text_of(&self.deps),
            Field::Description => text_of(&self.description),
            Field::Acceptance => text_of(&self.acceptance),
            Field::Priority => String::new(),
        }
    }

    fn step(&mut self, d: isize) {
        let i = Field::ALL
            .iter()
            .position(|f| *f == self.field)
            .unwrap_or(0) as isize;
        let n = Field::ALL.len() as isize;
        self.field = Field::ALL[((i + d).rem_euclid(n)) as usize];
    }

    pub fn on_key(&mut self, key: KeyEvent) -> FormResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return FormResult::Cancel,
            KeyCode::Char('s') if ctrl => return FormResult::Save,
            KeyCode::Char('g') if ctrl && self.field.is_multiline() => {
                return FormResult::External(self.field);
            }
            KeyCode::Tab => self.step(1),
            KeyCode::BackTab => self.step(-1),
            KeyCode::Enter if !self.field.is_multiline() && key.modifiers.is_empty() => {
                self.step(1)
            }
            KeyCode::Enter if self.field.is_multiline() && key.modifiers.is_empty() => {
                self.description_or_acceptance().insert_newline();
            }
            _ => match self.field {
                Field::Priority => match key.code {
                    KeyCode::Left | KeyCode::Char('h') => {
                        self.priority = (self.priority - 1).max(PRIORITY_HIGH)
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        self.priority = (self.priority + 1).min(PRIORITY_LOW)
                    }
                    KeyCode::Char(c @ '1'..='3') => self.priority = c as u8 - b'0',
                    _ => {}
                },
                f => {
                    let ml = f.is_multiline();
                    if let Some(ta) = self.area_mut(f) {
                        edit_key(ta, key, ml);
                    }
                }
            },
        }
        FormResult::Continue
    }

    fn description_or_acceptance(&mut self) -> &mut TextArea<'static> {
        if self.field == Field::Acceptance {
            &mut self.acceptance
        } else {
            &mut self.description
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        let f = self.field;
        if let Some(ta) = self.area_mut(f) {
            paste(ta, text, f.is_multiline());
        }
    }

    /// Checks the fields and writes them into `task`.
    pub fn apply(&self, task: &mut Task) -> Result<(), String> {
        let title = self.text(Field::Title).trim().to_string();
        let description = self.text(Field::Description).trim().to_string();
        if title.is_empty() && description.is_empty() {
            return Err("give a title or a description".into());
        }
        let deps = parse_ids(&self.text(Field::Deps)).map_err(|e| e.to_string())?;
        task.meta.title = title;
        task.meta.priority = self.priority;
        task.meta.depends_on = deps;
        task.description = description;
        task.acceptance = self.text(Field::Acceptance).trim().to_string();
        Ok(())
    }
}
