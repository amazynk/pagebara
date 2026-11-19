//! Finding the marked strings in Slint and Rust sources.

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Message {
    pub context: Option<String>,
    pub id: String,
    pub plural: Option<String>,
    pub comments: Vec<String>,
    pub line: usize,
    pub refs: Vec<String>,
}

struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
    /// The last `// TRANSLATORS:` comment and the line it ended on.
    note: Option<(String, usize)>,
}

impl<'a> Cursor<'a> {
    fn new(src: &'a str) -> Self {
        Cursor {
            s: src.as_bytes(),
            i: 0,
            line: 1,
            note: None,
        }
    }

    fn peek(&self, k: usize) -> u8 {
        self.s.get(self.i + k).copied().unwrap_or(0)
    }

    fn bump(&mut self) -> u8 {
        let c = self.peek(0);
        if c == b'\n' {
            self.line += 1;
        }
        self.i += 1;
        c
    }

    fn done(&self) -> bool {
        self.i >= self.s.len()
    }

    fn starts(&self, pat: &str) -> bool {
        self.s[self.i..].starts_with(pat.as_bytes())
    }

    /// Skips a comment at the cursor, if any; nested block comments count.
    fn comment(&mut self) -> bool {
        if self.starts("//") {
            let start = self.i + 2;
            while !self.done() && self.peek(0) != b'\n' {
                self.bump();
            }
            let text = String::from_utf8_lossy(&self.s[start..self.i]);
            let text = text.trim().trim_start_matches('/').trim();
            if let Some(n) = text.strip_prefix("TRANSLATORS:") {
                self.note = Some((n.trim().to_owned(), self.line));
            }
            true
        } else if self.starts("/*") {
            let mut depth = 0;
            while !self.done() {
                if self.starts("/*") {
                    depth += 1;
                    self.i += 1;
                } else if self.starts("*/") {
                    depth -= 1;
                    self.i += 1;
                    if depth == 0 {
                        self.i += 1;
                        break;
                    }
                }
                self.bump();
            }
            true
        } else {
            false
        }
    }

    fn space(&mut self) {
        loop {
            while self.peek(0).is_ascii_whitespace() {
                self.bump();
            }
            if !self.comment() {
                break;
            }
        }
    }

    /// A `"..."` literal (Slint or Rust escapes), or a Rust raw string.
    fn string(&mut self) -> Option<String> {
        if self.peek(0) == b'r' && matches!(self.peek(1), b'"' | b'#') {
            self.bump();
            let mut hashes = 0;
            while self.peek(0) == b'#' {
                hashes += 1;
                self.bump();
            }
            if self.bump() != b'"' {
                return None;
            }
            let start = self.i;
            let end = "\"".to_owned() + &"#".repeat(hashes);
            while !self.done() && !self.starts(&end) {
                self.bump();
            }
            let s = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
            self.i += end.len();
            return Some(s);
        }
        if self.peek(0) != b'"' {
            return None;
        }
        self.bump();
        let mut out = Vec::new();
        while !self.done() {
            match self.bump() {
                b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
                b'\\' => match self.bump() {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'0' => out.push(0),
                    b'\n' => {
                        while self.peek(0).is_ascii_whitespace() {
                            self.bump();
                        }
                    }
                    b'u' if self.peek(0) == b'{' => {
                        self.bump();
                        let start = self.i;
                        while !self.done() && self.peek(0) != b'}' {
                            self.bump();
                        }
                        let hex = String::from_utf8_lossy(&self.s[start..self.i]).replace('_', "");
                        self.bump();
                        if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        {
                            out.extend_from_slice(c.to_string().as_bytes());
                        }
                    }
                    c => out.push(c),
                },
                c => out.push(c),
            }
        }
        None
    }

    fn take_note(&mut self) -> Vec<String> {
        match self.note.take() {
            Some((n, line)) if line + 1 >= self.line => vec![n],
            _ => Vec::new(),
        }
    }
}

