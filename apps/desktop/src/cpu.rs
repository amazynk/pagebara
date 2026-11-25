//! The processor check. x86-64 builds target x86-64-v3 (AVX2, FMA, BMI1/2, F16C, LZCNT,
//! MOVBE; see `.cargo/config.toml`). On an older processor the first such instruction would
//! end the process with an illegal-instruction error and no explanation; this asks CPUID
//! first and explains instead.
//!
//! It runs before any other code of ours and avoids those instructions itself:
//! `is_x86_feature_detected!` cannot be used, as it answers "yes" at compile time when the
//! crate is built with the feature enabled. The message is English and names no app: the
//! translations are not loaded yet, and loading them is code that may already use AVX2.

#[cfg(target_arch = "x86_64")]
const MESSAGE: &str = "This app can't run on this processor.\n\nIt needs the x86-64-v3 \
     instruction set (AVX2, FMA and BMI2): Intel Haswell from 2013 or later, AMD Zen or later.";

/// The message box's title and text, as NUL-terminated UTF-16 made at compile time, so
/// nothing converts them at run time.
#[cfg(target_arch = "x86_64")]
static BOX_TITLE: [u16; pagebara_identity::NAME.len() + 1] = ascii_utf16(pagebara_identity::NAME);
#[cfg(target_arch = "x86_64")]
static BOX_TEXT: [u16; MESSAGE.len() + 1] = ascii_utf16(MESSAGE);

/// `s` as NUL-terminated UTF-16. ASCII only (checked when compiling), so each byte is one
/// code unit; `N` is the length plus the NUL.
#[cfg(target_arch = "x86_64")]
const fn ascii_utf16<const N: usize>(s: &str) -> [u16; N] {
    let b = s.as_bytes();
    assert!(b.len() + 1 == N, "N is the length plus the NUL");
    let mut w = [0u16; N];
    let mut i = 0;
    while i < b.len() {
        assert!(b[i] < 0x80, "ASCII only");
        w[i] = b[i] as u16;
        i += 1;
    }
    w
}

/// The x86-64-v3 features `cpuid(leaf, subleaf) → [eax, ebx, ecx, edx]` reports missing.
#[cfg(target_arch = "x86_64")]
fn missing(cpuid: impl Fn(u32, u32) -> [u32; 4]) -> Vec<&'static str> {
    let bit = |r: u32, b: u32| r >> b & 1 == 1;
    let max = cpuid(0, 0)[0];
    let l1 = cpuid(1, 0);
    let l7 = if max >= 7 { cpuid(7, 0) } else { [0; 4] };
    let e1 = if cpuid(0x8000_0000, 0)[0] >= 0x8000_0001 {
        cpuid(0x8000_0001, 0)
    } else {
        [0; 4]
    };
    let (ecx1, ebx7, ecx_e1) = (l1[2], l7[1], e1[2]);
    [
        // The OS saves the extended (YMM) register state: without it AVX is unusable.
        ("OSXSAVE", bit(ecx1, 27)),
        ("AVX", bit(ecx1, 28)),
        ("AVX2", bit(ebx7, 5)),
        ("FMA", bit(ecx1, 12)),
        ("F16C", bit(ecx1, 29)),
        ("BMI1", bit(ebx7, 3)),
        ("BMI2", bit(ebx7, 8)),
        ("LZCNT", bit(ecx_e1, 5)),
        ("MOVBE", bit(ecx1, 22)),
    ]
    .into_iter()
    .filter(|c| !c.1)
    .map(|c| c.0)
    .collect()
}

/// Ends the process with an explanation when the processor lacks what the build needs: a
/// line on the terminal, and on Windows a message box when there is no terminal (started
/// from Explorer or the Start menu).
#[cfg(target_arch = "x86_64")]
#[inline(never)]
pub fn require() {
    let missing = missing(|leaf, sub| {
        let r = std::arch::x86_64::__cpuid_count(leaf, sub);
        [r.eax, r.ebx, r.ecx, r.edx]
    });
    if missing.is_empty() {
        return;
    }
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr(),
        "{} can't run on this processor: it needs the x86-64-v3 instruction set (Intel \
         Haswell from 2013 or later, AMD Zen or later), and this processor lacks {}.",
        pagebara_identity::NAME,
        missing.join(", ")
    );
    if !pagebara_native::has_terminal() {
        pagebara_native::error_box(&BOX_TITLE, &BOX_TEXT);
    }
    std::process::exit(1);
}

#[cfg(not(target_arch = "x86_64"))]
pub fn require() {}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn an_old_processor_is_told_what_it_lacks() {
        let nehalem = |leaf: u32, _sub: u32| match leaf {
            0 => [0xb, 0, 0, 0],
            // SSE3, SSSE3, SSE4.1/4.2, POPCNT; no FMA, MOVBE, OSXSAVE, AVX, F16C.
            1 => [0, 0, 1 | 1 << 9 | 1 << 19 | 1 << 20 | 1 << 23, 0],
            7 => [0; 4],
            0x8000_0000 => [0x8000_0008, 0, 0, 0],
            _ => [0; 4],
        };
        assert_eq!(
            missing(nehalem),
            [
                "OSXSAVE", "AVX", "AVX2", "FMA", "F16C", "BMI1", "BMI2", "LZCNT", "MOVBE"
            ]
        );
    }

    #[test]
    fn the_processor_running_the_tests_passes() {
        let real = missing(|l, s| {
            let r = std::arch::x86_64::__cpuid_count(l, s);
            [r.eax, r.ebx, r.ecx, r.edx]
        });
        assert!(real.is_empty(), "{real:?}");
    }

    #[test]
    fn the_box_text_is_the_message() {
        let text = |w: &[u16]| {
            assert_eq!(w.last(), Some(&0));
            String::from_utf16(&w[..w.len() - 1]).unwrap()
        };
        assert_eq!(text(&BOX_TITLE), pagebara_identity::NAME);
        assert_eq!(text(&BOX_TEXT), MESSAGE);
    }
}
