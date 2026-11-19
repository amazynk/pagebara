//! A diff of two sequences of word keys: common ends trimmed, then words unique to both
//! sides as anchors (patience diff), then Myers' algorithm for what lies between, bounded
//! so that very different stretches count as replaced.

use std::collections::HashMap;
use std::ops::Range;

/// Myers' search gives up beyond this many edits in one stretch.
const MAX_EDITS: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Equal(Range<usize>, Range<usize>),
    Delete(Range<usize>),
    Insert(Range<usize>),
}

enum Work {
    Diff(Range<usize>, Range<usize>),
    Equal(Range<usize>, Range<usize>),
}

/// Edits turning `a` into `b`, in order; neighbouring edits of one kind are merged.
pub fn diff(a: &[u64], b: &[u64]) -> Vec<Edit> {
    let mut out = Vec::new();
    // Pending work in reverse order: the next piece of the result is on top.
    let mut stack = vec![Work::Diff(0..a.len(), 0..b.len())];
    while let Some(w) = stack.pop() {
        let (mut ra, mut rb) = match w {
            Work::Equal(x, y) => {
                push(&mut out, Edit::Equal(x, y));
                continue;
            }
            Work::Diff(x, y) => (x, y),
        };
        let pre = a[ra.clone()]
            .iter()
            .zip(&b[rb.clone()])
            .take_while(|(x, y)| x == y)
            .count();
        if pre > 0 {
            push(
                &mut out,
                Edit::Equal(ra.start..ra.start + pre, rb.start..rb.start + pre),
            );
            ra.start += pre;
            rb.start += pre;
        }
        let suf = a[ra.clone()]
            .iter()
            .rev()
            .zip(b[rb.clone()].iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        if suf > 0 {
            stack.push(Work::Equal(ra.end - suf..ra.end, rb.end - suf..rb.end));
            ra.end -= suf;
            rb.end -= suf;
        }
        if ra.is_empty() || rb.is_empty() {
            if !ra.is_empty() {
                push(&mut out, Edit::Delete(ra));
            }
            if !rb.is_empty() {
                push(&mut out, Edit::Insert(rb));
            }
            continue;
        }
        let anchors = unique_anchors(a, b, ra.clone(), rb.clone());
        if anchors.is_empty() {
            myers(a, b, ra, rb, &mut out);
            continue;
        }
        let mut parts = Vec::with_capacity(anchors.len() * 2 + 1);
        let (mut i, mut j) = (ra.start, rb.start);
        for (x, y) in anchors {
            parts.push(Work::Diff(i..x, j..y));
            parts.push(Work::Equal(x..x + 1, y..y + 1));
            (i, j) = (x + 1, y + 1);
        }
        parts.push(Work::Diff(i..ra.end, j..rb.end));
        stack.extend(parts.into_iter().rev());
    }
    out
}

fn push(out: &mut Vec<Edit>, e: Edit) {
    match (out.last_mut(), &e) {
        (_, Edit::Equal(a, _)) if a.is_empty() => {}
        (Some(Edit::Equal(a, b)), Edit::Equal(c, d)) if a.end == c.start && b.end == d.start => {
            a.end = c.end;
            b.end = d.end;
        }
        (Some(Edit::Delete(a)), Edit::Delete(c)) if a.end == c.start => a.end = c.end,
        (Some(Edit::Insert(a)), Edit::Insert(c)) if a.end == c.start => a.end = c.end,
        _ => out.push(e),
    }
}

/// Words occurring once in each stretch, paired, longest increasing chain.
fn unique_anchors(a: &[u64], b: &[u64], ra: Range<usize>, rb: Range<usize>) -> Vec<(usize, usize)> {
    let mut seen: HashMap<u64, (u32, u32, usize, usize)> = HashMap::new();
    for i in ra {
        let e = seen.entry(a[i]).or_default();
        e.0 += 1;
        e.2 = i;
    }
    for j in rb {
        if let Some(e) = seen.get_mut(&b[j]) {
            e.1 += 1;
            e.3 = j;
        }
    }
    let mut pairs: Vec<(usize, usize)> = seen
        .values()
        .filter(|e| e.0 == 1 && e.1 == 1)
        .map(|e| (e.2, e.3))
        .collect();
    pairs.sort_unstable();
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; pairs.len()];
    for (k, &(_, y)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs[t].1 < y);
        if pos > 0 {
            prev[k] = tails[pos - 1];
        }
        if pos == tails.len() {
            tails.push(k);
        } else {
            tails[pos] = k;
        }
    }
    let mut out = Vec::new();
    let mut k = tails.last().copied().unwrap_or(usize::MAX);
    while k != usize::MAX {
        out.push(pairs[k]);
        k = prev[k];
    }
    out.reverse();
    out
}