/// `@tr("ctx" => "text {}" | "plural" % n, ...)`.
pub fn slint(src: &str) -> Vec<Message> {
    let mut c = Cursor::new(src);
    let mut out = Vec::new();
    while !c.done() {
        if c.comment() {
            continue;
        }
        if c.peek(0) == b'"' {
            c.string();
            continue;
        }
        if c.starts("@tr(") {
            let line = c.line;
            let comments = c.take_note();
            c.i += 4;
            c.space();
            let Some(first) = c.string() else { continue };
            c.space();
            let (context, id) = if c.starts("=>") {
                c.i += 2;
                c.space();
                let Some(id) = c.string() else { continue };
                (Some(first), id)
            } else {
                (None, first)
            };
            c.space();
            let plural = if c.peek(0) == b'|' {
                c.bump();
                c.space();
                c.string()
            } else {
                None
            };
            out.push(Message {
                context,
                id,
                plural,
                comments,
                line,
                refs: Vec::new(),
            });
            continue;
        }
        c.bump();
    }
    out
}

/// `tr!("text {}", ..)`, `trn!("{n} page", "{n} pages", n, ..)`, `tr_noop!("text")`.
pub fn rust(src: &str) -> Vec<Message> {
    let mut c = Cursor::new(src);
    let mut out = Vec::new();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while !c.done() {
        if c.comment() {
            continue;
        }
        let b = c.peek(0);
        if b == b'"'
            || (b == b'r' && matches!(c.peek(1), b'"' | b'#') && (c.i == 0 || !ident(c.s[c.i - 1])))
        {
            if c.string().is_none() {
                c.bump();
            }
            continue;
        }
        if b == b'\'' {
            // A char literal, or a lifetime.
            if c.peek(1) == b'\\' {
                c.i += 2;
                while !c.done() && c.peek(0) != b'\'' {
                    c.bump();
                }
                c.bump();
            } else {
                let ch_len = std::str::from_utf8(&c.s[c.i + 1..])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .map_or(1, char::len_utf8);
                if c.peek(1 + ch_len) == b'\'' {
                    c.i += 2 + ch_len;
                } else {
                    c.bump();
                }
            }
            continue;
        }
        if ident(b) && (c.i == 0 || !ident(c.s[c.i - 1])) {
            let start = c.i;
            while ident(c.peek(0)) {
                c.bump();
            }
            let word = std::str::from_utf8(&c.s[start..c.i]).unwrap_or("");
            if !matches!(word, "tr" | "trn" | "tr_noop") || c.peek(0) != b'!' {
                continue;
            }
            let line = c.line;
            let comments = c.take_note();
            c.bump();
            c.space();
            if c.bump() != b'(' {
                continue;
            }
            c.space();
            let Some(id) = c.string() else { continue };
            let plural = if word == "trn" {
                c.space();
                if c.peek(0) == b',' {
                    c.bump();
                    c.space();
                }
                c.string()
            } else {
                None
            };
            out.push(Message {
                context: None,
                id,
                plural,
                comments,
                line,
                refs: Vec::new(),
            });
            continue;
        }
        c.bump();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slint_strings() {
        let m = slint(
            "Text { text: \"@tr(no)\"; }\n// TRANSLATORS: a button\nB { text: @tr(\"Open\"); }\n\
             T { text: @tr(\"menu\" => \"File\"); n: @tr(\"{n} page\" | \"{n} pages\" % count); }",
        );
        assert_eq!(m.len(), 3);
        assert_eq!(
            (m[0].id.as_str(), m[0].line, m[0].comments.clone()),
            ("Open", 3, vec!["a button".to_owned()])
        );
        assert_eq!(
            (m[1].context.as_deref(), m[1].id.as_str()),
            (Some("menu"), "File")
        );
        assert_eq!(m[2].plural.as_deref(), Some("{n} pages"));
    }

    #[test]
    fn rust_strings() {
        let m = rust(
            "fn f<'a>(x: &'a str) { let c = '\"'; // tr!(\"no\")\n let s = tr!(\"Saved “{}”.\\n\", x);\n\
             let p = trn!(\"{n} page\",\n \"{n} pages\", n); let q = \"tr!(no)\"; let r = tr_noop!(r\"raw\"); }",
        );
        let ids: Vec<_> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["Saved “{}”.\n", "{n} page", "raw"]);
        assert_eq!(m[1].plural.as_deref(), Some("{n} pages"));
        assert_eq!(m[0].line, 2);
    }
}
