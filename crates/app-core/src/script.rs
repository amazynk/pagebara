//! Automation scripts (`--script FILE`) for UI tests and demo recordings: a timed list of
//! actions, one per line.
//!
//! ```text
//! # comments and blank lines are ignored; $NAME expands environment variables
//! open "$SAMPLES/lease.pdf"
//! cursor on                       # draw a synthetic pointer
//! click "Highlight" 600           # a target, then how long the motion takes (ms)
//! drag page 1 72,120 page 1 300,120 900
//! stroke page 1 100,400 140,380 180,420 220,390 1200
//! stroke "Signature canvas" -100,0 0,-20 100,10   # offsets from the target's centre
//! type "Hello"
//! key ctrl+z
//! key ctrl+right|alt+right          # Windows/Linux chord | macOS chord
//! key physical-ctrl+tab             # Control itself on every desktop
//! scroll 600 800
//! zoom 150 500
//! wait 300
//! screenshot organize.png
//! assert annotations == 1
//! await ocr.running == false 60000   # an assert that waits up to that long (ms)
//! ```
//!
//! Targets: `X,Y` (window pixels), `page N X,Y` (points from the top left of page N as
//! shown), `"accessible label"` (optionally `"label"@2` for the second match), `#element-id`
//! `text P "words"` and `text-end P "words"` (where those words start or end on page P),
//! `annot P N` (the centre of the Nth annotation of page P, links included), `object P N`
//! (the centre of the Nth editable object of page P, in painting order), `handle NAME` (a
//! handle of the Edit mode's selection frame: `tl`, `t`, `tr`, `r`, `br`, `b`, `bl`, `l`,
//! `rotate`), `misspelling N` (the Nth misspelled word of the text being typed) and `here`
//! (where the pointer is). `rclick` clicks with the right button.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Window(f32, f32),
    Page {
        page: usize,
        x: f64,
        y: f64,
    },
    Label {
        label: String,
        nth: usize,
    },
    /// The start (or, with `end`, the end) of the first occurrence of `text` on a page.
    Text {
        page: usize,
        text: String,
        end: bool,
    },
    /// The centre of annotation `n` (1-based, in `/Annots` order, links included) of a page.
    Annot {
        page: usize,
        n: usize,
    },
    /// The centre of editable object `n` (1-based, in painting order, objects of the page
    /// itself) of a page.
    Object {
        page: usize,
        n: usize,
    },
    /// A handle of the selection frame in Edit mode.
    Handle(String),
    Id(String),
    /// The middle of misspelled word `n` (1-based) of the text being typed.
    Misspelling(usize),
    Here,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Open(String),
    Wait,
    Cursor(bool),
    Tool(String),
    Move(Target),
    /// A click with the right (secondary) button.
    RightClick(Target),
    Click {
        target: Target,
        count: u8,
    },
    Drag(Target, Target),
    /// A drag that keeps the button down at the end (for a screenshot mid-drag).
    Hold(Target, Target),
    /// Lets go of the button a [`Action::Hold`] kept down.
    Release,
    /// A pressed pointer moving through points on one page.
    Stroke {
        page: usize,
        points: Vec<(f64, f64)>,
    },
    /// A pressed pointer moving through offsets from the centre of a target.
    StrokeAt {
        target: Target,
        points: Vec<(f64, f64)>,
    },
    Type(String),
    Key(String),
    Scroll(f64),
    Zoom(f64),
    Page(usize),
    Screenshot(String),
    Assert {
        key: String,
        op: Op,
        value: String,
    },
    /// An app command by name: save, undo, redo, organize, find, close-tab...
    Call(String, Vec<String>),
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    NotContains,
}

