//! The affix file: flags, prefix and suffix rules, compounding and suggestion settings.

use std::collections::{HashMap, HashSet};

pub(crate) type Flag = u16;

/// A sorted set of flags.
pub(crate) type Flags = Box<[Flag]>;

pub(crate) fn has(flags: &[Flag], f: Option<Flag>) -> bool {
    let Some(f) = f else { return false };
    if flags.len() < 8 {
        flags.contains(&f)
    } else {
        flags.binary_search(&f).is_ok()
    }
}

/// How flags are written (`FLAG`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FlagMode {
    #[default]
    Char,
    /// Two characters per flag.
    Long,
    /// Decimal numbers separated by commas.
    Num,
    Utf8,
}

impl FlagMode {
    fn units(self, s: &str) -> Vec<Flag> {
        match self {
            FlagMode::Char | FlagMode::Utf8 => s.chars().map(|c| c as u32 as Flag).collect(),
            FlagMode::Long => {
                let cs: Vec<char> = s.chars().collect();
                cs.chunks(2)
                    .map(|p| match p {
                        [a, b] => (*a as u32 * 256 + *b as u32) as Flag,
                        [a] => *a as u32 as Flag,
                        _ => 0,
                    })
                    .collect()
            }
            FlagMode::Num => s.split(',').filter_map(|n| n.trim().parse().ok()).collect(),
        }
    }

    pub(crate) fn one(self, s: &str) -> Option<Flag> {
        self.units(s).first().copied()
    }
}

/// One element of an affix condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cond {
    Any,
    Char(char),
    Set(bool, Box<[char]>),
}

impl Cond {
    fn matches(&self, c: char) -> bool {
        match self {
            Cond::Any => true,
            Cond::Char(x) => *x == c,
            Cond::Set(neg, set) => set.contains(&c) != *neg,
        }
    }
}

fn parse_cond(s: &str) -> Box<[Cond]> {
    if s == "." {
        return Box::new([]);
    }
    let mut out = Vec::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '.' => out.push(Cond::Any),
            '[' => {
                let mut set = Vec::new();
                let mut neg = false;
                for (i, c) in it.by_ref().enumerate() {
                    match c {
                        ']' => break,
                        '^' if i == 0 => neg = true,
                        c => set.push(c),
                    }
                }
                out.push(Cond::Set(neg, set.into()));
            }
            c => out.push(Cond::Char(c)),
        }
    }
    out.into()
}

/// A prefix or suffix rule.
#[derive(Clone, Debug)]
pub(crate) struct Affix {
    pub flag: Flag,
    pub cross: bool,
    pub strip: Box<str>,
    pub add: Box<str>,
    pub cond: Box<[Cond]>,
    /// Continuation flags: further affixes, compounding and other flags of the affixed word.
    pub cont: Flags,
}

impl Affix {
    /// Whether the condition holds at the end of `stem` (suffixes).
    pub(crate) fn cond_end(&self, stem: &str) -> bool {
        let mut cs = stem.chars().rev();
        self.cond
            .iter()
            .rev()
            .all(|c| cs.next().is_some_and(|ch| c.matches(ch)))
    }

    /// Whether the condition holds at the start of `stem` (prefixes).
    pub(crate) fn cond_start(&self, stem: &str) -> bool {
        let mut cs = stem.chars();
        self.cond
            .iter()
            .all(|c| cs.next().is_some_and(|ch| c.matches(ch)))
    }
}

/// Affix rules sharing their added and stripped text.
#[derive(Debug, Default)]
pub(crate) struct Group {
    pub strip: Box<str>,
    pub rules: Vec<u32>,
}

/// Rules of one kind, indexed by the text they add.
#[derive(Debug, Default)]
pub(crate) struct AffixSet {
    pub rules: Vec<Affix>,
    pub by_add: HashMap<Box<str>, Vec<Group>>,
    /// Distinct byte lengths of the added texts, ascending.
    pub add_lens: Vec<usize>,
}

impl AffixSet {
    fn index(&mut self) {
        let mut lens = HashSet::new();
        for (i, r) in self.rules.iter().enumerate() {
            lens.insert(r.add.len());
            let groups = self.by_add.entry(r.add.clone()).or_default();
            match groups.iter_mut().find(|g| g.strip == r.strip) {
                Some(g) => g.rules.push(i as u32),
                None => groups.push(Group {
                    strip: r.strip.clone(),
                    rules: vec![i as u32],
                }),
            }
        }
        self.add_lens = lens.into_iter().collect();
        self.add_lens.sort_unstable();
    }
}

/// One element of a compound rule: a flag, optionally repeated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quant {
    One,
    /// `?`
    Optional,
    /// `*`
    Any,
}

