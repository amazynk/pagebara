//! Suggestions for a misspelled word: replacement tables, single edits (with the keyboard
//! layout and the likeliest letters first), two words, then dictionary words within two
//! edits. The work is budgeted so a suggestion takes milliseconds.

use std::collections::HashSet;

use crate::aff::has;
use crate::casing::{Cap, cap};
use crate::dictionary::{Dictionary, HIDDEN, SUGGEST};

/// Suggestions returned by [`Dictionary::suggest`].
pub const MAX_SUGGESTIONS: usize = 6;
/// Candidate words checked at most.
const BUDGET: usize = 8000;
/// Letters tried for insertions and replacements at most.
const MAX_TRY: usize = 48;

/// How a suggestion was found, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// Case, REP and MAP.
    Table,
    Swap,
    Key,
    Edit,
    /// Two words of three letters or more.
    Split,
    Far,
    /// Two words, one of them short ("co lour").
    ShortSplit,
}

/// A suggestion; `recased` when only its capitalised form is a word.
struct Found {
    word: String,
    kind: Kind,
    recased: bool,
}

struct Collector<'a> {
    dict: &'a Dictionary,
    found: Vec<Found>,
    seen: HashSet<String>,
    checks: usize,
}

impl Collector<'_> {
    fn full(&self) -> bool {
        self.checks >= BUDGET
    }

    fn push(&mut self, word: String, kind: Kind, recased: bool) {
        self.found.push(Found {
            word,
            kind,
            recased,
        });
    }

    fn try_word(&mut self, w: &str, kind: Kind) {
        if self.full() || w.is_empty() || !self.seen.insert(w.to_owned()) {
            return;
        }
        self.checks += 1;
        if w.contains(' ') {
            let parts: Vec<&str> = w.split(' ').collect();
            if parts
                .iter()
                .all(|p| !p.is_empty() && self.dict.cased(p, SUGGEST).is_some())
            {
                let short = parts.iter().any(|p| p.chars().count() < 3);
                let kind = if short && kind == Kind::Split {
                    Kind::ShortSplit
                } else {
                    kind
                };
                self.push(w.to_owned(), kind, false);
            }
            return;
        }
        if self.dict.cased(w, SUGGEST).is_some() {
            self.push(w.to_owned(), kind, false);
        } else if w.chars().next().is_some_and(char::is_lowercase) {
            // Names: "pariss" -> "Paris".
            let title = self.dict.casing.capitalize(w);
            self.checks += 1;
            if !self.seen.contains(&title) && self.dict.word(&title, SUGGEST).is_some() {
                self.seen.insert(title.clone());
                self.push(title, kind, true);
            }
        }
    }

    fn try_chars(&mut self, cs: &[char], kind: Kind) {
        let w: String = cs.iter().collect();
        self.try_word(&w, kind);
    }
}

