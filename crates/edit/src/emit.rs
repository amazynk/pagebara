//! Writing content stream syntax.

use std::io::Write;

use pagebara_content::lex::Operand;
use pagebara_content::{Instr, Matrix};
use pagebara_syntax::{Object, write_object};

use crate::model::Color;

/// Appends a number without exponent and with at most six decimals.
pub fn num(out: &mut Vec<u8>, v: f64) {
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

pub fn nums(out: &mut Vec<u8>, vs: &[f64]) {
    for &v in vs {
        num(out, v);
        out.push(b' ');
    }
}

pub fn matrix(out: &mut Vec<u8>, m: &Matrix) {
    nums(out, &[m.a, m.b, m.c, m.d, m.e, m.f].map(f64::from));
}

/// `a b c d e f cm`.
pub fn cm(out: &mut Vec<u8>, m: &Matrix) {
    matrix(out, m);
    out.extend_from_slice(b"cm\n");
}

/// `a b c d e f Tm`.
pub fn tm(out: &mut Vec<u8>, m: &Matrix) {
    matrix(out, m);
    out.extend_from_slice(b"Tm\n");
}

pub fn name(out: &mut Vec<u8>, n: &[u8]) {
    write_object(out, &Object::name(n));
}

pub fn hex_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'<');
    for b in bytes {
        let _ = write!(out, "{b:02X}");
    }
    out.push(b'>');
}

/// Operators setting a device colour.
pub fn color(out: &mut Vec<u8>, c: &Color, stroke: bool) {
    let (vals, op): (Vec<f64>, &[u8]) = match *c {
        Color::Gray(g) => (vec![g.into()], if stroke { b"G" } else { b"g" }),
        Color::Rgb(r, g, b) => (
            vec![r.into(), g.into(), b.into()],
            if stroke { b"RG" } else { b"rg" },
        ),
        Color::Cmyk(c, m, y, k) => (
            vec![c.into(), m.into(), y.into(), k.into()],
            if stroke { b"K" } else { b"k" },
        ),
    };
    nums(out, &vals);
    out.extend_from_slice(op);
    out.push(b'\n');
}

/// `/Name op` on its own line.
pub fn named_op(out: &mut Vec<u8>, n: &[u8], op: &[u8]) {
    name(out, n);
    out.push(b' ');
    out.extend_from_slice(op);
    out.push(b'\n');
}

pub fn operand(out: &mut Vec<u8>, i: &Instr<'_, '_>, o: &Operand<'_>) {
    match *o {
        Operand::Number(n) => {
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

/// An operator with its operands as read, on its own line (not for inline images).
pub fn instr(out: &mut Vec<u8>, i: &Instr<'_, '_>) {
    for a in i.args {
        operand(out, i, a);
        out.push(b' ');
    }
    out.extend_from_slice(i.keyword);
    out.push(b'\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        let mut o = Vec::new();
        for v in [1.0, -0.5, 1e-9, 12.3456789, -0.0000001] {
            num(&mut o, v);
            o.push(b' ');
        }
        assert_eq!(String::from_utf8(o).unwrap(), "1 -0.5 0 12.345679 0 ");
    }
}