/// Replacement tables (`ICONV`, `OCONV`), longest match first.
#[derive(Debug, Default)]
pub(crate) struct Conv {
    pairs: Vec<(Box<str>, Box<str>)>,
}

impl Conv {
    pub(crate) fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub(crate) fn apply(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        'outer: while let Some(c) = rest.chars().next() {
            for (from, to) in &self.pairs {
                if rest.starts_with(&**from) {
                    out.push_str(to);
                    rest = &rest[from.len()..];
                    continue 'outer;
                }
            }
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
        out
    }
}

/// A `REP` replacement: anchored at the start and/or the end of the word.
#[derive(Clone, Debug)]
pub(crate) struct Rep {
    pub from: Box<str>,
    pub to: Box<str>,
    pub start: bool,
    pub end: bool,
}

/// Everything in an affix file.
#[derive(Debug, Default)]
pub(crate) struct Aff {
    pub flag_mode: FlagMode,
    pub aliases: Vec<Flags>,
    pub prefixes: AffixSet,
    pub suffixes: AffixSet,
    /// Flags appearing in some suffix's continuation flags (twofold suffixes).
    pub suffix_cont: HashSet<Flag>,
    pub needaffix: Option<Flag>,
    pub forbidden: Option<Flag>,
    pub keepcase: Option<Flag>,
    pub nosuggest: Option<Flag>,
    pub onlyincompound: Option<Flag>,
    pub circumfix: Option<Flag>,
    pub compound_flag: Option<Flag>,
    pub compound_begin: Option<Flag>,
    pub compound_middle: Option<Flag>,
    pub compound_end: Option<Flag>,
    pub compound_permit: Option<Flag>,
    pub compound_forbid: Option<Flag>,
    pub compound_min: usize,
    pub compound_word_max: Option<usize>,
    pub compound_rules: Vec<Vec<(Flag, Quant)>>,
    pub check_compound_dup: bool,
    pub check_compound_triple: bool,
    pub check_compound_case: bool,
    pub check_sharps: bool,
    pub fullstrip: bool,
    pub nosplitsugs: bool,
    pub try_chars: String,
    pub key: String,
    pub rep: Vec<Rep>,
    pub map: Vec<Vec<Box<str>>>,
    pub breaks: Vec<Box<str>>,
    pub iconv: Conv,
    pub oconv: Conv,
    pub ignore: Vec<char>,
    pub word_chars: String,
    pub lang: Option<String>,
}

impl Aff {
    pub(crate) fn compounds(&self) -> bool {
        self.compound_flag.is_some() || self.compound_begin.is_some()
    }

    /// The sorted flags of a dictionary or continuation field (an alias number or flags).
    pub(crate) fn flags(&self, s: &str) -> Flags {
        if !self.aliases.is_empty()
            && let Ok(n) = s.trim().parse::<usize>()
        {
            return self
                .aliases
                .get(n.wrapping_sub(1))
                .cloned()
                .unwrap_or_default();
        }
        let mut v = self.flag_mode.units(s);
        v.sort_unstable();
        v.dedup();
        v.into()
    }

    pub(crate) fn strip_ignored(&self, s: &str) -> String {
        if self.ignore.is_empty() {
            s.to_owned()
        } else {
            s.chars().filter(|c| !self.ignore.contains(c)).collect()
        }
    }