impl Dictionary {
    /// Up to [`MAX_SUGGESTIONS`] corrections for `word`, best first.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        self.suggest_n(word, MAX_SUGGESTIONS)
    }

    /// Up to `max` corrections for `word`, best first.
    pub fn suggest_n(&self, word: &str, max: usize) -> Vec<String> {
        let w = self.normalize(word).trim().to_owned();
        if w.is_empty() || max == 0 {
            return Vec::new();
        }
        let shape = cap(&w);
        let base = match shape {
            Cap::Init | Cap::All => self.casing.lower(&w),
            _ => w.clone(),
        };
        let mut c = Collector {
            dict: self,
            found: Vec::new(),
            seen: HashSet::from([w.clone()]),
            checks: 0,
        };
        self.edits(&mut c, &w, &base);
        if c.found.len() < max {
            self.far(&mut c, &base, max);
        }
        let mut found = c.found;
        // Within a kind, words in the case asked for, then those most like the input.
        let folded = self.fold(&base);
        found.sort_by_cached_key(|f| {
            let tie = match f.kind {
                Kind::Table | Kind::Swap => 0,
                _ => -similarity(&folded, &self.fold(&f.word)),
            };
            (f.kind, f.recased, tie)
        });
        let mut out: Vec<String> = Vec::new();
        for Found { word: s, .. } in found {
            let s = match shape {
                Cap::All => self.casing.upper(&s),
                Cap::Init if s.chars().next().is_some_and(char::is_lowercase) => {
                    self.casing.capitalize(&s)
                }
                _ => s,
            };
            let s = if self.aff.oconv.is_empty() {
                s
            } else {
                self.aff.oconv.apply(&s)
            };
            if s != word && !out.contains(&s) {
                out.push(s);
            }
            if out.len() == max {
                break;
            }
        }
        out
    }

    fn edits(&self, c: &mut Collector<'_>, w: &str, base: &str) {
        let a = &self.aff;
        // Capitalisation: "paris" -> "Paris", "nasa" -> "NASA".
        c.try_word(&self.casing.capitalize(base), Kind::Table);
        let upper = self.casing.upper(w);
        if upper != w && self.word(&upper, SUGGEST).is_some() && c.seen.insert(upper.clone()) {
            c.push(upper, Kind::Table, false);
        }
        for r in &a.rep {
            for (i, _) in base.match_indices(&*r.from) {
                if (r.start && i != 0) || (r.end && i + r.from.len() != base.len()) {
                    continue;
                }
                let s = format!("{}{}{}", &base[..i], r.to, &base[i + r.from.len()..]);
                c.try_word(&s, Kind::Table);
            }
        }
        if !a.map.is_empty() {
            let mut tries = 0;
            self.map_related(c, base, &mut String::new(), false, &mut tries);
        }
        if w.contains(['’', 'ʼ']) {
            c.try_word(&w.replace(['’', 'ʼ'], "'"), Kind::Table);
        }
        let cs: Vec<char> = base.chars().collect();
        let n = cs.len();
        let mut buf = cs.clone();
        // Adjacent and distant swaps.
        for dist in 1..=3 {
            for i in 0..n.saturating_sub(dist) {
                if cs[i] == cs[i + dist] {
                    continue;
                }
                buf.swap(i, i + dist);
                c.try_chars(&buf, Kind::Swap);
                buf.swap(i, i + dist);
            }
        }
        // Neighbouring keys.
        let rows: Vec<Vec<char>> = a.key.split('|').map(|r| r.chars().collect()).collect();
        for i in 0..n {
            for row in &rows {
                for (k, &key) in row.iter().enumerate() {
                    if key != cs[i] {
                        continue;
                    }
                    for nb in [k.checked_sub(1), Some(k + 1)].into_iter().flatten() {
                        if let Some(&x) = row.get(nb) {
                            buf[i] = x;
                            c.try_chars(&buf, Kind::Key);
                        }
                    }
                }
            }
            buf[i] = cs[i];
        }
        // An extra letter.
        for i in 0..n {
            if i > 0 && cs[i] == cs[i - 1] && i + 1 < n {
                continue;
            }
            let mut s = cs.clone();
            s.remove(i);
            c.try_chars(&s, Kind::Edit);
        }
        let try_chars: Vec<char> = a.try_chars.chars().take(MAX_TRY).collect();
        let try_chars = if try_chars.is_empty() {
            "esianrtolcdugmphbyfvkwzxjq".chars().collect()
        } else {
            try_chars
        };
        // A forgotten letter.
        for i in 0..=n {
            for &t in &try_chars {
                let mut s = cs.clone();
                s.insert(i, t);
                c.try_chars(&s, Kind::Edit);
            }
        }
        // A letter in the wrong place.
        for i in 0..n {
            for j in 0..n {
                if j + 1 < i || j > i + 1 {
                    let mut s = cs.clone();
                    let ch = s.remove(i);
                    s.insert(j.min(s.len()), ch);
                    c.try_chars(&s, Kind::Edit);
                }
            }
        }
        // A wrong letter.
        for i in 0..n {
            for &t in &try_chars {
                if t != cs[i] {
                    buf[i] = t;
                    c.try_chars(&buf, Kind::Edit);
                }
            }
            buf[i] = cs[i];
        }
        // Two letters typed twice ("vacacation").
        for i in 0..n.saturating_sub(4) {
            if cs[i] == cs[i + 2] && cs[i + 1] == cs[i + 3] {
                let mut s = cs.clone();
                s.drain(i + 2..i + 4);
                c.try_chars(&s, Kind::Edit);
            }
        }
        // Two words.
        if !a.nosplitsugs {
            for i in 2..n.saturating_sub(1) {
                let s: String = cs[..i].iter().chain([' '].iter()).chain(&cs[i..]).collect();
                c.try_word(&s, Kind::Split);
            }
        }
    }

    /// Every combination of related characters (`MAP`): "zazolc" -> "zażółć".
    fn map_related(
        &self,
        c: &mut Collector<'_>,
        rest: &str,
        done: &mut String,
        changed: bool,
        tries: &mut usize,
    ) {
        if *tries > 300 || c.full() {
            return;
        }
        let Some(ch) = rest.chars().next() else {
            if changed {
                *tries += 1;
                c.try_word(done, Kind::Table);
            }
            return;
        };
        let len = done.len();
        for group in &self.aff.map {
            for item in group.iter().filter(|i| rest.starts_with(&***i)) {
                for other in group.iter().filter(|o| *o != item) {
                    done.push_str(other);
                    self.map_related(c, &rest[item.len()..], done, true, tries);
                    done.truncate(len);
                }
            }
        }
        done.push(ch);
        self.map_related(c, &rest[ch.len_utf8()..], done, changed, tries);
        done.truncate(len);
    }

    /// Lower case with related characters (`MAP`) made equal, for comparing words.
    fn fold(&self, s: &str) -> String {
        let mut s = self.casing.lower(s);
        for group in &self.aff.map {
            for item in &group[1..] {
                if s.contains(&**item) {
                    s = s.replace(&**item, &group[0]);
                }
            }
        }
        s
    }

    /// Dictionary roots within two edits of `base`.
    fn far(&self, c: &mut Collector<'_>, base: &str, max: usize) {
        let target: Vec<char> = base.chars().flat_map(char::to_lowercase).collect();
        if target.len() < 3 {
            return;
        }
        let a = &self.aff;
        let mut best: Vec<(usize, &str)> = Vec::new();
        let mut buf = Vec::with_capacity(32);
        for (root, flags) in self.words.iter() {
            if root.len() + 8 < base.len() || root.len() > base.len() + 8 {
                continue;
            }
            if has(flags, Some(HIDDEN))
                || has(flags, a.forbidden)
                || has(flags, a.needaffix)
                || has(flags, a.onlyincompound)
                || has(flags, a.nosuggest)
            {
                continue;
            }
            buf.clear();
            buf.extend(root.chars().flat_map(char::to_lowercase));
            if buf.len().abs_diff(target.len()) > 2 {
                continue;
            }
            if let Some(d) = distance(&target, &buf, 2)
                && d > 0
            {
                best.push((d, root));
            }
        }
        best.sort_by_key(|&(d, r)| (d, r.len().abs_diff(base.len()), common_prefix(r, base) == 0));
        for (_, root) in best.into_iter().take(max * 3) {
            c.try_word(root, Kind::Far);
            if c.found.len() >= max * 2 {
                break;
            }
        }
    }
}

