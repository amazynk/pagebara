//! Vim keys for the page view (Settings > Vim keys): a key-sequence parser that turns
//! keys into [`Action`]s, the ex command line (`:42`, `:w`, `:q`...), and the motions of
//! visual mode over a page's text. Nothing here touches the window; the app runs the
//! actions.
//!
//! Normal mode: `h j k l` scroll, `Ctrl-e`/`Ctrl-y` by a step, `Ctrl-d`/`Ctrl-u` half a
//! screen, `Ctrl-f`/`Ctrl-b` a screen, `J`/`K` next/previous page, `gg`/`G`/`{n}G` first,
//! last or nth page, `H`/`M`/`L` top, middle or bottom of the page, `zi`/`zo` zoom,
//! `zw`/`zp` fit width/page, `z0` actual size, `zz` page middle, `/` and `?` search,
//! `n`/`N` next/previous hit, `gt`/`gT`/`{n}gt` tabs, `m{a-z}` marks, `'{a-z}` and
//! `` `{a-z} `` jumps to them, `''` back, `Ctrl-o`/`Ctrl-i` the jump list, `u`/`Ctrl-r`
//! undo/redo, `]a`/`[a` next/previous annotation, `x` delete it, `v`/`V` visual mode and
//! `:` the command line. Counts go before a key (`5j`, `12G`).
//!
//! Mode names stay in English, as vim shows them.

use std::collections::VecDeque;
use std::ops::Range;

use pagebara_content::TextPage;

use crate::find::Hit;

/// A key as the parser sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Escape,
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    /// Any other key: normal mode leaves it to the app.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Visual { line: bool },
    Command,
    Search { forward: bool },
}

/// How the selection end moves in visual mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Down,
    Up,
    WordStart,
    WordBack,
    WordEnd,
    LineStart,
    LineEnd,
    PageStart,
    PageEnd,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    In,
    Out,
    FitWidth,
    FitPage,
    Actual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Next(usize),
    Prev(usize),
    /// 0-based.
    Go(usize),
}

/// A command of the `:` line.
#[derive(Clone, Debug, PartialEq)]
pub enum Ex {
    /// A page number as typed: the page with that label when there is one, as in the
    /// tool row's page box.
    Page(usize),
    LastPage,
    /// `:w` saves; `:w FILE` writes a copy to FILE and keeps working on this one. A FILE
    /// that exists is overwritten only with `:w!`.
    Write {
        path: Option<String>,
        force: bool,
    },
    /// `:saveas FILE`: saves as FILE, which the document then is (`!` to overwrite).
    SaveAs {
        path: String,
        force: bool,
    },
    Quit {
        force: bool,
    },
    WriteQuit,
    /// `:e FILE`; without a file, the open dialog.
    Edit(Option<String>),
    /// Zoom in percent.
    Zoom(f64),
    TabNext,
    TabPrev,
    NoHighlight,
    Help,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Consumed; nothing to do (a cancelled sequence, a key typed into the command line).
    Nothing,
    /// Arrow-key steps, counted.
    Scroll {
        dx: i32,
        dy: i32,
    },
    /// Half (`half`) or whole screens, negative up.
    Screen {
        half: bool,
        by: i32,
    },
    /// Page turns, negative back.
    Pages(i32),
    /// A page number as typed (`12G`): the page with that label when there is one.
    GotoPage(usize),
    FirstPage,
    LastPage,
    /// 0 top, 0.5 middle, 1 bottom of the current page.
    PagePart(f64),
    Zoom(Zoom),
    Search {
        query: String,
        forward: bool,
    },
    /// `n` (true) or `N`; `count` hits.
    SearchNext {
        forward: bool,
        count: usize,
    },
    Tab(Tab),
    SetMark(char),
    /// `exact` for `` ` ``: the position; else the top of the marked page.
    JumpMark {
        mark: char,
        exact: bool,
    },
    /// `''` or ``` `` ```: back to where the last jump started.
    JumpBack {
        exact: bool,
    },
    /// `Ctrl-o` (true) and `Ctrl-i`.
    JumpList {
        older: bool,
        count: usize,
    },
    Undo(usize),
    Redo(usize),
    Annotation {
        forward: bool,
        count: usize,
    },
    DeleteAnnotation,
    VisualStart {
        line: bool,
    },
    /// Switches between charwise and linewise visual mode.
    VisualKind {
        line: bool,
    },
    VisualMove {
        motion: Motion,
        count: usize,
    },
    /// `o`: the other end of the selection.
    VisualSwap,
    Yank,
    VisualExit,
    Ex(Ex),
}

/// What a key did.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Act(Action),
    /// Part of a longer sequence (a count, `g`, `z`, `m`...).
    Pending,
    /// Not a vim key here: the app's own handling applies.
    Pass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prefix {
    G,
    Z,
    Mark,
    Jump { exact: bool },
    Next,
    Prev,
}

impl Prefix {
    fn key(self) -> char {
        match self {
            Prefix::G => 'g',
            Prefix::Z => 'z',
            Prefix::Mark => 'm',
            Prefix::Jump { exact: false } => '\'',
            Prefix::Jump { exact: true } => '`',
            Prefix::Next => ']',
            Prefix::Prev => '[',
        }
    }
}

/// Counts beyond this are typos.
const MAX_COUNT: u32 = 99_999;
const HISTORY_LIMIT: usize = 100;

