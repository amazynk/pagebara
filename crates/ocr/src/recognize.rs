//! Text recognition post-processing: the character list of a model and greedy CTC
//! decoding into words with their horizontal extents.

/// The characters of a recognition model's output classes: class 0 is the CTC blank,
/// classes `1..=n` the dictionary, and the last one a space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Charset {
    chars: Vec<String>,
    /// The classes of `{`, `}` and `` ` ``, which the recogniser takes for `(`, `)` and `'`
    /// in small print (see [`fix_lookalikes`]).
    code: [Option<usize>; 3],
}

impl Charset {
    /// Reads `PostProcess.character_dict` of a PaddleOCR `inference.yml`.
    pub fn from_yaml(yml: &str) -> Option<Charset> {
        let mut lines = yml.lines().skip_while(|l| l.trim() != "character_dict:");
        lines.next()?;
        let mut chars = Vec::new();
        for l in lines {
            let Some(item) = l.strip_prefix("  - ") else {
                break;
            };
            chars.push(unquote(item)?);
        }
        (!chars.is_empty()).then(|| Charset::new(chars))
    }

    fn new(chars: Vec<String>) -> Charset {
        let class = |c: &str| chars.iter().position(|s| s == c).map(|i| i + 1);
        let code = [class("{"), class("}"), class("`")];
        Charset { chars, code }
    }

    /// Output classes a model with this list has.
    pub fn classes(&self) -> usize {
        self.chars.len() + 2
    }

    /// The text of class `i` (`None` for the blank).
    pub fn get(&self, i: usize) -> Option<&str> {
        match i {
            0 => None,
            i if i == self.chars.len() + 1 => Some(" "),
            i => self.chars.get(i - 1).map(String::as_str),
        }
    }
}

/// A YAML flow scalar as PaddleOCR writes them: plain, `'single'` or `"double"` quoted.
fn unquote(s: &str) -> Option<String> {
    // Only ASCII blanks: entries can be Unicode spaces (U+3000 in the Chinese list).
    let s = s.trim_end_matches([' ', '\t', '\r']);
    if let Some(body) = s.strip_prefix('\'').and_then(|b| b.strip_suffix('\'')) {
        return Some(body.replace("''", "'"));
    }
    if let Some(body) = s.strip_prefix('"').and_then(|b| b.strip_suffix('"')) {
        let mut out = String::new();
        let mut it = body.chars();
        while let Some(c) = it.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match it.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '0' => out.push('\0'),
                'x' | 'u' | 'U' => {
                    let hex: String = it.clone().take_while(|c| c.is_ascii_hexdigit()).collect();
                    let hex = &hex[..hex.len().min(8)];
                    for _ in 0..hex.len() {
                        it.next();
                    }
                    out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
                }
                c => out.push(c),
            }
        }
        return Some(out);
    }
    (!s.is_empty()).then(|| s.to_owned())
}

/// A recognised word: its text, confidence and extent as fractions of the line image width.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedWord {
    pub text: String,
    pub confidence: f32,
    pub start: f32,
    pub end: f32,
    /// Where each character starts: its byte offset in `text` and the fraction of the line
    /// width (the first is at `start`; each ends where the next starts, the last at `end`).
    pub cuts: Vec<(usize, f32)>,
}

