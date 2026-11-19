//! A loaded dictionary and word checking: affixes, compounds, case and word breaks.

use std::borrow::Cow;
use std::path::Path;

use crate::aff::{Aff, Affix, Flag, Flags, Quant, has};
use crate::casing::{Cap, Casing, cap};
use crate::table::WordTable;
use crate::tokenize::{Token, Tokens};
use crate::{Error, encoding};

/// Marks the all-capitals copy of a mixed-case word ("OPENOFFICE" for "OpenOffice").
pub(crate) const HIDDEN: Flag = Flag::MAX;

/// A Hunspell dictionary (`.aff` and `.dic`), ready to check words. Immutable, so it can be
/// shared between threads.
#[derive(Debug)]
pub struct Dictionary {
    pub(crate) aff: Aff,
    pub(crate) words: WordTable,
    pub(crate) casing: Casing,
    joiners: String,
}

/// Where a word part sits: alone, or first, inner or last part of a compound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pos {
    Alone,
    Begin,
    Middle,
    End,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Mode {
    /// Words flagged NOSUGGEST do not count.
    pub suggest: bool,
}

pub(crate) const CHECK: Mode = Mode { suggest: false };
pub(crate) const SUGGEST: Mode = Mode { suggest: true };

#[derive(Clone, Copy, Debug)]
pub(crate) struct Found {
    keepcase: bool,
}

/// A root with the affixes that make a word of it.
#[derive(Clone, Copy, Debug)]
struct Cand<'a> {
    root: &'a [Flag],
    pfx: Option<&'a Affix>,
    /// The outer suffix.
    sfx: Option<&'a Affix>,
    /// The inner suffix of a twofold suffix.
    sfx2: Option<&'a Affix>,
}

impl<'a> Cand<'a> {
    fn root(root: &'a [Flag]) -> Self {
        Cand {
            root,
            pfx: None,
            sfx: None,
            sfx2: None,
        }
    }

    fn affixes(&self) -> impl Iterator<Item = &'a Affix> + use<'a> {
        [self.pfx, self.sfx, self.sfx2].into_iter().flatten()
    }

    fn has(&self, f: Option<Flag>) -> bool {
        has(self.root, f) || self.affixes().any(|a| has(&a.cont, f))
    }
}

fn joined(a: &str, b: &str) -> String {
    let mut s = String::with_capacity(a.len() + b.len());
    s.push_str(a);
    s.push_str(b);
    s
}

/// A number ("1,234.5", "2-3"), which is always correct.
fn is_number(w: &str) -> bool {
    let mut prev_sep = true;
    for c in w.chars() {
        match c {
            '0'..='9' => prev_sep = false,
            '.' | ',' | '-' if !prev_sep => prev_sep = true,
            _ => return false,
        }
    }
    !prev_sep
}

/// The word and the flag field of a `.dic` line (`word/flags morphology`).
fn split_entry(line: &str) -> (String, &str) {
    let line = line.split('\t').next().unwrap_or(line);
    let mut word = String::new();
    let mut it = line.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            '\\' if it.peek().is_some_and(|&(_, n)| n == '/') => {
                word.push('/');
                it.next();
            }
            '/' if !word.is_empty() => {
                let rest = &line[i + 1..];
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                return (word, &rest[..end]);
            }
            c if c.is_whitespace() => break,
            c => word.push(c),
        }
    }
    (word, "")
}