/// The caret is a byte offset on a character boundary in the editable text.
#[derive(Clone, Debug, Default)]
struct Prompt {
    text: String,
    caret: usize,
    history_index: Option<usize>,
    draft: (String, usize),
}

impl Prompt {
    fn previous(&self) -> usize {
        self.text[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.caret
            + self.text[self.caret..]
                .chars()
                .next()
                .map_or(0, char::len_utf8)
    }

    fn erase_to(&mut self, start: usize) {
        self.text.drain(start..self.caret);
        self.caret = start;
    }

    fn recall(&mut self, history: &VecDeque<String>, older: bool) {
        let i = self.history_index.unwrap_or(history.len());
        if older && i > 0 {
            if self.history_index.is_none() {
                self.draft = (self.text.clone(), self.caret);
            }
            self.history_index = Some(i - 1);
        } else if !older && i < history.len() {
            if i + 1 == history.len() {
                (self.text, self.caret) = std::mem::take(&mut self.draft);
                self.history_index = None;
                return;
            }
            self.history_index = Some(i + 1);
        } else {
            return;
        }
        if let Some(i) = self.history_index {
            self.text.clone_from(&history[i]);
            self.caret = self.text.len();
        }
    }
}

#[derive(Clone, Debug)]
pub struct Vim {
    mode: Mode,
    count: u32,
    prefix: Option<Prefix>,
    prompt: Prompt,
    commands: VecDeque<String>,
    searches: VecDeque<String>,
}

impl Default for Vim {
    fn default() -> Self {
        Vim {
            mode: Mode::Normal,
            count: 0,
            prefix: None,
            prompt: Prompt::default(),
            commands: VecDeque::new(),
            searches: VecDeque::new(),
        }
    }
}

impl Vim {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Back to normal mode with nothing pending (a new document, focus elsewhere).
    pub fn reset(&mut self) {
        self.mode = Mode::Normal;
        self.count = 0;
        self.prefix = None;
        self.prompt = Prompt::default();
    }