    pub(crate) fn parse(text: &str) -> Aff {
        let mut a = Aff {
            compound_min: 3,
            key: "qwertyuiop|asdfghjkl|zxcvbnm".into(),
            ..Aff::default()
        };
        let mut breaks: Option<Vec<Box<str>>> = None;
        let mut iconv = Vec::new();
        let mut oconv = Vec::new();
        // The affix class being read: suffix?, flag, cross product, rules left.
        let mut class: Option<(bool, Flag, bool, usize)> = None;
        let lines: Vec<&str> = text.lines().collect();
        // FLAG and AF must be known before any flag is read. The first AF line is the count.
        let mut af_count = false;
        for l in &lines {
            let t: Vec<&str> = l.split_whitespace().collect();
            match t.as_slice() {
                ["FLAG", m, ..] => {
                    a.flag_mode = match *m {
                        "long" => FlagMode::Long,
                        "num" => FlagMode::Num,
                        "UTF-8" => FlagMode::Utf8,
                        _ => FlagMode::Char,
                    }
                }
                ["AF", f, ..] if af_count => {
                    let mut v = a.flag_mode.units(f);
                    v.sort_unstable();
                    v.dedup();
                    a.aliases.push(v.into());
                }
                ["AF", ..] => af_count = true,
                _ => {}
            }
        }
        for l in lines {
            let t: Vec<&str> = l.split_whitespace().collect();
            let Some(&key) = t.first() else { continue };
            let one = |i: usize| t.get(i).and_then(|s| a.flag_mode.one(s));
            match key {
                "PFX" | "SFX" if t.len() >= 4 => {
                    let suffix = key == "SFX";
                    let Some(flag) = one(1) else { continue };
                    match class {
                        Some((s, f, cross, left)) if s == suffix && f == flag && left > 0 => {
                            class = Some((s, f, cross, left - 1));
                            let strip = if t[2] == "0" { "" } else { t[2] };
                            let (add, cont) = match t[3].split_once('/') {
                                Some((add, cont)) => (add, a.flags(cont)),
                                None => (t[3], Flags::default()),
                            };
                            let add = if add == "0" { "" } else { add };
                            let rule = Affix {
                                flag,
                                cross,
                                strip: a.strip_ignored(strip).into(),
                                add: a.strip_ignored(add).into(),
                                cond: parse_cond(t.get(4).copied().unwrap_or(".")),
                                cont,
                            };
                            if suffix {
                                a.suffix_cont.extend(rule.cont.iter().copied());
                                a.suffixes.rules.push(rule);
                            } else {
                                a.prefixes.rules.push(rule);
                            }
                        }
                        _ => {
                            let n = t[3].parse().unwrap_or(0);
                            class = Some((suffix, flag, t[2] == "Y", n));
                        }
                    }
                }
                "NEEDAFFIX" | "PSEUDOROOT" => a.needaffix = one(1),
                "FORBIDDENWORD" => a.forbidden = one(1),
                "KEEPCASE" => a.keepcase = one(1),
                "NOSUGGEST" => a.nosuggest = one(1),
                "ONLYINCOMPOUND" => a.onlyincompound = one(1),
                "CIRCUMFIX" => a.circumfix = one(1),
                "COMPOUNDFLAG" => a.compound_flag = one(1),
                "COMPOUNDBEGIN" => a.compound_begin = one(1),
                "COMPOUNDMIDDLE" => a.compound_middle = one(1),
                "COMPOUNDEND" | "COMPOUNDLAST" => a.compound_end = one(1),
                "COMPOUNDPERMITFLAG" => a.compound_permit = one(1),
                "COMPOUNDFORBIDFLAG" => a.compound_forbid = one(1),
                "COMPOUNDMIN" => {
                    a.compound_min = t.get(1).and_then(|n| n.parse().ok()).unwrap_or(3).max(1)
                }
                "COMPOUNDWORDMAX" => a.compound_word_max = t.get(1).and_then(|n| n.parse().ok()),
                "COMPOUNDRULE" if t.len() >= 2 && t[1].parse::<usize>().is_err() => {
                    let rule = a.compound_rule(t[1]);
                    if !rule.is_empty() {
                        a.compound_rules.push(rule);
                    }
                }
                "CHECKCOMPOUNDDUP" => a.check_compound_dup = true,
                "CHECKCOMPOUNDTRIPLE" => a.check_compound_triple = true,
                "CHECKCOMPOUNDCASE" => a.check_compound_case = true,
                "CHECKSHARPS" => a.check_sharps = true,
                "FULLSTRIP" => a.fullstrip = true,
                "NOSPLITSUGS" => a.nosplitsugs = true,
                "TRY" if t.len() >= 2 => a.try_chars = t[1].into(),
                "KEY" if t.len() >= 2 => a.key = t[1].into(),
                "WORDCHARS" if t.len() >= 2 => a.word_chars = t[1].into(),
                "IGNORE" if t.len() >= 2 => a.ignore = t[1].chars().collect(),
                "LANG" if t.len() >= 2 => a.lang = Some(t[1].into()),
                "REP" if t.len() >= 3 => {
                    let (mut from, to) = (t[1], t[2].replace('_', " "));
                    let start = from.starts_with('^');
                    if start {
                        from = &from[1..];
                    }
                    let end = from.len() > 1 && from.ends_with('$');
                    if end {
                        from = &from[..from.len() - 1];
                    }
                    if !from.is_empty() {
                        a.rep.push(Rep {
                            from: from.into(),
                            to: to.into(),
                            start,
                            end,
                        });
                    }
                }
                "MAP" if t.len() >= 2 && t[1].parse::<usize>().is_err() => {
                    let mut items: Vec<Box<str>> = Vec::new();
                    let mut it = t[1].chars();
                    while let Some(c) = it.next() {
                        if c == '(' {
                            let s: String = it.by_ref().take_while(|&c| c != ')').collect();
                            items.push(s.into());
                        } else {
                            items.push(c.to_string().into());
                        }
                    }
                    items.retain(|i| !i.is_empty());
                    if items.len() > 1 {
                        a.map.push(items);
                    }
                }
                "BREAK" if t.len() >= 2 && t[1].parse::<usize>().is_err() => {
                    breaks.get_or_insert_with(Vec::new).push(t[1].into())
                }
                "BREAK" => {
                    breaks.get_or_insert_with(Vec::new);
                }
                "ICONV" | "OCONV" if t.len() >= 3 => {
                    let pair = (t[1].into(), t[2].replace('_', " ").into());
                    if key == "ICONV" {
                        iconv.push(pair)
                    } else {
                        oconv.push(pair)
                    }
                }
                _ => {}
            }
        }
        a.breaks = breaks.unwrap_or_else(|| vec!["-".into(), "^-".into(), "-$".into()]);
        for (conv, mut pairs) in [(&mut a.iconv, iconv), (&mut a.oconv, oconv)] {
            pairs.sort_by_key(|(f, _): &(Box<str>, Box<str>)| std::cmp::Reverse(f.len()));
            conv.pairs = pairs;
        }
        a.prefixes.index();
        a.suffixes.index();
        a
    }