/// Myers' O(ND) diff of a stretch; beyond [`MAX_EDITS`] the stretch is replaced whole.
fn myers(a: &[u64], b: &[u64], ra: Range<usize>, rb: Range<usize>, out: &mut Vec<Edit>) {
    let (n, m) = (ra.len() as isize, rb.len() as isize);
    let max = ((n + m) as usize).min(MAX_EDITS) as isize;
    let off = max + 1;
    let at = |k: isize| (off + k) as usize;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    // v as it was before each round d.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'outer: for d in 0..=max {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let down = k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]);
            let mut x = if down { v[at(k + 1)] } else { v[at(k - 1)] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[ra.start + x as usize] == b[rb.start + y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }
    let Some(dmax) = found else {
        push(out, Edit::Delete(ra));
        push(out, Edit::Insert(rb));
        return;
    };
    let eq = |x: isize, y: isize| {
        Edit::Equal(
            ra.start + x as usize..ra.start + x as usize + 1,
            rb.start + y as usize..rb.start + y as usize + 1,
        )
    };
    let mut rev: Vec<Edit> = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=dmax).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let down = k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]);
        let pk = if down { k + 1 } else { k - 1 };
        let px = v[at(pk)];
        let py = px - pk;
        let (sx, sy) = if down { (px, py + 1) } else { (px + 1, py) };
        while x > sx && y > sy {
            x -= 1;
            y -= 1;
            rev.push(eq(x, y));
        }
        if down {
            rev.push(Edit::Insert(
                rb.start + py as usize..rb.start + py as usize + 1,
            ));
        } else {
            rev.push(Edit::Delete(
                ra.start + px as usize..ra.start + px as usize + 1,
            ));
        }
        (x, y) = (px, py);
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        rev.push(eq(x, y));
    }
    for e in rev.into_iter().rev() {
        push(out, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Vec<u64> {
        s.split_whitespace().map(crate::text::key).collect()
    }

    /// Applies the edits and checks they rebuild `b` from `a`.
    fn check(a: &str, b: &str) -> Vec<Edit> {
        let (ka, kb) = (keys(a), keys(b));
        let e = diff(&ka, &kb);
        let mut rebuilt = Vec::new();
        let (mut i, mut j) = (0, 0);
        for x in &e {
            match x {
                Edit::Equal(r, s) => {
                    assert_eq!(r.start, i);
                    assert_eq!(s.start, j);
                    assert_eq!(ka[r.clone()], kb[s.clone()]);
                    rebuilt.extend_from_slice(&ka[r.clone()]);
                    (i, j) = (r.end, s.end);
                }
                Edit::Delete(r) => {
                    assert_eq!(r.start, i);
                    i = r.end;
                }
                Edit::Insert(s) => {
                    assert_eq!(s.start, j);
                    rebuilt.extend_from_slice(&kb[s.clone()]);
                    j = s.end;
                }
            }
        }
        assert_eq!((i, j), (ka.len(), kb.len()));
        assert_eq!(rebuilt, kb);
        e
    }

    #[test]
    fn edits() {
        let e = check("a b c d e", "a b x d e");
        assert_eq!(
            e,
            vec![
                Edit::Equal(0..2, 0..2),
                Edit::Delete(2..3),
                Edit::Insert(2..3),
                Edit::Equal(3..5, 3..5)
            ]
        );
        check("", "a b");
        check("a b", "");
        check("the cat sat on the mat", "the dog sat on the mat today");
        check("x x x y y y", "y y y x x x");
        check("a b c a b c a b c", "a b a b c c a c b");
        // Repeated words only (no anchors): Myers.
        check("a a b a b b a", "b a a b b a a a");
    }

    #[test]
    fn long_texts() {
        let a: String = (0..5000).map(|i| format!("w{} ", i % 97)).collect();
        let mut b = a.clone();
        b.insert_str(300, " inserted words here ");
        b = b.replace("w50 w51", "w50 changed w51");
        check(&a, &b);
    }
}