    /// The mode's name, shown over the pages.
    pub fn mode_label(&self) -> &'static str {
        match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Visual { line: false } => "VISUAL",
            Mode::Visual { line: true } => "V-LINE",
            Mode::Command => "COMMAND",
            Mode::Search { .. } => "SEARCH",
        }
    }

    /// The line being typed with its prompt (`:wq`, `/revenue`), or the keys of a
    /// sequence in progress (`5g`); empty when there is neither.
    pub fn status(&self) -> String {
        match self.mode {
            Mode::Command => format!(":{}", self.prompt.text),
            Mode::Search { forward } => {
                format!("{}{}", if forward { '/' } else { '?' }, self.prompt.text)
            }
            _ => {
                let mut s = String::new();
                if self.count > 0 {
                    s += &self.count.to_string();
                }
                if let Some(p) = self.prefix {
                    s.push(p.key());
                }
                s
            }
        }
    }

    /// The byte offset in `status`, including the prompt character.
    pub fn prompt_caret(&self) -> Option<usize> {
        matches!(self.mode, Mode::Command | Mode::Search { .. }).then_some(1 + self.prompt.caret)
    }

    /// The count typed so far, at least 1.
    fn take_count(&mut self) -> usize {
        let n = self.count.max(1) as usize;
        self.count = 0;
        n
    }

    /// Feeds one key. `ctrl` is the Control key itself (on macOS too, where the app's own
    /// shortcuts use Command).
    pub fn key(&mut self, key: Key, ctrl: bool) -> Outcome {
        match self.mode {
            Mode::Command | Mode::Search { .. } => self.line_key(key, ctrl),
            Mode::Normal | Mode::Visual { .. } => self.normal_key(key, ctrl),
        }
    }

    fn line_key(&mut self, key: Key, ctrl: bool) -> Outcome {
        let history = if self.mode == Mode::Command {
            &mut self.commands
        } else {
            &mut self.searches
        };
        let p = &mut self.prompt;
        match key {
            Key::Escape => self.reset(),
            Key::Enter => {
                let line = std::mem::take(&mut p.text);
                if !line.trim().is_empty() && history.back() != Some(&line) {
                    if history.len() == HISTORY_LIMIT {
                        history.pop_front();
                    }
                    history.push_back(line.clone());
                }
                let mode = self.mode;
                self.reset();
                return Outcome::Act(match mode {
                    Mode::Search { forward } => Action::Search {
                        query: line,
                        forward,
                    },
                    _ => parse_ex(&line).map_or(Action::Nothing, Action::Ex),
                });
            }
            Key::Left => p.caret = p.previous(),
            Key::Right => p.caret = p.next(),
            Key::Home => p.caret = 0,
            Key::End => p.caret = p.text.len(),
            Key::Up | Key::Down => p.recall(history, key == Key::Up),
            Key::Backspace if p.text.is_empty() => self.reset(),
            Key::Backspace => p.erase_to(p.previous()),
            Key::Delete => {
                p.text.drain(p.caret..p.next());
            }
            Key::Char('u') if ctrl => p.erase_to(0),
            Key::Char('w') if ctrl => {
                let word = p.text[..p.caret].trim_end();
                let start = word.trim_end_matches(|c: char| !c.is_whitespace()).len();
                p.erase_to(start);
            }
            Key::Char(c) if !ctrl && !c.is_control() => {
                p.text.insert(p.caret, c);
                p.caret += c.len_utf8();
            }
            _ => {}
        }
        Outcome::Act(Action::Nothing)
    }

    fn normal_key(&mut self, key: Key, ctrl: bool) -> Outcome {
        let visual = matches!(self.mode, Mode::Visual { .. });
        let c = match key {
            Key::Char(c) => c,
            Key::Escape => {
                let had = self.count > 0 || self.prefix.is_some();
                self.count = 0;
                self.prefix = None;
                if visual {
                    self.mode = Mode::Normal;
                    return Outcome::Act(Action::VisualExit);
                }
                return if had {
                    Outcome::Act(Action::Nothing)
                } else {
                    Outcome::Pass
                };
            }
            _ => {
                self.count = 0;
                self.prefix = None;
                return Outcome::Pass;
            }
        };
        if ctrl {
            self.prefix = None;
            return self.ctrl_key(c);
        }
        if let Some(p) = self.prefix.take() {
            return Outcome::Act(self.after_prefix(p, c, visual));
        }
        if c.is_ascii_digit() && (c != '0' || self.count > 0) {
            let d = c.to_digit(10).unwrap_or(0);
            self.count = (self.count * 10 + d).min(MAX_COUNT);
            return Outcome::Pending;
        }
        if visual {
            return self.visual_key(c);
        }
        let prefix = match c {
            'g' => Some(Prefix::G),
            'z' => Some(Prefix::Z),
            'm' => Some(Prefix::Mark),
            '\'' => Some(Prefix::Jump { exact: false }),
            '`' => Some(Prefix::Jump { exact: true }),
            ']' => Some(Prefix::Next),
            '[' => Some(Prefix::Prev),
            _ => None,
        };
        if let Some(p) = prefix {
            self.prefix = Some(p);
            return Outcome::Pending;
        }
        let had_count = self.count > 0;
        let n = self.take_count();
        let steps = n as i32;
        let action = match c {
            'j' => Action::Scroll { dx: 0, dy: steps },
            'k' => Action::Scroll { dx: 0, dy: -steps },
            'h' => Action::Scroll { dx: -steps, dy: 0 },
            'l' => Action::Scroll { dx: steps, dy: 0 },
            'J' => Action::Pages(steps),
            'K' => Action::Pages(-steps),
            'G' if had_count => Action::GotoPage(n),
            'G' => Action::LastPage,
            'H' => Action::PagePart(0.0),
            'M' => Action::PagePart(0.5),
            'L' => Action::PagePart(1.0),
            'n' => Action::SearchNext {
                forward: true,
                count: n,
            },
            'N' => Action::SearchNext {
                forward: false,
                count: n,
            },
            'u' => Action::Undo(n),
            'x' => Action::DeleteAnnotation,
            '/' | '?' => {
                self.mode = Mode::Search { forward: c == '/' };
                Action::Nothing
            }
            ':' => {
                self.mode = Mode::Command;
                Action::Nothing
            }
            'v' | 'V' => {
                let line = c == 'V';
                self.mode = Mode::Visual { line };
                Action::VisualStart { line }
            }
            // Space, + and - keep the app's meaning.
            _ => return Outcome::Pass,
        };
        Outcome::Act(action)
    }

    fn ctrl_key(&mut self, c: char) -> Outcome {
        let n = self.take_count();
        let steps = n as i32;
        Outcome::Act(match c.to_ascii_lowercase() {
            'd' => Action::Screen {
                half: true,
                by: steps,
            },
            'u' => Action::Screen {
                half: true,
                by: -steps,
            },
            'f' => Action::Screen {
                half: false,
                by: steps,
            },
            'b' => Action::Screen {
                half: false,
                by: -steps,
            },
            'e' => Action::Scroll { dx: 0, dy: steps },
            'y' => Action::Scroll { dx: 0, dy: -steps },
            'o' => Action::JumpList {
                older: true,
                count: n,
            },
            'i' => Action::JumpList {
                older: false,
                count: n,
            },
            'r' => Action::Redo(n),
            _ => return Outcome::Pass,
        })
    }

    fn after_prefix(&mut self, p: Prefix, c: char, visual: bool) -> Action {
        let had_count = self.count > 0;
        let n = self.take_count();
        match (p, c) {
            (Prefix::G, 'g') if visual => Action::VisualMove {
                motion: Motion::PageStart,
                count: 1,
            },
            (Prefix::G, 'g') if had_count => Action::GotoPage(n),
            (Prefix::G, 'g') => Action::FirstPage,
            (Prefix::G, 't') if !visual && had_count => Action::Tab(Tab::Go(n - 1)),
            (Prefix::G, 't') if !visual => Action::Tab(Tab::Next(1)),
            (Prefix::G, 'T') if !visual => Action::Tab(Tab::Prev(n)),
            (Prefix::Z, 'i') => Action::Zoom(Zoom::In),
            (Prefix::Z, 'o') => Action::Zoom(Zoom::Out),
            (Prefix::Z, 'w') => Action::Zoom(Zoom::FitWidth),
            (Prefix::Z, 'p') => Action::Zoom(Zoom::FitPage),
            (Prefix::Z, '0') => Action::Zoom(Zoom::Actual),
            (Prefix::Z, 'z') => Action::PagePart(0.5),
            (Prefix::Mark, c) if c.is_ascii_alphabetic() => Action::SetMark(c),
            (Prefix::Jump { exact }, c) if c.is_ascii_alphabetic() => {
                Action::JumpMark { mark: c, exact }
            }
            (Prefix::Jump { .. }, '\'') => Action::JumpBack { exact: false },
            (Prefix::Jump { .. }, '`') => Action::JumpBack { exact: true },
            (Prefix::Next, 'a') => Action::Annotation {
                forward: true,
                count: n,
            },
            (Prefix::Prev, 'a') => Action::Annotation {
                forward: false,
                count: n,
            },
            _ => Action::Nothing,
        }
    }

    fn visual_key(&mut self, c: char) -> Outcome {
        let Mode::Visual { line } = self.mode else {
            return Outcome::Pass;
        };
        if c == 'g' {
            self.prefix = Some(Prefix::G);
            return Outcome::Pending;
        }
        let n = self.take_count();
        let motion = match c {
            'h' => Some(Motion::Left),
            'l' => Some(Motion::Right),
            'j' => Some(Motion::Down),
            'k' => Some(Motion::Up),
            'w' | 'W' => Some(Motion::WordStart),
            'b' | 'B' => Some(Motion::WordBack),
            'e' | 'E' => Some(Motion::WordEnd),
            '0' | '^' => Some(Motion::LineStart),
            '$' => Some(Motion::LineEnd),
            'G' => Some(Motion::PageEnd),
            _ => None,
        };
        if let Some(motion) = motion {
            return Outcome::Act(Action::VisualMove { motion, count: n });
        }
        Outcome::Act(match c {
            'o' | 'O' => Action::VisualSwap,
            'y' | 'Y' => {
                self.mode = Mode::Normal;
                Action::Yank
            }
            'v' | 'V' => {
                let want = c == 'V';
                if want == line {
                    self.mode = Mode::Normal;
                    Action::VisualExit
                } else {
                    self.mode = Mode::Visual { line: want };
                    Action::VisualKind { line: want }
                }
            }
            _ => Action::Nothing,
        })
    }
}

