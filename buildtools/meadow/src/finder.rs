//! **The declaration finder**: `Ctrl-F` in the REPL.
//!
//! A pop-up in the manner of Helix's pickers: a query at the top, what it
//! matches below, and on the right everything known about the one selected --
//! its signature, its module, where it was written, its doc comment, and what
//! choosing it will do. Typing narrows the list; `Enter` puts the name on the
//! prompt where the cursor was.
//!
//! The query is a name, matched fuzzily, or a type, matched the way Hoogle
//! matches one -- `[a] -> Int` finds `length`. Which it is, and how either is
//! ranked, is [`meadow_find`]'s business; this module only shows it.
//!
//! # Choosing something that is not in scope
//!
//! A program cannot write `Std.String.length`: a name from a module it has not
//! `use`d has no spelling at all. So a choice says, as well as what to insert,
//! how the name is reached -- in scope already, through a qualifier some `use
//! ... as` introduced, or not yet -- and in that last case the `use` that would
//! bring it in, which the REPL runs just before the line it was chosen for.
//! The preview says which of the three it is before anything is chosen.
//!
//! # Why a state machine and a driver
//!
//! [`Picker`] takes keys and draws frames and knows nothing of a terminal;
//! [`pick`] is the few lines that give it one. The picker is tested by driving
//! it with keys and reading what it drew into a buffer, which is most of what
//! can go wrong; the driver is small enough to be right by reading.

use meadow_compiler::hir::VarId;
use meadow_find::{Decl, Hit, Index, Kind, Query};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::collections::HashMap;

/// What the finder searches, and what it needs to know to say how a choice
/// can be named from the prompt.
#[derive(Default)]
pub struct Catalog {
    pub index: Index,
    /// Every value a line can name unqualified, and which binding the name
    /// means: a name in scope may mean a *different* declaration of that name,
    /// and inserting it would then be quietly wrong.
    pub scope: HashMap<String, VarId>,
    /// Each qualifier a `use ... as C` introduced, with what it reaches.
    pub aliases: HashMap<String, HashMap<String, VarId>>,
}

/// How a declaration can be named from the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// By its name, as it is.
    InScope,
    /// Through a qualifier: `S.length`.
    Via(String),
    /// Not yet: this `use` would bring it in.
    NeedsUse(String),
    /// By its name, and nothing more can be said: a type, a constructor, an
    /// operator. Types and constructors are imported wholesale by a `use` of
    /// their module, and an operator is spelled as it is.
    AsNamed,
}

impl Catalog {
    pub fn reach(&self, d: &Decl) -> Reach {
        let Some(var) = d.var else {
            return Reach::AsNamed;
        };
        if self.scope.get(&d.name) == Some(&var) {
            return Reach::InScope;
        }
        let mut via: Vec<&String> = self
            .aliases
            .iter()
            .filter(|(_, names)| names.get(&d.name) == Some(&var))
            .map(|(alias, _)| alias)
            .collect();
        via.sort();
        if let Some(alias) = via.first() {
            return Reach::Via((*alias).clone());
        }
        // An operator cannot go in a `use` list as it is written, and one that
        // matters is in the prelude anyway.
        if d.name.starts_with('(') {
            return Reach::AsNamed;
        }
        Reach::NeedsUse(format!("use {} ({})", d.module, d.name))
    }

    /// What choosing `d` puts on the prompt, and the `use` to run first, if one
    /// has to be.
    pub fn choose(&self, d: &Decl) -> Choice {
        match self.reach(d) {
            Reach::InScope | Reach::AsNamed => Choice {
                text: d.name.clone(),
                needs: None,
            },
            Reach::Via(alias) => Choice {
                text: format!("{alias}.{}", d.name),
                needs: None,
            },
            Reach::NeedsUse(line) => Choice {
                text: d.name.clone(),
                needs: Some(line),
            },
        }
    }
}

/// What was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// Goes on the prompt, at the cursor.
    pub text: String,
    /// A `use` to run before the line this was chosen for.
    pub needs: Option<String>,
}

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    Choose(Choice),
    Cancel,
}

/// The finder, apart from any terminal.
pub struct Picker<'a> {
    catalog: &'a Catalog,
    query: String,
    /// In characters, not bytes.
    cursor: usize,
    hits: Vec<Hit>,
    list: ListState,
    /// No colour: `NO_COLOR` is set, or a test is reading the text.
    plain: bool,
}