impl Op {
    fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "==" | "=" => Op::Eq,
            "!=" => Op::Ne,
            "<" => Op::Lt,
            "<=" => Op::Le,
            ">" => Op::Gt,
            ">=" => Op::Ge,
            "contains" => Op::Contains,
            "!contains" => Op::NotContains,
            _ => return None,
        })
    }

    /// Compares numerically when both sides are numbers, else as text.
    pub fn holds(self, actual: &str, expected: &str) -> bool {
        if let (Ok(a), Ok(b)) = (actual.parse::<f64>(), expected.parse::<f64>()) {
            return match self {
                Op::Eq => a == b,
                Op::Ne => a != b,
                Op::Lt => a < b,
                Op::Le => a <= b,
                Op::Gt => a > b,
                Op::Ge => a >= b,
                Op::Contains => actual.contains(expected),
                Op::NotContains => !actual.contains(expected),
            };
        }
        match self {
            Op::Eq => actual == expected,
            Op::Ne => actual != expected,
            Op::Contains => actual.contains(expected),
            Op::NotContains => !actual.contains(expected),
            Op::Lt => actual < expected,
            Op::Le => actual <= expected,
            Op::Gt => actual > expected,
            Op::Ge => actual >= expected,
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Op::Eq => "==",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Contains => "contains",
            Op::NotContains => "!contains",
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub action: Action,
    /// How long the action takes (motion, scrolling, waiting), in milliseconds.
    pub ms: u64,
    /// 1-based line in the script, for messages.
    pub line: usize,
}

/// Splits a line into words; double quotes group (and `\"` escapes inside them).
fn words(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(match n {
                        'n' => '\n',
                        n => n,
                    });
                }
            }
            '"' => {
                quoted = !quoted;
                any = true;
                if !quoted {
                    // Keep a marker so labels stay distinguishable from coordinates.
                    cur.insert(0, '\u{1}');
                }
            }
            '#' if !quoted && !any && chars.peek().is_none_or(|n| n.is_whitespace()) => break,
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quoted {
        return Err("unclosed quote".into());
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

fn is_quoted(w: &str) -> bool {
    w.starts_with('\u{1}')
}

fn unquote(w: &str) -> &str {
    w.strip_prefix('\u{1}').unwrap_or(w)
}

/// Expands `$NAME` and `${NAME}` from `env`.
pub fn expand(s: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, tail) = if let Some(b) = after.strip_prefix('{') {
            match b.find('}') {
                Some(j) => (&b[..j], &b[j + 1..]),
                None => (b, ""),
            }
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if name.is_empty() {
            out.push('$');
        } else {
            out.push_str(&env(name).unwrap_or_default());
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn pair(s: &str) -> Option<(f64, f64)> {
    let (a, b) = s.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// Reads a target at `w[i..]`; returns it and the words used.
fn target(w: &[String]) -> Result<(Target, usize), String> {
    let first = w.first().ok_or("missing target")?;
    if let Some((label, nth)) = first.rsplit_once('@')
        && is_quoted(label)
        && let Ok(n) = nth.parse::<usize>()
    {
        return Ok((
            Target::Label {
                label: unquote(label).into(),
                nth: n.saturating_sub(1),
            },
            1,
        ));
    }
    if is_quoted(first) {
        return Ok((
            Target::Label {
                label: unquote(first).into(),
                nth: 0,
            },
            1,
        ));
    }
    if let Some(id) = first.strip_prefix('#') {
        return Ok((Target::Id(id.into()), 1));
    }
    if first == "here" {
        return Ok((Target::Here, 1));
    }
    if first == "misspelling" {
        let n = w
            .get(1)
            .and_then(|s| s.parse().ok())
            .ok_or("misspelling needs a number")?;
        return Ok((Target::Misspelling(n), 2));
    }
    if first == "text" || first == "text-end" {
        let page: usize = w
            .get(1)
            .and_then(|s| s.parse().ok())
            .ok_or("text needs a page number")?;
        let text = w
            .get(2)
            .filter(|s| is_quoted(s))
            .ok_or("text needs quoted words")?;
        return Ok((
            Target::Text {
                page: page.saturating_sub(1),
                text: unquote(text).into(),
                end: first == "text-end",
            },
            3,
        ));
    }
    if first == "annot" || first == "object" {
        let num = |i: usize| -> Result<usize, String> {
            w.get(i)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| format!("{first} needs a page and a number"))
        };
        let (page, n) = (num(1)?.saturating_sub(1), num(2)?);
        let t = if first == "annot" {
            Target::Annot { page, n }
        } else {
            Target::Object { page, n }
        };
        return Ok((t, 3));
    }
    if first == "handle" {
        let name = w.get(1).ok_or("handle needs a name")?;
        return Ok((Target::Handle(unquote(name).into()), 2));
    }
    if first == "page" {
        let n: usize = w
            .get(1)
            .and_then(|s| s.parse().ok())
            .ok_or("page needs a number")?;
        let (x, y) = w.get(2).and_then(|s| pair(s)).ok_or("page N needs X,Y")?;
        return Ok((
            Target::Page {
                page: n.saturating_sub(1),
                x,
                y,
            },
            3,
        ));
    }
    let (x, y) = pair(first).ok_or_else(|| format!("not a target: {first}"))?;
    Ok((Target::Window(x as f32, y as f32), 1))
}

fn ms(w: &[String], default: u64) -> Result<u64, String> {
    match w.first() {
        None => Ok(default),
        Some(s) => s
            .trim_end_matches("ms")
            .parse()
            .map_err(|_| format!("not a duration: {s}")),
    }
}

fn parse_line(w: &[String]) -> Result<(Action, u64), String> {
    let cmd = w[0].as_str();
    let arg = |i: usize| -> Result<&str, String> {
        w.get(i)
            .map(|s| unquote(s))
            .ok_or_else(|| format!("{cmd} needs an argument"))
    };
    let num = |i: usize| -> Result<f64, String> {
        arg(i)?
            .parse()
            .map_err(|_| format!("{cmd}: not a number: {}", arg(i).unwrap_or("")))
    };
    Ok(match cmd {
        "open" => (Action::Open(arg(1)?.into()), 0),
        "wait" => (Action::Wait, ms(&w[1..], 0)?),
        "cursor" => (Action::Cursor(arg(1)? != "off"), 0),
        "tool" => (Action::Tool(arg(1)?.into()), 0),
        "move" => {
            let (t, n) = target(&w[1..])?;
            (Action::Move(t), ms(&w[1 + n..], 400)?)
        }
        "rclick" => {
            let (t, n) = target(&w[1..])?;
            (Action::RightClick(t), ms(&w[1 + n..], 400)?)
        }
        "click" | "dblclick" | "tripleclick" => {
            let (t, n) = target(&w[1..])?;
            let count = match cmd {
                "click" => 1,
                "dblclick" => 2,
                _ => 3,
            };
            (Action::Click { target: t, count }, ms(&w[1 + n..], 400)?)
        }
        "drag" => {
            let (a, n) = target(&w[1..])?;
            let (b, m) = target(&w[1 + n..])?;
            (Action::Drag(a, b), ms(&w[1 + n + m..], 600)?)
        }
        "drag-hold" => {
            let (a, n) = target(&w[1..])?;
            let (b, m) = target(&w[1 + n..])?;
            (Action::Hold(a, b), ms(&w[1 + n + m..], 600)?)
        }
        "release" => (Action::Release, ms(&w[1..], 0)?),
        "stroke" => {
            let page_form = w.get(1).map(String::as_str) == Some("page");
            let (t, n) = if page_form {
                (None, 3)
            } else {
                let (t, n) = target(&w[1..])?;
                (Some(t), 1 + n)
            };
            let mut points = Vec::new();
            let mut i = n;
            while let Some(p) = w.get(i).and_then(|s| pair(s)) {
                points.push(p);
                i += 1;
            }
            if points.len() < 2 {
                return Err("a stroke needs two points or more".into());
            }
            let action = match t {
                Some(target) => Action::StrokeAt { target, points },
                None => Action::Stroke {
                    page: (num(2)? as usize).saturating_sub(1),
                    points,
                },
            };
            (action, ms(&w[i..], 800)?)
        }
        "type" => (Action::Type(arg(1)?.into()), ms(&w[2..], 0)?),
        "key" => (Action::Key(arg(1)?.into()), 0),
        "scroll" => (Action::Scroll(num(1)?), ms(&w[2..], 500)?),
        "zoom" => (Action::Zoom(num(1)? / 100.0), ms(&w[2..], 400)?),
        "page" => (Action::Page((num(1)? as usize).saturating_sub(1)), 0),
        "screenshot" => (Action::Screenshot(arg(1)?.into()), 0),
        "assert" => {
            let op = Op::parse(arg(2)?)
                .ok_or_else(|| format!("unknown operator {}", arg(2).unwrap_or("")))?;
            let value = w[3..]
                .iter()
                .map(|s| unquote(s))
                .collect::<Vec<_>>()
                .join(" ");
            (
                Action::Assert {
                    key: arg(1)?.into(),
                    op,
                    value,
                },
                0,
            )
        }
        "await" => {
            let (assert, _) = parse_line(&[&["assert".to_owned()], &w[1..w.len() - 1]].concat())?;
            (assert, ms(&w[w.len() - 1..], 0)?)
        }
        "call" => (
            Action::Call(
                arg(1)?.into(),
                w[2..].iter().map(|s| unquote(s).to_owned()).collect(),
            ),
            0,
        ),
        "quit" => (Action::Quit, 0),
        _ => return Err(format!("unknown command {cmd}")),
    })
}

/// Parses a script; `env` expands `$NAME`s.
pub fn parse(src: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<Vec<Step>, String> {
    let mut out = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        let line = expand(raw, env);
        let w = words(&line).map_err(|e| format!("line {}: {e}", i + 1))?;
        if w.is_empty() {
            continue;
        }
        let (action, ms) = parse_line(&w).map_err(|e| format!("line {}: {e}", i + 1))?;
        out.push(Step {
            action,
            ms,
            line: i + 1,
        });
    }
    Ok(out)
}

/// Ease-in-out (cubic), for natural pointer motion.
pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// The point at `t` (0..=1) along a polyline, by length.
pub fn along(points: &[(f64, f64)], t: f64) -> (f64, f64) {
    let Some(&first) = points.first() else {
        return (0.0, 0.0);
    };
    let lens: Vec<f64> = points
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .collect();
    let total: f64 = lens.iter().sum();
    if total <= 0.0 {
        return first;
    }
    let mut d = t.clamp(0.0, 1.0) * total;
    for (i, l) in lens.iter().enumerate() {
        if d <= *l || i == lens.len() - 1 {
            let f = if *l > 0.0 { (d / l).min(1.0) } else { 1.0 };
            let (a, b) = (points[i], points[i + 1]);
            return (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
        }
        d -= l;
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(n: &str) -> Option<String> {
        (n == "DIR").then(|| "/tmp/x".into())
    }

    #[test]
    fn right_clicks_and_misspellings() {
        let s = parse("rclick misspelling 2 300\nclick misspelling 1", &|_| None).unwrap();
        assert_eq!(s[0].action, Action::RightClick(Target::Misspelling(2)));
        assert_eq!(s[0].ms, 300);
        assert!(matches!(
            s[1].action,
            Action::Click {
                target: Target::Misspelling(1),
                count: 1
            }
        ));
        assert!(parse("rclick misspelling x", &|_| None).is_err());
    }

    #[test]
    fn parses_a_script() {
        let s = parse(
            "# demo\nopen \"$DIR/a b.pdf\"\nclick \"Save\" 300\nclick \"Pages\"@2\n\
             drag page 1 10,20 page 2 30.5,40 900\nstroke page 1 0,0 10,10 20,0\n\
             assert annotations >= 2\nkey ctrl+shift+z\ncall organize\nwait 250ms\n\
             type \"say \\\"hi\\\"\"\nzoom 150",
            &env,
        )
        .unwrap();
        assert_eq!(s[0].action, Action::Open("/tmp/x/a b.pdf".into()));
        assert_eq!(
            s[1].action,
            Action::Click {
                target: Target::Label {
                    label: "Save".into(),
                    nth: 0
                },
                count: 1
            }
        );
        assert_eq!(s[1].ms, 300);
        assert!(matches!(
            &s[2].action,
            Action::Click {
                target: Target::Label { nth: 1, .. },
                ..
            }
        ));
        assert_eq!(
            s[3].action,
            Action::Drag(
                Target::Page {
                    page: 0,
                    x: 10.0,
                    y: 20.0
                },
                Target::Page {
                    page: 1,
                    x: 30.5,
                    y: 40.0
                }
            )
        );
        assert_eq!(s[3].ms, 900);
        assert!(matches!(&s[4].action, Action::Stroke { points, .. } if points.len() == 3));
        assert_eq!(s[4].ms, 800);
        assert_eq!(
            s[5].action,
            Action::Assert {
                key: "annotations".into(),
                op: Op::Ge,
                value: "2".into()
            }
        );
        assert_eq!(s[8].ms, 250);
        assert_eq!(s[9].action, Action::Type("say \"hi\"".into()));
        assert_eq!(s[10].action, Action::Zoom(1.5));
        assert_eq!(s[10].line, 12);
        assert!(parse("frobnicate", &env).is_err());
        assert!(parse("click \"open", &env).is_err());
        let s = parse("await sigs.verdict == \"valid\" 20000", &env).unwrap();
        assert_eq!(
            s[0].action,
            Action::Assert {
                key: "sigs.verdict".into(),
                op: Op::Eq,
                value: "valid".into()
            }
        );
        assert_eq!(s[0].ms, 20000);
        let s = parse("click object 1 3 200\ndrag handle br page 1 10,10", &env).unwrap();
        assert_eq!(
            s[0].action,
            Action::Click {
                target: Target::Object { page: 0, n: 3 },
                count: 1
            }
        );
        assert_eq!(s[0].ms, 200);
        assert!(matches!(&s[1].action, Action::Drag(Target::Handle(h), _) if h == "br"));
    }

    #[test]
    fn comparisons_and_motion() {
        assert!(Op::Ge.holds("10", "9"));
        assert!(!Op::Eq.holds("10", "9"));
        assert!(Op::Eq.holds("true", "true"));
        assert!(Op::Contains.holds("Hello world", "lo w"));
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-9);
        assert_eq!(
            along(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], 0.75),
            (10.0, 5.0)
        );
    }
}