/// Parses a `:` command line (without the colon); `None` for an empty one.
pub fn parse_ex(line: &str) -> Option<Ex> {
    let line = line.trim().trim_start_matches(':').trim();
    if line.is_empty() {
        return None;
    }
    if line == "$" {
        return Some(Ex::LastPage);
    }
    if let Ok(n) = line.parse::<usize>() {
        return Some(Ex::Page(n));
    }
    let (cmd, arg) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim()),
        None => (line, ""),
    };
    let arg = (!arg.is_empty()).then(|| arg.to_owned());
    Some(match cmd {
        "w" | "write" | "w!" | "write!" => Ex::Write {
            force: cmd.ends_with('!'),
            path: arg,
        },
        "sav" | "saveas" | "sav!" | "saveas!" => match arg {
            Some(path) => Ex::SaveAs {
                force: cmd.ends_with('!'),
                path,
            },
            None => Ex::Unknown(line.to_owned()),
        },
        "q" | "quit" | "clo" | "close" => Ex::Quit { force: false },
        "q!" | "quit!" | "clo!" | "close!" => Ex::Quit { force: true },
        "wq" | "x" | "xit" | "exit" => Ex::WriteQuit,
        "e" | "edit" | "o" | "open" | "tabe" | "tabedit" | "tabnew" => Ex::Edit(arg),
        "tabn" | "tabnext" | "bn" | "bnext" => Ex::TabNext,
        "tabp" | "tabprevious" | "tabN" | "tabNext" | "bp" | "bprevious" => Ex::TabPrev,
        "noh" | "nohlsearch" => Ex::NoHighlight,
        "h" | "help" => Ex::Help,
        "se" | "set" => match arg.as_deref().and_then(parse_zoom) {
            Some(z) => Ex::Zoom(z),
            None => Ex::Unknown(line.to_owned()),
        },
        _ => Ex::Unknown(line.to_owned()),
    })
}

/// `zoom=150`, `zoom=150%` or `zoom 150`: the percentage.
fn parse_zoom(arg: &str) -> Option<f64> {
    let rest = arg.strip_prefix("zoom")?;
    let v = rest
        .trim_start_matches(['=', ' '])
        .trim_end_matches('%')
        .trim();
    v.parse::<f64>().ok().filter(|z| z.is_finite() && *z > 0.0)
}

/// Where a search starts: the top of the view (vim's cursor) and which way it goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchFrom {
    /// The page at the top of the view, and the displayed point of it there.
    pub page: usize,
    pub v: f64,
    pub forward: bool,
    /// The page the running search started on (it goes to the end, then from the start);
    /// `None` when not known, and then only a finished search can tell.
    pub start: Option<usize>,
}

