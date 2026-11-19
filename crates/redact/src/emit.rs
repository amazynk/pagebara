//! Writing content stream operators back out.

use std::io::Write;

use pagebara_content::Instr;
use pagebara_content::lex::Operand;
use pagebara_syntax::{Object, write_object};

use crate::geom::Point;
use crate::path::{Seg, Sub};

/// Appends a number without exponent and with at most six decimals.
pub(crate) fn num(out: &mut Vec<u8>, v: f64) {
    let v = if v.is_finite() { v } else { 0.0 };
    let r = v.round();
    if (v - r).abs() < 1e-7 && r.abs() < 1e15 {
        let _ = write!(out, "{}", r as i64);
        return;
    }
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    out.extend_from_slice(if s == "-0" { b"0" } else { s.as_bytes() });
}

pub(crate) fn nums(out: &mut Vec<u8>, vs: &[f64]) {
    for &v in vs {
        num(out, v);
        out.push(b' ');
    }
}

pub(crate) fn operand(out: &mut Vec<u8>, i: &Instr<'_, '_>, o: &Operand<'_>) {
    match *o {
        Operand::Number(n) => {
            // f32 prints its shortest round-trip form, without exponent.
            let n = if n.is_finite() { n } else { 0.0 };
            let _ = write!(out, "{n}");
        }
        Operand::Bool(b) => out.extend_from_slice(if b { b"true" } else { b"false" }),
        Operand::Null => out.extend_from_slice(b"null"),
        Operand::Name(raw) => {
            out.push(b'/');
            out.extend_from_slice(raw);
        }
        Operand::String { raw, hex: false } => {
            out.push(b'(');
            out.extend_from_slice(raw);
            out.push(b')');
        }
        Operand::String { raw, hex: true } => {
            out.push(b'<');
            out.extend(raw.iter().filter(|b| b.is_ascii_hexdigit()));
            out.push(b'>');
        }
        Operand::Array { .. } => {
            out.push(b'[');
            for (k, e) in i.array(o).iter().enumerate() {
                if k > 0 {
                    out.push(b' ');
                }
                operand(out, i, e);
            }
            out.push(b']');
        }
        Operand::Dict(_) => match i.dict(o) {
            Some(d) => write_object(out, &Object::Dict(d.clone())),
            None => out.extend_from_slice(b"<<>>"),
        },
    }
}

/// An operator with its operands as read (inline images included), on its own line.
pub(crate) fn instr(out: &mut Vec<u8>, i: &Instr<'_, '_>) {
    if i.keyword == b"BI" {
        out.extend_from_slice(b"BI");
        for a in i.args {
            out.push(b' ');
            operand(out, i, a);
        }
        out.extend_from_slice(b" ID ");
        out.extend_from_slice(i.data);
        out.extend_from_slice(b"\nEI\n");
        return;
    }
    for a in i.args {
        operand(out, i, a);
        out.push(b' ');
    }
    out.extend_from_slice(i.keyword);
    out.push(b'\n');
}

pub(crate) fn hex_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'<');
    for b in bytes {
        let _ = write!(out, "{b:02X}");
    }
    out.push(b'>');
}

fn point(out: &mut Vec<u8>, p: Point) {
    num(out, p.x);
    out.push(b' ');
    num(out, p.y);
    out.push(b' ');
}

/// Path construction operators for subpaths already mapped to user space.
pub(crate) fn path(out: &mut Vec<u8>, subs: &[Sub]) {
    for s in subs {
        point(out, s.start);
        out.extend_from_slice(b"m\n");
        let mut cur = s.start;
        for seg in &s.segs {
            if seg.start() != cur {
                point(out, seg.start());
                out.extend_from_slice(b"l\n");
            }
            match *seg {
                Seg::Line(_, b) => {
                    point(out, b);
                    out.extend_from_slice(b"l\n");
                }
                Seg::Cubic(_, b, c, d) => {
                    point(out, b);
                    point(out, c);
                    point(out, d);
                    out.extend_from_slice(b"c\n");
                }
            }
            cur = seg.end();
        }
        if s.closed {
            out.extend_from_slice(b"h\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        let mut o = Vec::new();
        for v in [1.0, -0.5, 1e-9, 12.3456789, -0.0000001, 1e20] {
            num(&mut o, v);
            o.push(b' ');
        }
        assert_eq!(
            String::from_utf8(o).unwrap(),
            "1 -0.5 0 12.345679 0 100000000000000000000 "
        );
    }
}
