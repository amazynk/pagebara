use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Which pages a stamp goes on: a list of 1-based pages and ranges (`1-3, 7, 10-`; empty
/// for all), then only the odd or even ones of those if asked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageSet {
    pub list: String,
    pub odd: bool,
    pub even: bool,
}

impl PageSet {
    pub fn all() -> PageSet {
        PageSet::default()
    }

    pub fn list(s: &str) -> PageSet {
        PageSet {
            list: s.into(),
            ..PageSet::default()
        }
    }

    /// The chosen pages (0-based, ascending, no repeats) of a document with `count`.
    pub fn resolve(&self, count: usize) -> Result<Vec<usize>> {
        let mut pick = vec![false; count];
        let list = self.list.trim();
        if list.is_empty() || list.eq_ignore_ascii_case("all") {
            pick.fill(true);
        } else {
            for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let num = |s: &str| -> Result<usize> {
                    let n: usize = s
                        .trim()
                        .parse()
                        .map_err(|_| Error::Pages(format!("not a page number: {s}")))?;
                    if n == 0 || n > count {
                        return Err(Error::Pages(format!(
                            "page {n} does not exist (the document has {count})"
                        )));
                    }
                    Ok(n - 1)
                };
                let (a, b) = match part.split_once('-') {
                    Some((a, b)) => (
                        if a.trim().is_empty() { 0 } else { num(a)? },
                        if b.trim().is_empty() {
                            count.saturating_sub(1)
                        } else {
                            num(b)?
                        },
                    ),
                    None => {
                        let n = num(part)?;
                        (n, n)
                    }
                };
                for p in pick.iter_mut().take(b.max(a) + 1).skip(a.min(b)) {
                    *p = true;
                }
            }
        }
        let out: Vec<usize> = (0..count)
            .filter(|&i| pick[i])
            .filter(|i| !(self.odd && !self.even) || i % 2 == 0)
            .filter(|i| !(self.even && !self.odd) || i % 2 == 1)
            .collect();
        if out.is_empty() {
            return Err(Error::NoPages);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(PageSet::all().resolve(3).unwrap(), vec![0, 1, 2]);
        assert_eq!(PageSet::list("2-3, 5").resolve(6).unwrap(), vec![1, 2, 4]);
        assert_eq!(PageSet::list("4-").resolve(6).unwrap(), vec![3, 4, 5]);
        let odd = PageSet {
            odd: true,
            ..PageSet::default()
        };
        assert_eq!(odd.resolve(5).unwrap(), vec![0, 2, 4]);
        let even = PageSet {
            even: true,
            ..PageSet::list("1-4")
        };
        assert_eq!(even.resolve(6).unwrap(), vec![1, 3]);
        assert!(PageSet::list("9").resolve(3).is_err());
        assert!(PageSet::list("x").resolve(3).is_err());
    }
}