/// Where a search lands, as in vim: the first hit at or below the position, or going
/// back the last one above it, wrapping around the document (then `true`). `top` gives
/// the top of a hit on the position's page in displayed points (its boxes worked out).
/// `None` while the hits found so far cannot tell.
pub fn search_target(
    hits: &[Hit],
    done: bool,
    from: SearchFrom,
    top: impl Fn(&Hit) -> Option<f64>,
) -> Option<(usize, bool)> {
    let n = hits.len();
    // A hit right at the top of the view is below it, and on screen.
    let below = |h: &Hit| {
        h.page > from.page || (h.page == from.page && top(h).is_none_or(|t| t >= from.v - 1.0))
    };
    if from.forward {
        // Pages are searched in order from the start page down, so a hit found below the
        // position is the first one there is once the position is on that stretch.
        let settled = |i: usize| {
            done || from
                .start
                .is_some_and(|s| s <= from.page && hits[i].page >= from.page)
        };
        match hits.iter().position(below) {
            Some(i) if settled(i) => Some((i, false)),
            Some(_) => None,
            None if done && n > 0 => Some((0, true)),
            None => None,
        }
    } else {
        // A page's hits arrive together: one above the position on its own page settles
        // it; one on an earlier page only once every page has been searched.
        match hits.iter().rposition(|h| !below(h)) {
            Some(i) if done || hits[i].page == from.page => Some((i, false)),
            Some(_) => None,
            None if done && n > 0 => Some((n - 1, true)),
            None => None,
        }
    }
}

/// A visual-mode selection on one page: character indices, `cursor` the end that moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visual {
    pub page: usize,
    pub anchor: usize,
    pub cursor: usize,
    pub line: bool,
}

impl Visual {
    /// The selected characters: both ends included, whole lines when linewise.
    pub fn range(&self, text: &TextPage) -> Range<usize> {
        let n = text.chars.len();
        if n == 0 {
            return 0..0;
        }
        let (a, b) = (self.anchor.min(self.cursor), self.anchor.max(self.cursor));
        if self.line {
            let first = line_of(text, a).map_or(a, |l| l.0);
            let last = line_of(text, b).map_or(b + 1, |l| l.1);
            first..last.min(n)
        } else {
            a..(b + 1).min(n)
        }
    }
}

/// The character range of the line holding character `i`.
fn line_of(text: &TextPage, i: usize) -> Option<(usize, usize)> {
    let k = line_index(text, i)?;
    let l = &text.lines[k];
    Some((l.chars.0 as usize, l.chars.1 as usize))
}

/// Index in [`TextPage::lines`] of the line holding character `i` (the last line that
/// starts at or before it).
fn line_index(text: &TextPage, i: usize) -> Option<usize> {
    let k = text
        .lines
        .partition_point(|l| (l.chars.0 as usize) <= i)
        .checked_sub(1)?;
    Some(k)
}

/// The horizontal middle of character `i`.
fn char_x(text: &TextPage, i: usize) -> f64 {
    let b = text.chars[i].bbox();
    f64::from(b.x0 + b.x1) / 2.0
}

/// Where `motion` takes the cursor at character `from`, `count` times.
pub fn motion(text: &TextPage, from: usize, motion: Motion, count: usize) -> usize {
    let n = text.chars.len();
    if n == 0 {
        return 0;
    }
    let mut at = from.min(n - 1);
    for _ in 0..count.max(1) {
        let next = step(text, at, motion);
        if next == at {
            break;
        }
        at = next;
    }
    at
}