impl<'a> Picker<'a> {
    pub fn new(catalog: &'a Catalog) -> Picker<'a> {
        let mut p = Picker {
            catalog,
            query: String::new(),
            cursor: 0,
            hits: Vec::new(),
            list: ListState::default(),
            plain: std::env::var_os("NO_COLOR").is_some(),
        };
        p.refresh();
        p
    }

    /// Without colour, whatever the environment says.
    pub fn plain(mut self) -> Self {
        self.plain = true;
        self
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The declaration selected, if the list has any.
    pub fn selected(&self) -> Option<&'a Decl> {
        let hit = self.hits.get(self.list.selected()?)?;
        Some(&self.catalog.index.decls[hit.decl])
    }

    /// The declarations listed, best first.
    pub fn listed(&self) -> impl Iterator<Item = &'a Decl> + '_ {
        self.hits.iter().map(|h| &self.catalog.index.decls[h.decl])
    }

    fn refresh(&mut self) {
        let catalog = self.catalog;
        // What the prompt can already name wins a tie. Asked once per
        // comparison of a sort, so it is a lookup and not [`Catalog::reach`].
        let in_scope = |d: &Decl| match d.var {
            Some(v) => catalog.scope.get(&d.name) == Some(&v),
            None => d.prelude,
        };
        self.hits = catalog
            .index
            .search_with(&Query::parse(&self.query), usize::MAX, &in_scope);
        self.list
            .select(if self.hits.is_empty() { None } else { Some(0) });
        *self.list.offset_mut() = 0;
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.query
            .char_indices()
            .nth(chars)
            .map_or(self.query.len(), |(i, _)| i)
    }

    fn edit(&mut self, f: impl FnOnce(&mut Self)) {
        let before = self.query.clone();
        f(self);
        if self.query != before {
            self.refresh();
        }
    }

    fn step_selection(&mut self, by: isize) {
        let n = self.hits.len();
        if n == 0 {
            return;
        }
        let at = self.list.selected().unwrap_or(0) as isize;
        // Wraps, as Helix's does: past the bottom is the top.
        let next = (at + by).rem_euclid(n as isize);
        self.list.select(Some(next as usize));
    }

    pub fn key(&mut self, key: KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Step::Cancel,
            KeyCode::Char('c' | 'f') if ctrl => return Step::Cancel,
            KeyCode::Enter => {
                return match self.selected() {
                    Some(d) => Step::Choose(self.catalog.choose(d)),
                    None => Step::Continue,
                };
            }
            KeyCode::Up | KeyCode::BackTab => self.step_selection(-1),
            KeyCode::Down | KeyCode::Tab => self.step_selection(1),
            KeyCode::Char('p' | 'k') if ctrl => self.step_selection(-1),
            KeyCode::Char('n' | 'j') if ctrl => self.step_selection(1),
            KeyCode::PageUp => self.step_selection(-10),
            KeyCode::PageDown => self.step_selection(10),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.query.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.query.chars().count(),
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.query.chars().count(),
            KeyCode::Char('u') if ctrl => self.edit(|p| {
                let at = p.byte_at(p.cursor);
                p.query.replace_range(..at, "");
                p.cursor = 0;
            }),
            KeyCode::Char('w') if ctrl => self.edit(|p| {
                let end = p.byte_at(p.cursor);
                let head = &p.query[..end];
                let trimmed = head.trim_end();
                let start = trimmed
                    .rfind(|c: char| c.is_whitespace())
                    .map_or(0, |i| i + 1);
                p.cursor -= head[start..].chars().count();
                p.query.replace_range(start..end, "");
            }),
            KeyCode::Backspace => self.edit(|p| {
                if p.cursor > 0 {
                    let at = p.byte_at(p.cursor - 1);
                    p.query.remove(at);
                    p.cursor -= 1;
                }
            }),
            KeyCode::Delete => self.edit(|p| {
                if p.cursor < p.query.chars().count() {
                    let at = p.byte_at(p.cursor);
                    p.query.remove(at);
                }
            }),
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.edit(|p| {
                    let at = p.byte_at(p.cursor);
                    p.query.insert(at, c);
                    p.cursor += 1;
                })
            }
            _ => {}
        }
        Step::Continue
    }

    // --- drawing -------------------------------------------------------------

    fn style(&self, s: Style) -> Style {
        if self.plain { Style::default() } else { s }
    }

    fn kind_colour(kind: Kind) -> Color {
        match kind {
            Kind::Function | Kind::Value | Kind::Method | Kind::Operation => Color::Blue,
            Kind::Macro => Color::Magenta,
            Kind::Constructor => Color::Green,
            Kind::Type | Kind::Record | Kind::Alias => Color::Yellow,
            Kind::Trait => Color::Cyan,
            Kind::Effect => Color::Red,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = popup(frame.area());
        frame.render_widget(Clear, area);
        // A preview needs room beside the list; on a narrow terminal the list
        // is what matters.
        let (left, right) = if area.width >= 90 {
            let [l, r] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(area);
            (l, Some(r))
        } else {
            (area, None)
        };
        self.draw_list(frame, left);
        if let Some(right) = right {
            self.draw_preview(frame, right);
        }
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let query = Query::parse(&self.query);
        let mode = if query.is_type() {
            " by type "
        } else {
            " by name "
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(Line::from(" Find a declaration ").style(self.style(Style::new().bold())))
            .title(
                Line::from(mode)
                    .right_aligned()
                    .style(self.style(Style::new().dim())),
            )
            .title_bottom(
                Line::from(" ↵ insert · ↑↓ move · esc close ")
                    .style(self.style(Style::new().dim())),
            );
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [prompt, rule, rows] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);

        // The prompt, with the count on the right.
        let count = format!("{}/{} ", self.hits.len(), self.catalog.index.len());
        let [text, tally] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(count.chars().count() as u16),
        ])
        .areas(prompt);
        let shown = if self.query.is_empty() {
            Line::from(vec![
                Span::styled("❯ ", self.style(Style::new().fg(Color::Cyan))),
                Span::styled(
                    "a name, or a type like  [a] -> Int",
                    self.style(Style::new().dim()),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled("❯ ", self.style(Style::new().fg(Color::Cyan))),
                Span::raw(self.query.clone()),
            ])
        };
        frame.render_widget(Paragraph::new(shown), text);
        frame.render_widget(
            Paragraph::new(count).style(self.style(Style::new().dim())),
            tally,
        );
        frame.set_cursor_position(Position::new(
            text.x + 2 + self.query[..self.byte_at(self.cursor)].chars().count() as u16,
            text.y,
        ));
        frame.render_widget(
            Paragraph::new("─".repeat(rule.width as usize)).style(self.style(Style::new().dim())),
            rule,
        );

        // Columns: what it is, its name, its type, and -- at the right, dim --
        // the module it is in, which is what tells two `repeat`s apart. The
        // name and module columns are as wide as what is listed needs, within
        // reason; the type gets what is left, and is cut short first.
        let width = (rows.width as usize).saturating_sub(1); // the `▌`
        let decls: Vec<&Decl> = self
            .hits
            .iter()
            .map(|h| &self.catalog.index.decls[h.decl])
            .collect();
        let name_w = decls
            .iter()
            .map(|d| d.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(24);
        let tag = |d: &Decl| d.module.rsplit('.').next().unwrap_or("").to_string();
        let tag_w = decls
            .iter()
            .map(|d| tag(d).chars().count())
            .max()
            .unwrap_or(0)
            .min(16);
        let items: Vec<ListItem> = decls
            .iter()
            .map(|d| {
                let label = format!("{:<7}", d.kind.label());
                let name = format!("{:<name_w$}", truncate(&d.name, name_w));
                let fixed = 7 + name_w + 2 + 2 + tag_w;
                let room = width.saturating_sub(fixed);
                let detail = if d.kind.is_value() {
                    truncate(&d.detail, room)
                } else {
                    String::new()
                };
                let pad = room.saturating_sub(detail.chars().count());
                ListItem::new(Line::from(vec![
                    Span::styled(
                        label,
                        self.style(Style::new().fg(Self::kind_colour(d.kind))),
                    ),
                    Span::styled(name, self.style(Style::new().bold())),
                    Span::raw("  "),
                    Span::raw(detail),
                    Span::raw(" ".repeat(pad + 2)),
                    Span::styled(
                        format!("{:>tag_w$}", truncate(&tag(d), tag_w)),
                        self.style(Style::new().dim()),
                    ),
                ]))
            })
            .collect();
        let highlight = if self.plain {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().bg(Color::DarkGray)
        };
        let list = List::new(items)
            .highlight_style(highlight)
            .highlight_symbol("▌")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
        frame.render_stateful_widget(list, rows, &mut self.list);
    }

    fn draw_preview(&self, frame: &mut Frame, area: Rect) {
        let Some(d) = self.selected() else {
            let empty = Block::bordered().border_type(BorderType::Rounded);
            let hint = if self.query.is_empty() {
                "Nothing to show."
            } else {
                "Nothing matches."
            };
            frame.render_widget(
                Paragraph::new(hint)
                    .style(self.style(Style::new().dim()))
                    .block(empty),
                area,
            );
            return;
        };
        let block = Block::bordered().border_type(BorderType::Rounded).title(
            Line::from(format!(" {} ", d.qualified())).style(self.style(Style::new().bold())),
        );
        let dim = self.style(Style::new().dim());
        let mut lines = vec![
            Line::from(Span::styled(
                d.headline(),
                self.style(Style::new().fg(Self::kind_colour(d.kind)).bold()),
            )),
            Line::raw(""),
            Line::from(vec![
                Span::styled("module  ", dim),
                Span::raw(d.module.clone()),
            ]),
        ];
        if let Some(loc) = &d.location {
            lines.push(Line::from(vec![
                Span::styled("where   ", dim),
                Span::raw(format!("{}:{}", loc.file, loc.line)),
            ]));
        }
        match self.catalog.reach(d) {
            Reach::InScope | Reach::AsNamed => {
                lines.push(Line::from(vec![
                    Span::styled("scope   ", dim),
                    Span::raw("in scope"),
                ]));
            }
            Reach::Via(alias) => lines.push(Line::from(vec![
                Span::styled("scope   ", dim),
                Span::raw(format!("as {alias}.{}", d.name)),
            ])),
            Reach::NeedsUse(line) => {
                lines.push(Line::from(vec![
                    Span::styled("scope   ", dim),
                    Span::raw("not yet; ↵ also runs"),
                ]));
                lines.push(Line::from(vec![
                    Span::raw("        "),
                    Span::styled(line, self.style(Style::new().fg(Color::Cyan))),
                ]));
            }
        }
        if let Some(doc) = &d.doc {
            lines.push(Line::raw(""));
            lines.extend(doc.lines().map(|l| Line::raw(l.to_string())));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(block),
            area,
        );
    }
}

