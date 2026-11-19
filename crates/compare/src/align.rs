//! Pairing the pages of two documents: an order-preserving alignment maximising the
//! similarity of their words, so inserted and deleted pages stand out. Pages with the
//! same text anchor the alignment; only the stretches between anchors are aligned by
//! dynamic programming.

use std::collections::HashMap;

use crate::text::DocText;

/// Pages below this similarity are not paired.
const MIN_SIMILARITY: f32 = 0.2;
/// Stretches larger than this (pages × pages) are aligned within a band around the
/// diagonal.
const FULL_DP: usize = 250_000;
const BAND: isize = 60;

/// Per page: its sorted word keys and a hash of the whole text.
struct Sig {
    keys: Vec<u64>,
    hash: u64,
}

fn sigs(t: &DocText) -> Vec<Sig> {
    (0..t.pages())
        .map(|p| {
            let w = t.page_words(p);
            let mut keys: Vec<u64> = w.iter().map(|w| w.key).collect();
            let hash = keys.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, k| {
                (h ^ k).wrapping_mul(0x100_0000_01b3)
            });
            keys.sort_unstable();
            Sig { keys, hash }
        })
        .collect()
}

/// Dice similarity of two sorted multisets; two pages without words are alike.
fn similarity(a: &[u64], b: &[u64]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.5;
    }
    let (mut i, mut j, mut common) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
        }
    }
    (2 * common) as f32 / (a.len() + b.len()) as f32
}

/// Pairs of page indices: `(Some(a), Some(b))` matched, `(Some(a), None)` deleted,
/// `(None, Some(b))` inserted; in the order of both documents.
pub fn align(a: &DocText, b: &DocText) -> Vec<(Option<usize>, Option<usize>)> {
    let (sa, sb) = (sigs(a), sigs(b));
    // Anchors: pages whose text is unique in both documents and identical.
    let mut count: HashMap<u64, (usize, usize, usize, usize)> = HashMap::new();
    for (i, s) in sa.iter().enumerate() {
        let e = count.entry(s.hash).or_default();
        e.0 += 1;
        e.2 = i;
    }
    for (j, s) in sb.iter().enumerate() {
        let e = count.entry(s.hash).or_default();
        e.1 += 1;
        e.3 = j;
    }
    let mut pairs: Vec<(usize, usize)> = count
        .values()
        .filter(|c| c.0 == 1 && c.1 == 1 && !sa[c.2].keys.is_empty())
        .map(|c| (c.2, c.3))
        .collect();
    pairs.sort_unstable();
    let anchors = lis(&pairs);
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (ai, bj) in anchors.into_iter().chain([(sa.len(), sb.len())]) {
        dp(&sa, &sb, i..ai, j..bj, &mut out);
        if ai < sa.len() && bj < sb.len() {
            out.push((Some(ai), Some(bj)));
        }
        (i, j) = (ai + 1, bj + 1);
    }
    out
}

/// The longest chain of pairs increasing in both coordinates (pairs sorted by the first).
fn lis(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; pairs.len()];
    for (k, &(_, b)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs[t].1 < b);
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

/// Aligns a stretch: maximises the summed similarity of paired pages.
fn dp(
    sa: &[Sig],
    sb: &[Sig],
    ra: std::ops::Range<usize>,
    rb: std::ops::Range<usize>,
    out: &mut Vec<(Option<usize>, Option<usize>)>,
) {
    let (n, m) = (ra.len(), rb.len());
    if n == 0 || m == 0 {
        out.extend(ra.map(|i| (Some(i), None)));
        out.extend(rb.map(|j| (None, Some(j))));
        return;
    }
    let banded = n * m > FULL_DP;
    let in_band = |i: usize, j: usize| {
        !banded || {
            let diag = (i as f64 * m as f64 / n as f64) as isize;
            (j as isize - diag).abs() <= BAND
        }
    };
    // score[i][j]: best over the first i and j pages; step: 0 diag, 1 skip a, 2 skip b.
    let w = m + 1;
    let mut score = vec![0f32; (n + 1) * w];
    let mut step = vec![0u8; (n + 1) * w];
    for i in 0..=n {
        for j in 0..=m {
            if i == 0 && j == 0 {
                continue;
            }
            let mut best = f32::MIN;
            let mut s = 0;
            if i > 0 && score[(i - 1) * w + j] > best {
                best = score[(i - 1) * w + j];
                s = 1;
            }
            if j > 0 && score[i * w + j - 1] > best {
                best = score[i * w + j - 1];
                s = 2;
            }
            if i > 0 && j > 0 && in_band(i - 1, j - 1) {
                let sim = similarity(&sa[ra.start + i - 1].keys, &sb[rb.start + j - 1].keys);
                if sim >= MIN_SIMILARITY && score[(i - 1) * w + j - 1] + sim > best {
                    best = score[(i - 1) * w + j - 1] + sim;
                    s = 0;
                }
            }
            score[i * w + j] = best;
            step[i * w + j] = s;
        }
    }
    let mut rev = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        match step[i * w + j] {
            0 if i > 0 && j > 0 => {
                rev.push((Some(ra.start + i - 1), Some(rb.start + j - 1)));
                i -= 1;
                j -= 1;
            }
            1 if i > 0 => {
                rev.push((Some(ra.start + i - 1), None));
                i -= 1;
            }
            _ if j > 0 => {
                rev.push((None, Some(rb.start + j - 1)));
                j -= 1;
            }
            _ => {
                rev.push((Some(ra.start + i - 1), None));
                i -= 1;
            }
        }
    }
    rev.reverse();
    out.extend(rev);
}
