//! Renders Markdown to `ratatui` lines.
//!
//! Supported: paragraphs, headings, bold, italic, strikethrough, inline code, fenced code
//! blocks with syntax colors, lists, quotes, links, rules, and colored text with the
//! bracketed span of Pandoc: `[text]{.red}`.

use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;

pub const COLORS: [(&str, Color); 7] = [
    ("red", Color::Red),
    ("green", Color::Green),
    ("yellow", Color::Yellow),
    ("blue", Color::Blue),
    ("magenta", Color::Magenta),
    ("cyan", Color::Cyan),
    ("gray", Color::Gray),
];

const COLOR_START: char = '\u{E000}';
const COLOR_END: char = '\u{E001}';
const CODE_BG: Color = Color::Rgb(40, 44, 52);

pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<String>,
}

/// Renders `text`. `base` is the style of normal text.
pub fn render(text: &str, base: Style) -> Rendered {
    let src = mark_color_spans(text);
    let mut r = Renderer {
        base,
        lines: Vec::new(),
        cur: Vec::new(),
        styles: vec![base],
        lists: Vec::new(),
        item_prefix: None,
        item_indent: 0,
        quote: 0,
        code: None,
        links: Vec::new(),
    };
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(&src, opts) {
        r.event(ev);
    }
    r.flush();
    while r.lines.last().is_some_and(|l| l.spans.is_empty()) {
        r.lines.pop();
    }
    Rendered {
        lines: r.lines,
        links: r.links,
    }
}