/// Shared letter pairs, triples (twice as much) and first letter (twice as much).
fn similarity(a: &str, b: &str) -> i32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut score = if a.first() == b.first() { 2 } else { 0 };
    for n in [2, 3] {
        let mut grams: Vec<&[char]> = b.windows(n).collect();
        for g in a.windows(n) {
            if let Some(i) = grams.iter().position(|x| *x == g) {
                grams.swap_remove(i);
                score += n as i32 - 1;
            }
        }
    }
    score
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// The optimal string alignment distance of `a` and `b`, if at most `limit`.
fn distance(a: &[char], b: &[char], limit: usize) -> Option<usize> {
    let (n, m) = (a.len(), b.len());
    let mut prev2 = vec![0usize; m + 1];
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        let mut row_min = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(prev2[j - 2] + 1);
            }
            cur[j] = v;
            row_min = row_min.min(v);
        }
        if row_min > limit {
            return None;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    Some(prev[m]).filter(|&d| d <= limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(aff: &str, dic: &str) -> Dictionary {
        Dictionary::from_bytes(aff.as_bytes(), dic.as_bytes()).unwrap()
    }

    #[test]
    fn edit_distance() {
        let d = |a: &str, b: &str| {
            let a: Vec<char> = a.chars().collect();
            let b: Vec<char> = b.chars().collect();
            distance(&a, &b, 2)
        };
        assert_eq!(d("teh", "the"), Some(1));
        assert_eq!(d("acomodate", "accommodate"), Some(2));
        assert_eq!(d("kitten", "sitting"), None);
    }

    #[test]
    fn suggestions() {
        let d = dict(
            "TRY esianrtolcdugmphbyfvkwz\nREP 1\nREP f ph\nNOSUGGEST !\nSFX S Y 1\nSFX S 0 s .\n",
            "9\nthe/S\nreceive\nphone/S\nParis\nhello\nworld\nten\ncrap/!\naccommodate\n",
        );
        assert_eq!(d.suggest("teh")[0], "the");
        assert_eq!(d.suggest("recieve")[0], "receive");
        assert_eq!(d.suggest("fones")[0], "phones");
        assert_eq!(d.suggest("paris")[0], "Paris");
        assert_eq!(d.suggest("Teh")[0], "The");
        assert_eq!(d.suggest("TEH")[0], "THE");
        assert!(d.suggest("helloworld").contains(&"hello world".to_string()));
        assert!(d.suggest("acomodate").contains(&"accommodate".to_string()));
        assert!(!d.suggest("crapp").contains(&"crap".to_string()));
        assert!(d.suggest("xqzv").len() <= MAX_SUGGESTIONS);
        let map = dict("MAP 1\nMAP u(ue)ü\n", "1\nmüde\n");
        assert_eq!(map.suggest("muede")[0], "müde");
        let key = dict("TRY \n", "2\nword\nwore\n");
        assert_eq!(key.suggest("wird")[0], "word");
    }
}