    fn compound_rule(&self, s: &str) -> Vec<(Flag, Quant)> {
        let mut out: Vec<(Flag, Quant)> = Vec::new();
        let cs: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < cs.len() {
            match cs[i] {
                '*' | '?' => {
                    if let Some(last) = out.last_mut() {
                        last.1 = if cs[i] == '*' {
                            Quant::Any
                        } else {
                            Quant::Optional
                        };
                    }
                    i += 1;
                }
                '(' => {
                    let end = cs[i..]
                        .iter()
                        .position(|&c| c == ')')
                        .map_or(cs.len(), |p| i + p);
                    let inner: String = cs[i + 1..end].iter().collect();
                    if let Some(f) = self.flag_mode.one(&inner) {
                        out.push((f, Quant::One));
                    }
                    i = end + 1;
                }
                _ => {
                    let w = if self.flag_mode == FlagMode::Long {
                        2
                    } else {
                        1
                    };
                    let unit: String = cs[i..(i + w).min(cs.len())].iter().collect();
                    if let Some(f) = self.flag_mode.one(&unit) {
                        out.push((f, Quant::One));
                    }
                    i += w;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affix_classes_and_conditions() {
        let a = Aff::parse(
            "SET UTF-8\nTRY abc\nSFX S Y 2\nSFX S y ies [^aeiou]y\nSFX S 0 s [^y]\n\
             PFX U N 1\nPFX U 0 un/X .\nKEEPCASE K\nREP 1\nREP ^f ph\nMAP 1\nMAP u(ue)ü\n",
        );
        assert_eq!(a.suffixes.rules.len(), 2);
        assert_eq!(a.prefixes.rules.len(), 1);
        let ies = &a.suffixes.rules[0];
        assert_eq!((&*ies.strip, &*ies.add), ("y", "ies"));
        assert!(ies.cond_end("fly") && !ies.cond_end("day") && !ies.cond_end("y"));
        assert!(a.suffixes.rules[1].cond_end("cat"));
        let un = &a.prefixes.rules[0];
        assert!(!un.cross && *un.cont == [b'X' as Flag]);
        assert_eq!(a.keepcase, Some(b'K' as Flag));
        assert_eq!(a.suffixes.add_lens, [1, 3]);
        assert!(a.rep[0].start && &*a.rep[0].from == "f");
        assert_eq!(a.map[0].len(), 3);
        assert_eq!(a.breaks.len(), 3);
    }

    #[test]
    fn flag_modes_and_aliases() {
        let a = Aff::parse("FLAG long\nCOMPOUNDFLAG Cx\nCOMPOUNDRULE 1\nCOMPOUNDRULE (N1)*(Nd)?\n");
        assert_eq!(&*a.flags("AaBb"), [0x4161, 0x4262]);
        assert_eq!(a.compound_flag, Some(0x4378));
        assert_eq!(
            a.compound_rules[0],
            [(0x4e31, Quant::Any), (0x4e64, Quant::Optional)]
        );
        let a = Aff::parse("FLAG num\nAF 2\nAF 5,3\nAF 7\nFORBIDDENWORD 9\n");
        assert_eq!(&*a.flags("1"), [3, 5]);
        assert_eq!(&*a.flags("2"), [7]);
        assert_eq!(a.forbidden, Some(9));
        let a = Aff::parse("ICONV 2\nICONV ’ '\nICONV ab x\n");
        assert_eq!(a.iconv.apply("don’t abc"), "don't xc");
    }
}
