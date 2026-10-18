//! The user settings in `$XDG_CONFIG_HOME/aq/config.toml`, else in
//! `~/.config/aq/config.toml`.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::queue::write_atomic;
use crate::tui::Filter;

/// The key of the width of the task list, in columns.
const LIST_WIDTH: &str = "list_width";
/// The key of the filter of the task list at the start of the TUI.
const LIST_FILTER: &str = "list_filter";
/// The key of the editor command.
const EDITOR: &str = "editor";
/// The key of the switch for desktop notifications.
const NOTIFICATIONS: &str = "notifications";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The width of the task list in columns. `None` uses the automatic width.
    pub list_width: Option<u16>,
    /// The filter of the task list at the start of the TUI.
    pub list_filter: Filter,
    /// The command that edits a file. An empty command uses `$VISUAL`, else
    /// `$EDITOR`, else `vi`.
    pub editor: String,
    /// True if the TUI sends a desktop notification when another process
    /// changes the status of a task.
    pub notifications: bool,
}

/// The path of the config file for the values of `$XDG_CONFIG_HOME` and `$HOME`.
/// An empty or relative `$XDG_CONFIG_HOME` is ignored.
pub fn path_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = xdg
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("aq").join("config.toml"))
}

/// The path of the config file of the user.
pub fn default_path() -> Option<PathBuf> {
    path_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Reads the config file. A missing file gives the defaults.
pub fn load(path: &Path) -> Result<Config> {
    let table = read_table(path)?;
    parse(&table).with_context(|| format!("config file {}", path.display()))
}

fn read_table(path: &Path) -> Result<toml::Table> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .parse::<toml::Table>()
            .with_context(|| format!("config file {}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn parse(t: &toml::Table) -> Result<Config> {
    let list_width = match t.get(LIST_WIDTH) {
        None => None,
        Some(v) => {
            let n = v
                .as_integer()
                .with_context(|| format!("`{LIST_WIDTH}` must be a number"))?;
            Some(
                u16::try_from(n)
                    .ok()
                    .filter(|n| *n > 0)
                    .with_context(|| format!("`{LIST_WIDTH}` must be 1..65535"))?,
            )
        }
    };
    let list_filter = match t.get(LIST_FILTER) {
        None => Filter::default(),
        Some(v) => v.as_str().and_then(Filter::from_name).with_context(|| {
            let names: Vec<_> = Filter::CYCLE.iter().map(|f| f.name()).collect();
            format!("`{LIST_FILTER}` must be one of {}", names.join(", "))
        })?,
    };
    let editor = match t.get(EDITOR) {
        None => String::new(),
        Some(v) => v
            .as_str()
            .with_context(|| format!("`{EDITOR}` must be a string"))?
            .trim()
            .to_string(),
    };
    let notifications = match t.get(NOTIFICATIONS) {
        None => false,
        Some(v) => v
            .as_bool()
            .with_context(|| format!("`{NOTIFICATIONS}` must be true or false"))?,
    };
    Ok(Config {
        list_width,
        list_filter,
        editor,
        notifications,
    })
}

/// Writes `cfg` to the config file and makes its directory. Keys that aq does not
/// know stay in the file.
pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    let mut table = read_table(path)?;
    match cfg.list_width {
        Some(w) => table.insert(LIST_WIDTH.into(), toml::Value::Integer(w.into())),
        None => table.remove(LIST_WIDTH),
    };
    if cfg.list_filter == Filter::default() {
        table.remove(LIST_FILTER);
    } else {
        table.insert(
            LIST_FILTER.into(),
            toml::Value::String(cfg.list_filter.name().into()),
        );
    }
    if cfg.editor.trim().is_empty() {
        table.remove(EDITOR);
    } else {
        table.insert(EDITOR.into(), toml::Value::String(cfg.editor.trim().into()));
    }
    if cfg.notifications {
        table.insert(NOTIFICATIONS.into(), toml::Value::Boolean(true));
    } else {
        table.remove(NOTIFICATIONS);
    }
    let dir = path.parent().context("the config path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("make {}", dir.display()))?;
    write_atomic(path, &toml::to_string(&table)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_uses_xdg_then_home() {
        let p = path_from(Some("/x".into()), Some("/home/u".into()));
        assert_eq!(p, Some(PathBuf::from("/x/aq/config.toml")));
        let p = path_from(Some("rel".into()), Some("/home/u".into()));
        assert_eq!(p, Some(PathBuf::from("/home/u/.config/aq/config.toml")));
        let p = path_from(None, Some("/home/u".into()));
        assert_eq!(p, Some(PathBuf::from("/home/u/.config/aq/config.toml")));
        assert_eq!(path_from(Some("".into()), None), None);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let d = tempfile::tempdir().unwrap();
        let cfg = load(&d.path().join("aq/config.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn save_and_load_keep_other_keys() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("aq/config.toml");
        let cfg = Config {
            list_width: Some(40),
            list_filter: Filter::Review,
            editor: "nvim -p".into(),
            notifications: true,
        };
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path).unwrap(), cfg);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("list_filter = \"review\""), "{text}");
        fs::write(
            &path,
            "future = true\nlist_width = 30\nlist_filter = \"all\"\n",
        )
        .unwrap();
        save(&path, &Config::default()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("future = true"), "{text}");
        assert!(!text.contains("list_width"), "{text}");
        assert!(!text.contains("list_filter"), "{text}");
        assert_eq!(load(&path).unwrap(), Config::default());
    }

    #[test]
    fn invalid_file_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        fs::write(&path, "list_width = \"wide\"\n").unwrap();
        let e = format!("{:#}", load(&path).unwrap_err());
        assert!(e.contains("list_width"), "{e}");
        fs::write(&path, "list_width = 0\n").unwrap();
        assert!(load(&path).is_err());
        fs::write(&path, "notifications = 1\n").unwrap();
        let e = format!("{:#}", load(&path).unwrap_err());
        assert!(e.contains("notifications"), "{e}");
        fs::write(&path, "list_filter = \"todo\"\n").unwrap();
        let e = format!("{:#}", load(&path).unwrap_err());
        assert!(e.contains("list_filter"), "{e}");
        assert!(e.contains("open, review, blocked, all, done"), "{e}");
        fs::write(&path, "list_filter = 1\n").unwrap();
        assert!(load(&path).is_err());
        fs::write(&path, "not toml [").unwrap();
        assert!(load(&path).is_err());
    }
}