impl Dictionary {
    /// Parses a dictionary from the contents of its `.aff` and `.dic` files.
    pub fn from_bytes(aff: &[u8], dic: &[u8]) -> Result<Dictionary, Error> {
        let set = encoding::declared(aff);
        let aff = Aff::parse(&encoding::decode(aff, set.as_deref()));
        let text = encoding::decode(dic, set.as_deref());
        let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
        let count: usize = lines
            .next()
            .and_then(|l| l.split_whitespace().next()?.parse().ok())
            .ok_or(Error::Invalid(
                "the .dic file does not start with a word count",
            ))?;
        let turkic = aff
            .lang
            .as_deref()
            .is_some_and(|l| l.starts_with("tr") || l.starts_with("az") || l.starts_with("crh"));
        let casing = Casing { turkic };
        let mut words = WordTable::with_capacity(count.min(4 << 20), text.len());
        for line in lines {
            if line.starts_with('#') || line.starts_with('\t') {
                continue;
            }
            let (word, flags) = split_entry(line);
            let word = if aff.ignore.is_empty() {
                word
            } else {
                aff.strip_ignored(&word)
            };
            if word.is_empty() {
                continue;
            }
            let flags: Flags = if flags.is_empty() {
                Flags::default()
            } else {
                aff.flags(flags)
                    .iter()
                    .copied()
                    .filter(|&f| f != HIDDEN)
                    .collect()
            };
            if matches!(cap(&word), Cap::Mixed | Cap::MixedInit) && !has(&flags, aff.keepcase) {
                let mut hidden = flags.to_vec();
                hidden.push(HIDDEN);
                hidden.sort_unstable();
                words.push(&casing.upper(&word), hidden.into());
            }
            words.push(&word, flags);
        }
        words.finish();
        let joiners = aff
            .word_chars
            .chars()
            .filter(|c| {
                !c.is_numeric() && !matches!(c, '.' | '/' | '\\' | '+') && !c.is_whitespace()
            })
            .collect();
        Ok(Dictionary {
            aff,
            words,
            casing,
            joiners,
        })
    }

    /// Reads a dictionary from its `.aff` and `.dic` files.
    pub fn open(aff: &Path, dic: &Path) -> Result<Dictionary, Error> {
        let read = |p: &Path| {
            std::fs::read(p).map_err(|e| Error::Io(p.display().to_string(), e.to_string()))
        };
        Dictionary::from_bytes(&read(aff)?, &read(dic)?)
    }