/// Greedy CTC decoding of `frames` × `classes` probabilities into words. `width` is the
/// share of the frames covering the line image (the rest is padding).
pub fn decode(
    probs: &[f32],
    frames: usize,
    classes: usize,
    charset: &Charset,
    width: f32,
) -> Vec<DecodedWord> {
    // (text, first frame, last frame, probability) per character, and the probabilities
    // of `{`, `}` and `` ` `` where it is surest.
    let mut chars: Vec<(&str, usize, usize, f32)> = Vec::new();
    let mut code: Vec<[f32; 3]> = Vec::new();
    let code_at = |row: &[f32]| {
        charset
            .code
            .map(|c| c.and_then(|c| row.get(c)).copied().unwrap_or(0.0))
    };
    let mut prev = 0;
    let used = ((frames as f32 * width).ceil() as usize).min(frames);
    for t in 0..used {
        let row = &probs[t * classes..(t + 1) * classes];
        let (best, p) = row
            .iter()
            .enumerate()
            .fold((0, f32::MIN), |b, (i, &v)| if v > b.1 { (i, v) } else { b });
        if best != 0 {
            if best == prev {
                if let Some(c) = chars.last_mut() {
                    c.2 = t;
                    if p > c.3 {
                        c.3 = p;
                        if let Some(k) = code.last_mut() {
                            *k = code_at(row);
                        }
                    }
                }
            } else if let Some(s) = charset.get(best) {
                chars.push((s, t, t, p));
                code.push(code_at(row));
            }
        }
        prev = best;
    }

    fix_lookalikes(&mut chars, &code);

    // Words: runs of non-space characters. A word spans from halfway between its first
    // character and the one before (or a typical half-advance before it) to the matching
    // point after its last.
    let n = used.max(1) as f32;
    let pitch = typical_pitch(&chars);
    let mut words = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].0.trim().is_empty() {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < chars.len() && !chars[j + 1].0.trim().is_empty() {
            j += 1;
        }
        let first = chars[i].1 as f32 + 0.5;
        let last = chars[j].2 as f32 + 0.5;
        let before = if i > 0 {
            chars[i - 1].2 as f32 + 0.5
        } else {
            f32::MIN
        };
        let after = chars.get(j + 1).map_or(f32::MAX, |c| c.1 as f32 + 0.5);
        let start = (first - pitch * 0.6).max((before + first) / 2.0).max(0.0);
        let end = (last + pitch * 0.6).min((last + after) / 2.0).min(n);
        let mut text = String::new();
        let mut cuts = Vec::with_capacity(j + 1 - i);
        for k in i..=j {
            // Between characters, halfway from the last frame of one to the first of the next.
            let at = if k == i {
                start
            } else {
                (chars[k - 1].2 as f32 + chars[k].1 as f32 + 1.0) / 2.0
            };
            cuts.push((text.len(), at / n));
            text.push_str(chars[k].0);
        }
        let confidence = chars[i..=j].iter().map(|c| c.3).sum::<f32>() / (j + 1 - i) as f32;
        words.push(DecodedWord {
            text,
            confidence,
            start: start / n,
            end: end / n,
            cuts,
        });
        i = j + 1;
    }
    words
}

/// Probability of a brace above which the line's brackets decide (true parentheses give
/// braces far less), and of backticks for quotes.
const LOOKALIKE: f32 = 0.005;
const QUOTE_LOOKALIKE: f32 = 0.08;

/// Brackets and quotes of a line that the recogniser read as their look-alikes, when the
/// line says so and the look-alike was a close second: an opening `(` closed by `}` or left
/// open was `{`, a `)` closing `{` was `}`, and a pair of `'` around a word was backticks.
fn fix_lookalikes(chars: &mut [(&str, usize, usize, f32)], code: &[[f32; 3]]) {
    let mut open: Vec<usize> = Vec::new();
    for i in 0..chars.len() {
        match chars[i].0 {
            "(" | "[" | "{" => open.push(i),
            close @ (")" | "]" | "}") => {
                let Some(&o) = open.last() else { continue };
                match (chars[o].0, close) {
                    ("(", "}") if code[o][0] >= LOOKALIKE => chars[o].0 = "{",
                    ("{", ")") if code[i][1] >= LOOKALIKE => chars[i].0 = "}",
                    _ => {}
                }
                open.pop();
            }
            _ => {}
        }
    }
    for o in open {
        if chars[o].0 == "(" && code[o][0] >= LOOKALIKE {
            chars[o].0 = "{";
        }
    }
    let quotes: Vec<usize> = (0..chars.len()).filter(|&i| chars[i].0 == "'").collect();
    for &[a, b] in quotes.as_chunks::<2>().0 {
        let word = b > a + 1 && chars[a + 1..b].iter().all(|c| !c.0.trim().is_empty());
        if word && code[a][2] >= QUOTE_LOOKALIKE && code[b][2] >= QUOTE_LOOKALIKE {
            chars[a].0 = "`";
            chars[b].0 = "`";
        }
    }
}