/// Replaces `[text]{.color}` with private-use markers, so that the parser keeps the
/// text as text and the renderer can color it.
fn mark_color_spans(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let found = after.find(']').and_then(|close| {
            let inner = &after[..close];
            if inner.contains('[') {
                return None;
            }
            let tail = after[close + 1..].strip_prefix("{.")?;
            let end = tail.find('}')?;
            let idx = COLORS.iter().position(|(n, _)| *n == &tail[..end])?;
            Some((inner, idx, open + 1 + close + 1 + 2 + end + 1))
        });
        match found {
            Some((inner, idx, consumed)) => {
                out.push(COLOR_START);
                out.push(char::from(b'0' + idx as u8));
                out.push_str(inner);
                out.push(COLOR_END);
                rest = &rest[consumed..];
            }
            None => {
                out.push('[');
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

struct Renderer {
    base: Style,
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    item_prefix: Option<String>,
    item_indent: usize,
    quote: usize,
    code: Option<(String, String)>,
    links: Vec<String>,
}

impl Renderer {
    fn style(&self) -> Style {
        *self.styles.last().unwrap_or(&self.base)
    }

    fn push_style(&mut self, f: impl FnOnce(Style) -> Style) {
        let s = f(self.style());
        self.styles.push(s);
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn prefix(&mut self) -> Vec<Span<'static>> {
        let mut p = Vec::new();
        if self.quote > 0 {
            p.push(Span::styled(
                "│ ".repeat(self.quote),
                Style::default().fg(Color::DarkGray),
            ));
        }
        match self.item_prefix.take() {
            Some(bullet) => p.push(Span::styled(bullet, self.base.fg(Color::DarkGray))),
            None if self.item_indent > 0 => p.push(Span::raw(" ".repeat(self.item_indent))),
            None => {}
        }
        p
    }

    fn flush(&mut self) {
        if self.cur.is_empty() && self.item_prefix.is_none() {
            return;
        }
        let mut spans = self.prefix();
        spans.append(&mut self.cur);
        self.lines.push(Line::from(spans));
    }

    fn blank(&mut self) {
        if self.lines.last().is_some_and(|l| !l.spans.is_empty()) && self.lists.is_empty() {
            self.lines.push(Line::default());
        }
    }

    fn text(&mut self, t: &str) {
        let mut buf = String::new();
        let mut chars = t.chars();
        while let Some(c) = chars.next() {
            match c {
                COLOR_START => {
                    self.emit(&mut buf);
                    let idx = chars.next().map_or(0, |d| (d as u8).saturating_sub(b'0'));
                    let color = COLORS.get(idx as usize).map_or(Color::Reset, |c| c.1);
                    self.push_style(|s| s.fg(color));
                }
                COLOR_END => {
                    self.emit(&mut buf);
                    self.pop_style();
                }
                c => buf.push(c),
            }
        }
        self.emit(&mut buf);
    }

    fn emit(&mut self, buf: &mut String) {
        if !buf.is_empty() {
            self.cur
                .push(Span::styled(std::mem::take(buf), self.style()));
        }
    }

    fn event(&mut self, ev: Event<'_>) {
        if let Some((_, buf)) = self.code.as_mut() {
            match ev {
                Event::Text(t) => {
                    buf.push_str(&t);
                    return;
                }
                Event::End(TagEnd::CodeBlock) => {}
                _ => return,
            }
        }
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.cur.push(Span::styled(
                t.into_string(),
                self.style().fg(Color::LightYellow).bg(CODE_BG),
            )),
            Event::Html(t) | Event::InlineHtml(t) => self.text(&t),
            Event::SoftBreak => self.cur.push(Span::styled(" ", self.style())),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.blank();
                self.lines.push(Line::styled(
                    "─".repeat(24),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Event::TaskListMarker(done) => {
                self.cur.push(Span::raw(if done { "[x] " } else { "[ ] " }));
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph if self.item_prefix.is_none() => {
                self.flush();
                self.blank();
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.blank();
                let color = match level {
                    HeadingLevel::H1 => Color::LightCyan,
                    HeadingLevel::H2 => Color::Cyan,
                    _ => Color::Reset,
                };
                self.push_style(|s| s.add_modifier(Modifier::BOLD).fg(color));
            }
            Tag::Emphasis => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(|s| s.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.links.push(dest_url.into_string());
                self.push_style(|s| s.fg(Color::LightBlue).add_modifier(Modifier::UNDERLINED));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.blank();
                self.quote += 1;
                self.push_style(|s| s.add_modifier(Modifier::ITALIC));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.blank();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.split_whitespace().next().unwrap_or("").into(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len().saturating_sub(1);
                let bullet = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let b = format!("{n}. ");
                        *n += 1;
                        b
                    }
                    _ => "• ".to_string(),
                };
                let indent = "  ".repeat(depth);
                self.item_indent = indent.len() + bullet.chars().count();
                self.item_prefix = Some(format!("{indent}{bullet}"));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.pop_style()
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
                self.pop_style();
            }
            TagEnd::CodeBlock => {
                if let Some((lang, buf)) = self.code.take() {
                    let prefix = self.prefix();
                    for line in highlight(&lang, &buf) {
                        let mut spans = prefix.clone();
                        spans.extend(line);
                        self.lines.push(Line::from(spans));
                    }
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.item_indent = 0;
                }
            }
            TagEnd::Item => {
                self.flush();
                self.item_prefix = None;
            }
            _ => {}
        }
    }
}

fn syntaxes() -> &'static (SyntaxSet, ThemeSet) {
    static SETS: OnceLock<(SyntaxSet, ThemeSet)> = OnceLock::new();
    SETS.get_or_init(|| {
        (
            SyntaxSet::load_defaults_newlines(),
            ThemeSet::load_defaults(),
        )
    })
}

/// Highlights a code block. Each line has a dark background and a space on each side.
fn highlight(lang: &str, code: &str) -> Vec<Vec<Span<'static>>> {
    let (ss, ts) = syntaxes();
    let syntax = ss
        .find_syntax_by_token(lang)
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let theme = &ts.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);
    let bg = Style::default().bg(CODE_BG);
    let mut out = Vec::new();
    for line in code.lines() {
        let mut spans = vec![Span::styled(" ", bg)];
        let line_nl = format!("{line}\n");
        match h.highlight_line(&line_nl, ss) {
            Ok(ranges) => {
                for (st, piece) in ranges {
                    let piece = piece.trim_end_matches('\n');
                    if piece.is_empty() {
                        continue;
                    }
                    let mut s = bg.fg(Color::Rgb(
                        st.foreground.r,
                        st.foreground.g,
                        st.foreground.b,
                    ));
                    if st.font_style.contains(FontStyle::BOLD) {
                        s = s.add_modifier(Modifier::BOLD);
                    }
                    if st.font_style.contains(FontStyle::ITALIC) {
                        s = s.add_modifier(Modifier::ITALIC);
                    }
                    spans.push(Span::styled(piece.to_string(), s));
                }
            }
            Err(_) => spans.push(Span::styled(line.to_string(), bg)),
        }
        spans.push(Span::styled(" ", bg));
        out.push(spans);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(r: &Rendered) -> Vec<String> {
        r.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn color_span() {
        let r = render("a [**red** x]{.red} b [not]{.pink}", Style::default());
        assert_eq!(plain(&r), vec!["a red x b [not]{.pink}"]);
        let red = r.lines[0]
            .spans
            .iter()
            .find(|s| s.content == "red")
            .unwrap();
        assert_eq!(red.style.fg, Some(Color::Red));
        assert!(red.style.add_modifier.contains(Modifier::BOLD));
        let x = r.lines[0].spans.iter().find(|s| s.content == " x").unwrap();
        assert_eq!(x.style.fg, Some(Color::Red));
    }

    #[test]
    fn lists_and_links() {
        let r = render(
            "Intro\n\n- one\n- two [site](https://e.org)\n\n1. a\n2. b",
            Style::default(),
        );
        assert_eq!(
            plain(&r),
            vec!["Intro", "", "• one", "• two site", "", "1. a", "2. b"]
        );
        assert_eq!(r.links, vec!["https://e.org"]);
    }

    #[test]
    fn code_block() {
        let r = render("```rust\nfn main() {}\n```", Style::default());
        assert_eq!(plain(&r), vec![" fn main() {} "]);
    }
}