    /// The number of dictionary entries.
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.len() == 0
    }

    /// The language the affix file declares (`LANG`), if any.
    pub fn language(&self) -> Option<&str> {
        self.aff.lang.as_deref()
    }

    /// The words of `text` with their byte ranges, as this dictionary splits them.
    pub fn tokens<'a>(&'a self, text: &'a str) -> Tokens<'a> {
        Tokens::with_joiners(text, &self.joiners)
    }

    /// The words of `text` that are misspelled.
    pub fn misspellings<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Token<'a>> + 'a {
        self.tokens(text).filter(move |t| {
            // An abbreviation's period ("etc.") is not part of the token.
            !self.check(t.word)
                && !(text[t.range.end..].starts_with('.') && self.check(&joined(t.word, ".")))
        })
    }

    /// Whether `word` is spelled correctly.
    pub fn check(&self, word: &str) -> bool {
        let w = self.normalize(word);
        self.spell(&w, 0) || (w.contains(['’', 'ʼ']) && self.spell(&w.replace(['’', 'ʼ'], "'"), 0))
    }

    /// `ICONV` and `IGNORE` applied.
    pub(crate) fn normalize<'a>(&self, word: &'a str) -> Cow<'a, str> {
        let mut w = Cow::Borrowed(word);
        if !self.aff.iconv.is_empty() {
            w = Cow::Owned(self.aff.iconv.apply(&w));
        }
        if !self.aff.ignore.is_empty() {
            w = Cow::Owned(self.aff.strip_ignored(&w));
        }
        w
    }

    fn spell(&self, w: &str, depth: u8) -> bool {
        if w.is_empty() || is_number(w) || self.cased(w, CHECK).is_some() {
            return true;
        }
        if depth >= 8 {
            return false;
        }
        for p in &self.aff.breaks {
            if let Some(s) = p.strip_prefix('^') {
                if !s.is_empty()
                    && w.len() > s.len()
                    && w.starts_with(s)
                    && self.spell(&w[s.len()..], depth + 1)
                {
                    return true;
                }
            } else if let Some(s) = p.strip_suffix('$').filter(|s| !s.is_empty()) {
                if w.len() > s.len()
                    && w.ends_with(s)
                    && self.spell(&w[..w.len() - s.len()], depth + 1)
                {
                    return true;
                }
            } else {
                for (i, _) in w.match_indices(&**p) {
                    if i > 0
                        && i + p.len() < w.len()
                        && self.spell(&w[..i], depth + 1)
                        && self.spell(&w[i + p.len()..], depth + 1)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Checks `w` as written and in the other cases its capitalisation allows: "The" may be
    /// "the", "PARIS" may be "Paris" (unless the word keeps its case).
    pub(crate) fn cased(&self, w: &str, mode: Mode) -> Option<Found> {
        if let Some(f) = self.word(w, mode) {
            return Some(f);
        }
        let shape = cap(w);
        if !matches!(shape, Cap::Init | Cap::All) {
            return None;
        }
        let lower = self.casing.lower(w);
        let other = |w: &str| self.word(w, mode).filter(|f| !f.keepcase);
        if let Some(f) = other(&lower) {
            return Some(f);
        }
        if shape == Cap::All
            && let Some(f) = other(&self.casing.capitalize(&lower))
        {
            return Some(f);
        }
        if self.aff.check_sharps && lower.contains("ss") {
            return self.sharps(&lower, shape == Cap::All, mode);
        }
        None
    }

    /// "STRASSE" as "straße" or "Straße".
    fn sharps(&self, lower: &str, title: bool, mode: Mode) -> Option<Found> {
        let at: Vec<usize> = lower.match_indices("ss").map(|(i, _)| i).take(5).collect();
        for bits in 1u32..(1 << at.len()) {
            let mut s = String::with_capacity(lower.len());
            let mut last = 0;
            for (k, &i) in at.iter().enumerate() {
                if bits & (1 << k) != 0 && i >= last {
                    s.push_str(&lower[last..i]);
                    s.push('ß');
                    last = i + 2;
                }
            }
            s.push_str(&lower[last..]);
            if let Some(f) = self.word(&s, mode) {
                return Some(f);
            }
            if title && let Some(f) = self.word(&self.casing.capitalize(&s), mode) {
                return Some(f);
            }
        }
        None
    }

    /// Checks one word exactly as written: a root, an affixed root or a compound.
    pub(crate) fn word(&self, w: &str, mode: Mode) -> Option<Found> {
        let a = &self.aff;
        let mut found = None;
        for root in self.words.get(w) {
            if has(root, a.forbidden) {
                return None;
            }
            if self.alone(&Cand::root(root), mode) {
                found = Some(Found {
                    keepcase: has(root, a.keepcase),
                });
            }
        }
        if found.is_some() {
            return found;
        }
        self.affixed(w, Pos::Alone, &mut |c| {
            let ok = self.alone(c, mode);
            if ok {
                found = Some(Found {
                    keepcase: has(c.root, a.keepcase),
                });
            }
            ok
        });
        if found.is_some() {
            return found;
        }
        let none = Found { keepcase: false };
        let chars = w.chars().count();
        if a.compounds() && chars >= 2 * a.compound_min && self.compound(w, 0, mode) {
            return Some(none);
        }
        if a.compound_rules.iter().any(|r| self.rule(w, r, 0, 0)) {
            return Some(none);
        }
        None
    }

    /// Whether a candidate is a word on its own.
    fn alone(&self, c: &Cand<'_>, mode: Mode) -> bool {
        let a = &self.aff;
        if has(c.root, a.forbidden) || has(c.root, a.onlyincompound) {
            return false;
        }
        let n = c.affixes().count();
        if n == 0 && has(c.root, a.needaffix) {
            return false;
        }
        if c.affixes().any(|x| has(&x.cont, a.onlyincompound))
            || (n > 0 && c.affixes().all(|x| has(&x.cont, a.needaffix)))
        {
            return false;
        }
        if a.circumfix.is_some() {
            let p = c.pfx.is_some_and(|x| has(&x.cont, a.circumfix));
            let s = [c.sfx, c.sfx2]
                .into_iter()
                .flatten()
                .any(|x| has(&x.cont, a.circumfix));
            if p != s {
                return false;
            }
        }
        !(mode.suggest && c.has(a.nosuggest))
    }

    /// Calls `f` with each way `word` is a root, possibly with a prefix and/or suffixes,
    /// until it returns true.
    fn affixed(&self, word: &str, pos: Pos, f: &mut dyn FnMut(&Cand<'_>) -> bool) -> bool {
        for root in self.words.get(word) {
            if f(&Cand::root(root)) {
                return true;
            }
        }
        self.suffixed(word, pos, None, None, f) || self.prefixed(word, pos, f)
    }

    fn suffix_allowed(&self, s: &Affix, pos: Pos) -> bool {
        !matches!(pos, Pos::Begin | Pos::Middle) || has(&s.cont, self.aff.compound_permit)
    }

    /// `word` as root + suffix; with `outer`, as root + suffix + `outer`; with `pfx`, as
    /// `pfx` + root + suffix (`word` lacks the prefix).
    fn suffixed(
        &self,
        word: &str,
        pos: Pos,
        pfx: Option<&Affix>,
        outer: Option<&Affix>,
        f: &mut dyn FnMut(&Cand<'_>) -> bool,
    ) -> bool {
        let set = &self.aff.suffixes;
        for &len in &set.add_lens {
            if len > word.len() {
                break;
            }
            let cut = word.len() - len;
            if !word.is_char_boundary(cut) {
                continue;
            }
            let Some(groups) = set.by_add.get(&word[cut..]) else {
                continue;
            };
            let base = &word[..cut];
            if base.is_empty() && !self.aff.fullstrip {
                continue;
            }
            for g in groups {
                let stem = joined(base, &g.strip);
                let found = self.words.contains(&stem);
                for &ri in &g.rules {
                    let s = &set.rules[ri as usize];
                    if !self.suffix_allowed(s, pos) || !s.cond_end(&stem) {
                        continue;
                    }
                    if let Some(o) = outer
                        && !has(&s.cont, Some(o.flag))
                    {
                        continue;
                    }
                    let flag = Some(s.flag);
                    if found {
                        for root in self.words.get(&stem) {
                            if !has(root, flag) {
                                continue;
                            }
                            if let Some(p) = pfx {
                                let joint = (p.cross && s.cross && has(root, Some(p.flag)))
                                    || has(&s.cont, Some(p.flag));
                                if !joint {
                                    continue;
                                }
                            }
                            let c = match outer {
                                Some(o) => Cand {
                                    root,
                                    pfx,
                                    sfx: Some(o),
                                    sfx2: Some(s),
                                },
                                None => Cand {
                                    root,
                                    pfx,
                                    sfx: Some(s),
                                    sfx2: None,
                                },
                            };
                            if f(&c) {
                                return true;
                            }
                        }
                    }
                    if outer.is_none()
                        && pfx.is_none()
                        && matches!(pos, Pos::Alone | Pos::End)
                        && self.aff.suffix_cont.contains(&s.flag)
                        && self.suffixed(&stem, pos, None, Some(s), f)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// `word` as prefix + root, or prefix + root + suffix.
    fn prefixed(&self, word: &str, pos: Pos, f: &mut dyn FnMut(&Cand<'_>) -> bool) -> bool {
        let set = &self.aff.prefixes;
        for &len in &set.add_lens {
            if len > word.len() {
                break;
            }
            if !word.is_char_boundary(len) {
                continue;
            }
            let Some(groups) = set.by_add.get(&word[..len]) else {
                continue;
            };
            let rest = &word[len..];
            if rest.is_empty() && !self.aff.fullstrip {
                continue;
            }
            for g in groups {
                let stem = joined(&g.strip, rest);
                let found = self.words.contains(&stem);
                for &ri in &g.rules {
                    let p = &set.rules[ri as usize];
                    if (pos == Pos::End && !has(&p.cont, self.aff.compound_permit))
                        || !p.cond_start(&stem)
                    {
                        continue;
                    }
                    if found {
                        for root in self.words.get(&stem) {
                            let c = Cand {
                                root,
                                pfx: Some(p),
                                sfx: None,
                                sfx2: None,
                            };
                            if has(root, Some(p.flag)) && f(&c) {
                                return true;
                            }
                        }
                    }
                    if (p.cross || self.aff.suffix_cont.contains(&p.flag))
                        && self.suffixed(&stem, pos, Some(p), None, f)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Whether `word` is a compound of two or more parts (from the `parts`-th part on).
    fn compound(&self, word: &str, parts: usize, mode: Mode) -> bool {
        let a = &self.aff;
        if parts > 8 {
            return false;
        }
        let min = a.compound_min;
        let bounds: Vec<usize> = word.char_indices().map(|(i, _)| i).collect();
        let n = bounds.len();
        if n < 2 * min {
            return false;
        }
        let pos = if parts == 0 { Pos::Begin } else { Pos::Middle };
        for &at in &bounds[min..=n - min] {
            let (first, rest) = word.split_at(at);
            if !self.affixed(first, pos, &mut |c| self.part(c, pos, mode)) {
                continue;
            }
            if a.check_compound_triple && triple(first, rest) {
                continue;
            }
            if a.check_compound_case
                && (first.chars().next_back().is_some_and(char::is_uppercase)
                    || rest.chars().next().is_some_and(char::is_uppercase))
            {
                continue;
            }
            let room = |more: usize| a.compound_word_max.is_none_or(|m| parts + more <= m);
            if room(2)
                && !(a.check_compound_dup && first == rest)
                && self.affixed(rest, Pos::End, &mut |c| self.part(c, Pos::End, mode))
            {
                return true;
            }
            if room(3) && self.compound(rest, parts + 1, mode) {
                return true;
            }
        }
        false
    }

    /// Whether a candidate may be a compound part at `pos`.
    fn part(&self, c: &Cand<'_>, pos: Pos, mode: Mode) -> bool {
        let a = &self.aff;
        if has(c.root, a.forbidden)
            || has(c.root, HIDDEN.into())
            || (c.pfx.is_none() && c.sfx.is_none() && has(c.root, a.needaffix))
            || c.affixes().any(|x| has(&x.cont, a.compound_forbid))
            || (mode.suggest && c.has(a.nosuggest))
        {
            return false;
        }
        let place = match pos {
            Pos::Begin => a.compound_begin,
            Pos::Middle => a.compound_middle,
            Pos::End => a.compound_end,
            Pos::Alone => None,
        };
        c.has(a.compound_flag) || c.has(place)
    }

    /// Whether `word` (from part `parts`) matches compound rule elements `rule[ri..]`.
    fn rule(&self, word: &str, rule: &[(Flag, Quant)], ri: usize, parts: usize) -> bool {
        if word.is_empty() {
            return parts >= 2 && rule[ri..].iter().all(|(_, q)| *q != Quant::One);
        }
        let Some(&(flag, q)) = rule.get(ri) else {
            return false;
        };
        if q != Quant::One && self.rule(word, rule, ri + 1, parts) {
            return true;
        }
        let a = &self.aff;
        for (k, (i, c)) in word.char_indices().enumerate() {
            let end = i + c.len_utf8();
            if k + 1 < a.compound_min {
                continue;
            }
            let part = &word[..end];
            if self
                .words
                .get(part)
                .any(|r| has(r, Some(flag)) && !has(r, a.forbidden))
            {
                let next = if q == Quant::Any { ri } else { ri + 1 };
                if self.rule(&word[end..], rule, next, parts + 1) {
                    return true;
                }
            }
        }
        false
    }
}

/// Three equal letters across a compound boundary ("Schifffahrt" style).
fn triple(first: &str, rest: &str) -> bool {
    let a: Vec<char> = first.chars().rev().take(2).collect();
    let b: Vec<char> = rest.chars().take(2).collect();
    match (a.as_slice(), b.as_slice()) {
        ([x, y, ..], [z, ..]) if x == y && y == z => true,
        ([x, ..], [y, z, ..]) if x == y && y == z => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(aff: &str, dic: &str) -> Dictionary {
        Dictionary::from_bytes(aff.as_bytes(), dic.as_bytes()).unwrap()
    }

    #[test]
    fn affixes() {
        let d = dict(
            "SET UTF-8\nPFX U Y 1\nPFX U 0 un .\nSFX S Y 3\nSFX S y ies [^aeiou]y\nSFX S 0 s [aeiou]y\nSFX S 0 s [^y]\n\
             SFX D N 1\nSFX D 0 ed .\nPFX R N 1\nPFX R 0 re .\n",
            "5\nfly/S\nday/S\ndo/US\ncook/DR\nact/US\n",
        );
        for w in [
            "fly", "flies", "days", "undo", "dos", "undos", "unacts", "cooked", "recook",
        ] {
            assert!(d.check(w), "{w}");
        }
        for w in [
            "flys", "dayies", "unfly", "recooked", "uncooked", "unrecook",
        ] {
            assert!(!d.check(w), "{w}");
        }
    }

    #[test]
    fn special_flags() {
        let d = dict(
            "NEEDAFFIX N\nFORBIDDENWORD !\nKEEPCASE K\nONLYINCOMPOUND O\nCIRCUMFIX C\n\
             SFX S Y 1\nSFX S 0 s .\nPFX G Y 1\nPFX G 0 ge/C .\nSFX T Y 2\nSFX T 0 t/C .\nSFX T 0 en .\n",
            "6\nfoo/NS\nbar/S\nbars/!\nWiFi/K\nwurst/O\nmach/GT\n",
        );
        assert!(!d.check("foo") && d.check("foos"));
        assert!(d.check("bar") && !d.check("bars"));
        assert!(d.check("WiFi") && !d.check("WIFI") && !d.check("wifi"));
        assert!(!d.check("wurst"));
        assert!(d.check("gemacht") && d.check("machen"));
        assert!(!d.check("macht") && !d.check("gemachen"));
    }

    #[test]
    fn twofold_suffixes() {
        let d = dict(
            "SFX A Y 1\nSFX A 0 able/B .\nSFX B Y 1\nSFX B 0 ness .\n",
            "1\nread/A\n",
        );
        assert!(d.check("readable") && d.check("readableness") && !d.check("readness"));
    }

    #[test]
    fn casing() {
        let d = dict("", "4\nparis\nParis\nthe\nNASA\n");
        let d2 = dict("", "1\nLondon\n");
        assert!(d.check("The") && d.check("THE") && d.check("the"));
        assert!(!d.check("tHe"));
        assert!(d2.check("London") && d2.check("LONDON") && !d2.check("london"));
        assert!(d.check("NASA") && !d.check("Nasa") && !d.check("nasa"));
        let d3 = dict("", "1\nOpenOffice\n");
        assert!(d3.check("OpenOffice") && d3.check("OPENOFFICE") && !d3.check("Openoffice"));
        let sharp = dict("CHECKSHARPS\n", "1\nStraße\n");
        assert!(sharp.check("STRASSE") && sharp.check("Straße") && !sharp.check("Strasse"));
        let tr = dict("LANG tr_TR\n", "1\nistanbul\n");
        assert!(tr.check("İSTANBUL") && tr.check("İstanbul") && !tr.check("ISTANBUL"));
    }

    #[test]
    fn compounds() {
        let d = dict(
            "COMPOUNDFLAG X\nCOMPOUNDMIN 3\nONLYINCOMPOUND O\n",
            "4\nhaus/X\ntür/X\nbau\nzwi/OX\n",
        );
        assert!(d.check("haustür") && d.check("haustürhaus") && !d.check("hausbau"));
        assert!(d.check("zwihaus") && !d.check("zwi"));
        let d = dict(
            "COMPOUNDBEGIN B\nCOMPOUNDMIDDLE M\nCOMPOUNDEND E\nCOMPOUNDMIN 2\nCOMPOUNDPERMITFLAG P\n\
             SFX s Y 1\nSFX s 0 s/BP .\nSFX n Y 1\nSFX n 0 n .\n",
            "3\narbeit/s\nzimmer/En\nfirma/BM\n",
        );
        assert!(d.check("arbeitszimmer") && d.check("arbeitszimmern"));
        assert!(
            d.check("firmafirmazimmer") && !d.check("zimmerarbeit") && !d.check("arbeitzimmer")
        );
        let d = dict(
            "COMPOUNDFLAG X\nCHECKCOMPOUNDDUP\nCHECKCOMPOUNDTRIPLE\nCOMPOUNDMIN 1\n",
            "2\nfoo/X\nooh/X\n",
        );
        assert!(!d.check("foofoo") && !d.check("fooooh"));
    }

    #[test]
    fn compound_rules_and_breaks() {
        let d = dict(
            "COMPOUNDMIN 1\nONLYINCOMPOUND c\nCOMPOUNDRULE 2\nCOMPOUNDRULE n*1t\nCOMPOUNDRULE n*mp\n",
            "6\n0/nm\n1/n1\n2/nm\n1st/p\nth/tc\nwell\n",
        );
        assert!(d.check("1st") && d.check("21st") && d.check("11th") && d.check("201st"));
        assert!(!d.check("th") && !d.check("2th"));
        let d = dict("", "2\nwell\nknown\n");
        assert!(d.check("well-known") && !d.check("well-knwn") && d.check("12,345.6"));
    }

    #[test]
    fn encodings_and_conversions() {
        let aff = b"SET ISO8859-1\nICONV 1\nICONV \xb4 '\nIGNORE -\n";
        let d = Dictionary::from_bytes(aff, b"2\nca\xdf\ndon't\n").unwrap();
        assert!(d.check("caß") && d.check("don´t") && d.check("don't") && d.check("ca-ß"));
        assert!(d.check("don’t"));
    }

    #[test]
    fn dic_lines() {
        assert_eq!(split_entry("a\\/b/XY po:noun"), ("a/b".into(), "XY"));
        assert_eq!(split_entry("word\tst:x"), ("word".into(), ""));
        assert_eq!(split_entry("/slash"), ("/slash".into(), ""));
        assert!(is_number("1,234.5") && !is_number("1..2") && !is_number("-1") && !is_number("1."));
    }
}