/// The median distance in frames between neighbouring characters of a word.
fn typical_pitch(chars: &[(&str, usize, usize, f32)]) -> f32 {
    let mut d: Vec<f32> = chars
        .windows(2)
        .filter(|w| !w[0].0.trim().is_empty() && !w[1].0.trim().is_empty())
        .map(|w| (w[1].1 + w[1].2) as f32 / 2.0 - (w[0].1 + w[0].2) as f32 / 2.0)
        .collect();
    if d.is_empty() {
        return 2.0;
    }
    d.sort_by(f32::total_cmp);
    d[d.len() / 2].max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookalikes_follow_the_line() {
        let fix = |text: &str, code: &[[f32; 3]]| {
            let mut chars: Vec<(&str, usize, usize, f32)> = text
                .split(' ')
                .map(|c| (if c.is_empty() { " " } else { c }, 0, 0, 0.6))
                .collect();
            fix_lookalikes(&mut chars, code);
            chars.iter().map(|c| c.0).collect::<String>()
        };
        let maybe = [0.3, 0.3, 0.3];
        let sure = [0.0; 3];
        // `fn main() (` and `("(:?}"`: the `(` was a brace.
        assert_eq!(fix("( ) (", &[sure, sure, maybe]), "(){");
        assert_eq!(fix("( : }", &[maybe, sure, sure]), "{:}");
        // Open at the end but surely a parenthesis (a call over several lines).
        assert_eq!(fix("f (", &[sure, sure]), "f(");
        // `{ x )`: the `)` was a brace.
        assert_eq!(fix("{ x )", &[sure, sure, maybe]), "{x}");
        // Quotes around a word, both maybe backticks.
        assert_eq!(fix("' t x ' .", &[maybe, sure, sure, maybe, sure]), "`tx`.");
        assert_eq!(fix("' t x ' .", &[maybe, sure, sure, sure, sure]), "'tx'.");
    }

    #[test]
    fn reads_yaml() {
        let y = "PostProcess:\n  name: CTCLabelDecode\n  character_dict:\n  - '0'\n  - A\n  - ''''\n  - \"\\\"\"\n  - \"\\u00e9\"\n  - \u{3000}\nOther: 1\n";
        let c = Charset::from_yaml(y).unwrap();
        assert_eq!(c.classes(), 8);
        let all: Vec<_> = (0..8).map(|i| c.get(i)).collect();
        assert_eq!(
            all,
            [
                None,
                Some("0"),
                Some("A"),
                Some("'"),
                Some("\""),
                Some("é"),
                Some("\u{3000}"),
                Some(" ")
            ]
        );
    }

    #[test]
    fn decodes_words() {
        let c = Charset::new(vec!["a".into(), "b".into()]);
        // Frames: a a _ b _ space _ b b _ (padding)
        let seq = [1, 1, 0, 2, 0, 3, 0, 2, 2, 0, 0, 0];
        let mut probs = vec![0.0; seq.len() * 4];
        for (t, &k) in seq.iter().enumerate() {
            probs[t * 4 + k] = 0.9;
        }
        let w = decode(&probs, seq.len(), 4, &c, 10.0 / 12.0);
        let text: Vec<_> = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(text, ["ab", "b"]);
        assert!(w[0].start < 0.1 && w[0].end <= 0.55 && w[1].start >= 0.55);
        assert!(w[1].end <= 1.0 && (w[0].confidence - 0.9).abs() < 1e-6);
        // "a" ends and "b" starts between frames 1 and 3.
        assert_eq!(w[0].cuts.len(), 2);
        assert_eq!(w[0].cuts[0], (0, w[0].start));
        assert_eq!(w[0].cuts[1], (1, 2.5 / 10.0));
    }
}