/// Where the pop-up goes: most of the screen, centred, as Helix's is -- and
/// all of it when there is little of it.
fn popup(screen: Rect) -> Rect {
    let width = if screen.width < 60 {
        screen.width
    } else {
        (screen.width * 9 / 10).min(160)
    };
    let height = if screen.height < 16 {
        screen.height
    } else {
        (screen.height * 8 / 10).min(48)
    };
    Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    s.chars().take(max - 1).collect::<String>() + "…"
}

/// Show the finder over the terminal, and answer what was chosen.
///
/// Called while a line editor has the terminal, so it leaves the terminal's
/// mode alone -- the editor's raw mode is what it needs, and putting the
/// terminal back afterwards would be putting it back into the wrong one. It
/// draws on the alternate screen, which is how what was on the screen before
/// comes back exactly as it was.
pub fn pick(catalog: &Catalog) -> std::io::Result<Option<Choice>> {
    use ratatui::crossterm::{cursor, event, execute, terminal};
    let mut out = std::io::stdout();
    execute!(out, terminal::EnterAlternateScreen)?;
    let result = (|| {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
        terminal.clear()?;
        let mut picker = Picker::new(catalog);
        loop {
            terminal.draw(|f| picker.draw(f))?;
            match event::read()? {
                // A key is reported pressed and released on some platforms;
                // only the press is a keystroke.
                event::Event::Key(k) if k.kind != KeyEventKind::Release => match picker.key(k) {
                    Step::Continue => {}
                    Step::Choose(c) => return Ok(Some(c)),
                    Step::Cancel => return Ok(None),
                },
                _ => {}
            }
        }
    })();
    execute!(out, cursor::Show, terminal::LeaveAlternateScreen)?;
    result
}