fn step(text: &TextPage, at: usize, motion: Motion) -> usize {
    let n = text.chars.len();
    let words = &text.words;
    let start = |w: &pagebara_content::TextWord| w.chars.0 as usize;
    let end = |w: &pagebara_content::TextWord| (w.chars.1 as usize).saturating_sub(1);
    match motion {
        Motion::Left => at.saturating_sub(1),
        Motion::Right => (at + 1).min(n - 1),
        Motion::WordStart => words.iter().map(start).find(|&s| s > at).unwrap_or(at),
        Motion::WordBack => words
            .iter()
            .rev()
            .map(start)
            .find(|&s| s < at)
            .unwrap_or(at),
        Motion::WordEnd => words.iter().map(end).find(|&e| e > at).unwrap_or(at),
        Motion::LineStart => line_of(text, at).map_or(at, |l| l.0),
        Motion::LineEnd => line_of(text, at).map_or(at, |l| l.1.saturating_sub(1)),
        Motion::PageStart => 0,
        Motion::PageEnd => n - 1,
        Motion::Down | Motion::Up => {
            let Some(k) = line_index(text, at) else {
                return at;
            };
            let target = if motion == Motion::Down {
                k + 1
            } else {
                match k.checked_sub(1) {
                    Some(t) => t,
                    None => return at,
                }
            };
            let Some(l) = text.lines.get(target) else {
                return at;
            };
            let (a, b) = (l.chars.0 as usize, (l.chars.1 as usize).min(n));
            if a >= b {
                return at;
            }
            let x = char_x(text, at);
            (a..b)
                .min_by(|&i, &j| {
                    (char_x(text, i) - x)
                        .abs()
                        .total_cmp(&(char_x(text, j) - x).abs())
                })
                .unwrap_or(a)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagebara_content::{Point, Rect, TextChar, TextLine, TextWord};

    fn keys(v: &mut Vim, s: &str) -> Vec<Outcome> {
        s.chars().map(|c| v.key(Key::Char(c), false)).collect()
    }

    fn last(v: &mut Vim, s: &str) -> Outcome {
        keys(v, s).pop().unwrap()
    }

    fn act(a: Action) -> Outcome {
        Outcome::Act(a)
    }

    #[test]
    fn counts_and_scrolling() {
        let mut v = Vim::default();
        assert_eq!(last(&mut v, "j"), act(Action::Scroll { dx: 0, dy: 1 }));
        assert_eq!(keys(&mut v, "12")[1], Outcome::Pending);
        assert_eq!(v.status(), "12");
        assert_eq!(last(&mut v, "k"), act(Action::Scroll { dx: 0, dy: -12 }));
        assert_eq!(v.status(), "");
        assert_eq!(last(&mut v, "3l"), act(Action::Scroll { dx: 3, dy: 0 }));
        assert_eq!(
            v.key(Key::Char('d'), true),
            act(Action::Screen { half: true, by: 1 })
        );
        keys(&mut v, "2");
        assert_eq!(
            v.key(Key::Char('b'), true),
            act(Action::Screen {
                half: false,
                by: -2
            })
        );
        // Shortcuts the app owns pass through.
        assert_eq!(v.key(Key::Char('s'), true), Outcome::Pass);
        assert_eq!(v.key(Key::Other, false), Outcome::Pass);
        assert_eq!(last(&mut v, " "), Outcome::Pass);
    }

    #[test]
    fn pages_and_tabs() {
        let mut v = Vim::default();
        assert_eq!(last(&mut v, "gg"), act(Action::FirstPage));
        assert_eq!(last(&mut v, "G"), act(Action::LastPage));
        assert_eq!(last(&mut v, "12G"), act(Action::GotoPage(12)));
        assert_eq!(last(&mut v, "7gg"), act(Action::GotoPage(7)));
        assert_eq!(last(&mut v, "2J"), act(Action::Pages(2)));
        assert_eq!(last(&mut v, "gt"), act(Action::Tab(Tab::Next(1))));
        assert_eq!(last(&mut v, "3gt"), act(Action::Tab(Tab::Go(2))));
        assert_eq!(last(&mut v, "gT"), act(Action::Tab(Tab::Prev(1))));
        assert_eq!(last(&mut v, "L"), act(Action::PagePart(1.0)));
        assert_eq!(last(&mut v, "zw"), act(Action::Zoom(Zoom::FitWidth)));
        assert_eq!(last(&mut v, "z0"), act(Action::Zoom(Zoom::Actual)));
        // A count of zero is not a count: `0` alone passes.
        assert_eq!(last(&mut v, "0"), Outcome::Pass);
        // Escape cancels a pending sequence, and passes when nothing is pending.
        keys(&mut v, "5g");
        assert_eq!(v.status(), "5g");
        assert_eq!(v.key(Key::Escape, false), act(Action::Nothing));
        assert_eq!(v.status(), "");
        assert_eq!(v.key(Key::Escape, false), Outcome::Pass);
    }

    #[test]
    fn marks_jumps_and_annotations() {
        let mut v = Vim::default();
        assert_eq!(last(&mut v, "ma"), act(Action::SetMark('a')));
        assert_eq!(
            last(&mut v, "'a"),
            act(Action::JumpMark {
                mark: 'a',
                exact: false
            })
        );
        assert_eq!(
            last(&mut v, "`a"),
            act(Action::JumpMark {
                mark: 'a',
                exact: true
            })
        );
        assert_eq!(last(&mut v, "''"), act(Action::JumpBack { exact: false }));
        assert_eq!(last(&mut v, "m1"), act(Action::Nothing));
        assert_eq!(
            v.key(Key::Char('o'), true),
            act(Action::JumpList {
                older: true,
                count: 1
            })
        );
        assert_eq!(
            last(&mut v, "2]a"),
            act(Action::Annotation {
                forward: true,
                count: 2
            })
        );
        assert_eq!(last(&mut v, "x"), act(Action::DeleteAnnotation));
        assert_eq!(last(&mut v, "3u"), act(Action::Undo(3)));
        assert_eq!(v.key(Key::Char('r'), true), act(Action::Redo(1)));
    }

    #[test]
    fn search_and_command_line() {
        let mut v = Vim::default();
        keys(&mut v, "/reve");
        assert_eq!(v.mode(), Mode::Search { forward: true });
        assert_eq!(v.status(), "/reve");
        v.key(Key::Backspace, false);
        keys(&mut v, "enue");
        assert_eq!(
            v.key(Key::Enter, false),
            act(Action::Search {
                query: "revenue".into(),
                forward: true
            })
        );
        assert_eq!(v.mode(), Mode::Normal);
        assert_eq!(
            last(&mut v, "2N"),
            act(Action::SearchNext {
                forward: false,
                count: 2
            })
        );
        keys(&mut v, "?");
        assert_eq!(
            v.key(Key::Enter, false),
            act(Action::Search {
                query: String::new(),
                forward: false
            })
        );
        keys(&mut v, ":wq");
        assert_eq!(v.status(), ":wq");
        assert_eq!(v.key(Key::Enter, false), act(Action::Ex(Ex::WriteQuit)));
        // Backspace on an empty line leaves the command line, as in vim.
        keys(&mut v, ":");
        v.key(Key::Backspace, false);
        assert_eq!(v.mode(), Mode::Normal);
        keys(&mut v, ":set zoom");
        v.key(Key::Char('w'), true);
        assert_eq!(v.status(), ":set ");
        v.key(Key::Escape, false);
        assert_eq!(v.mode(), Mode::Normal);
    }

    #[test]
    fn edit_a_prompt_at_character_boundaries() {
        let mut v = Vim::default();
        keys(&mut v, "/a猫ы🙂");
        v.key(Key::Left, false);
        v.key(Key::Backspace, false);
        keys(&mut v, "ø");
        assert_eq!(v.status(), "/a猫ø🙂");
        assert_eq!(v.prompt_caret(), Some("/a猫ø".len()));
        v.key(Key::Home, false);
        v.key(Key::Left, false);
        v.key(Key::Backspace, false);
        assert_eq!(v.prompt_caret(), Some(1));
        v.key(Key::Right, false);
        v.key(Key::Delete, false);
        keys(&mut v, " β ");
        v.key(Key::Char('w'), true);
        assert_eq!(v.status(), "/a ø🙂");
        v.key(Key::Char('u'), true);
        assert_eq!(v.status(), "/ø🙂");
        v.key(Key::End, false);
        v.key(Key::Delete, false);
        v.key(Key::Right, false);
        assert_eq!(v.prompt_caret(), Some("/ø🙂".len()));
        v.key(Key::Escape, false);
        assert_eq!(v.prompt_caret(), None);
    }

    #[test]
    fn recall_history_and_restore_the_draft_and_caret() {
        let mut v = Vim::default();
        for entry in [":12", ":w", ":w", "/one", "?two", "?"] {
            keys(&mut v, entry);
            v.key(Key::Enter, false);
        }
        keys(&mut v, ":cancelled");
        v.key(Key::Escape, false);
        v.reset();
        keys(&mut v, ":draft");
        v.key(Key::Left, false);
        v.key(Key::Up, false);
        assert_eq!(v.status(), ":w");
        keys(&mut v, "q");
        v.key(Key::Up, false);
        v.key(Key::Up, false);
        assert_eq!(v.status(), ":12");
        v.key(Key::Down, false);
        assert_eq!(v.status(), ":w");
        v.key(Key::Down, false);
        v.key(Key::Down, false);
        assert_eq!(v.status(), ":draft");
        assert_eq!(v.prompt_caret(), Some(":draf".len()));
        v.key(Key::Escape, false);
        keys(&mut v, "/");
        v.key(Key::Up, false);
        assert_eq!(v.status(), "/two");
        v.key(Key::Up, false);
        assert_eq!(v.status(), "/one");
        v.key(Key::Enter, false);
        keys(&mut v, "?");
        v.key(Key::Up, false);
        assert_eq!(v.status(), "?one");
    }

    #[test]
    fn history_keeps_the_most_recent_entries() {
        let mut v = Vim::default();
        for n in 0..=HISTORY_LIMIT {
            keys(&mut v, &format!(":{n}"));
            v.key(Key::Enter, false);
        }
        keys(&mut v, ":");
        for _ in 0..=HISTORY_LIMIT {
            v.key(Key::Up, false);
        }
        assert_eq!(v.status(), ":1");
    }

    #[test]
    fn ex_commands() {
        assert_eq!(parse_ex(""), None);
        assert_eq!(parse_ex("42"), Some(Ex::Page(42)));
        assert_eq!(parse_ex(" $ "), Some(Ex::LastPage));
        assert_eq!(
            parse_ex("w"),
            Some(Ex::Write {
                path: None,
                force: false
            })
        );
        assert_eq!(
            parse_ex("w ~/copy.pdf"),
            Some(Ex::Write {
                path: Some("~/copy.pdf".into()),
                force: false
            })
        );
        assert_eq!(
            parse_ex("w! copy.pdf"),
            Some(Ex::Write {
                path: Some("copy.pdf".into()),
                force: true
            })
        );
        assert_eq!(
            parse_ex("saveas! b.pdf"),
            Some(Ex::SaveAs {
                path: "b.pdf".into(),
                force: true
            })
        );
        assert_eq!(parse_ex("sav"), Some(Ex::Unknown("sav".into())));
        assert_eq!(parse_ex("q!"), Some(Ex::Quit { force: true }));
        assert_eq!(parse_ex("x"), Some(Ex::WriteQuit));
        assert_eq!(parse_ex("e"), Some(Ex::Edit(None)));
        assert_eq!(
            parse_ex("tabe a b.pdf"),
            Some(Ex::Edit(Some("a b.pdf".into())))
        );
        assert_eq!(parse_ex("set zoom=150%"), Some(Ex::Zoom(150.0)));
        assert_eq!(parse_ex("set zoom 80"), Some(Ex::Zoom(80.0)));
        assert_eq!(
            parse_ex("set zoom=-3"),
            Some(Ex::Unknown("set zoom=-3".into()))
        );
        assert_eq!(parse_ex("tabN"), Some(Ex::TabPrev));
        assert_eq!(parse_ex("frob"), Some(Ex::Unknown("frob".into())));
    }

    #[test]
    fn search_targets() {
        // Hits on pages 1, 3 (at 100 and 500 points down) and 6.
        let hit = |page: usize, v: f32| Hit {
            page,
            boxes: vec![[0.0, v, 10.0, v + 10.0]],
            boxed: true,
        };
        let hits = [hit(1, 300.0), hit(3, 100.0), hit(3, 500.0), hit(6, 50.0)];
        let top = |h: &Hit| h.boxes.first().map(|b| f64::from(b[1]));
        let at = |page, v, forward, start| SearchFrom {
            page,
            v,
            forward,
            start,
        };
        let go = |done, from| search_target(&hits, done, from, top);
        // Down from the middle of page 3: its second hit; back: its first.
        assert_eq!(go(true, at(3, 300.0, true, None)), Some((2, false)));
        assert_eq!(go(true, at(3, 300.0, false, None)), Some((1, false)));
        // A hit at the top of the view counts as below it.
        assert_eq!(go(true, at(3, 100.0, true, None)), Some((1, false)));
        // Past the last hit, down wraps to the first; above the first, back wraps.
        assert_eq!(go(true, at(7, 0.0, true, None)), Some((0, true)));
        assert_eq!(go(true, at(0, 0.0, false, None)), Some((3, true)));
        // Still searching: down is settled by a hit on the stretch searched from the
        // start page; back only by a hit above the position on its own page.
        assert_eq!(go(false, at(3, 300.0, true, Some(3))), Some((2, false)));
        assert_eq!(go(false, at(3, 300.0, true, None)), None);
        assert_eq!(go(false, at(3, 300.0, false, Some(3))), Some((1, false)));
        assert_eq!(go(false, at(4, 0.0, false, Some(4))), None);
        assert_eq!(go(false, at(7, 0.0, true, Some(7))), None);
        assert_eq!(search_target(&[], true, at(0, 0.0, true, None), top), None);
    }

    #[test]
    fn visual_mode_keys() {
        let mut v = Vim::default();
        assert_eq!(last(&mut v, "v"), act(Action::VisualStart { line: false }));
        assert_eq!(v.mode_label(), "VISUAL");
        assert_eq!(
            last(&mut v, "3w"),
            act(Action::VisualMove {
                motion: Motion::WordStart,
                count: 3
            })
        );
        assert_eq!(
            last(&mut v, "0"),
            act(Action::VisualMove {
                motion: Motion::LineStart,
                count: 1
            })
        );
        assert_eq!(
            last(&mut v, "gg"),
            act(Action::VisualMove {
                motion: Motion::PageStart,
                count: 1
            })
        );
        assert_eq!(last(&mut v, "V"), act(Action::VisualKind { line: true }));
        assert_eq!(v.mode_label(), "V-LINE");
        assert_eq!(last(&mut v, "y"), act(Action::Yank));
        assert_eq!(v.mode(), Mode::Normal);
        keys(&mut v, "v");
        assert_eq!(v.key(Key::Escape, false), act(Action::VisualExit));
        assert_eq!(v.mode(), Mode::Normal);
    }

    /// Two lines: "ab cd" over "efg hi", 10 points per character.
    fn page() -> TextPage {
        let rows = ["ab cd", "efg hi"];
        let mut p = TextPage::default();
        for (r, row) in rows.iter().enumerate() {
            let y = 700.0 - 20.0 * r as f32;
            let first = p.chars.len() as u32;
            let first_word = p.words.len() as u32;
            let mut word: Option<u32> = None;
            for (i, ch) in row.chars().enumerate() {
                let x = 10.0 * i as f32;
                let at = p.text.len() as u32;
                p.text.push(ch);
                p.chars.push(TextChar {
                    text: (at, at + 1),
                    quad: [
                        Point::new(x, y),
                        Point::new(x + 10.0, y),
                        Point::new(x + 10.0, y + 10.0),
                        Point::new(x, y + 10.0),
                    ],
                    glyph: 0,
                });
                let idx = p.chars.len() as u32 - 1;
                if ch == ' ' {
                    if let Some(s) = word.take() {
                        p.words.push(TextWord {
                            chars: (s, idx),
                            bbox: Rect::new(0.0, 0.0, 0.0, 0.0),
                        });
                    }
                } else if word.is_none() {
                    word = Some(idx);
                }
            }
            if let Some(s) = word {
                p.words.push(TextWord {
                    chars: (s, p.chars.len() as u32),
                    bbox: Rect::new(0.0, 0.0, 0.0, 0.0),
                });
            }
            p.text.push('\n');
            p.lines.push(TextLine {
                chars: (first, p.chars.len() as u32),
                words: (first_word, p.words.len() as u32),
                bbox: Rect::new(0.0, y, 60.0, y + 10.0),
                vertical: false,
                rtl: false,
            });
        }
        p
    }

    #[test]
    fn motions_over_text() {
        let t = page();
        // Characters: a0 b1 _2 c3 d4 | e5 f6 g7 _8 h9 i10
        assert_eq!(motion(&t, 0, Motion::WordStart, 1), 3);
        assert_eq!(motion(&t, 0, Motion::WordStart, 2), 5);
        assert_eq!(motion(&t, 0, Motion::WordEnd, 1), 1);
        assert_eq!(motion(&t, 6, Motion::WordBack, 1), 5);
        assert_eq!(motion(&t, 5, Motion::WordBack, 1), 3);
        assert_eq!(motion(&t, 7, Motion::LineStart, 1), 5);
        assert_eq!(motion(&t, 1, Motion::LineEnd, 1), 4);
        assert_eq!(motion(&t, 3, Motion::Down, 1), 8);
        assert_eq!(motion(&t, 9, Motion::Up, 1), 4);
        assert_eq!(motion(&t, 1, Motion::Up, 1), 1);
        assert_eq!(motion(&t, 4, Motion::Right, 10), 10);
        assert_eq!(motion(&t, 3, Motion::PageEnd, 1), 10);
        let sel = Visual {
            page: 0,
            anchor: 4,
            cursor: 1,
            line: false,
        };
        assert_eq!(sel.range(&t), 1..5);
        let lines = Visual { line: true, ..sel };
        assert_eq!(lines.range(&t), 0..5);
        let both = Visual { cursor: 6, ..lines };
        assert_eq!(both.range(&t), 0..11);
    }
}
