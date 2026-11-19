//! PDF functions (§7.10): sampled, exponential, stitching and PostScript calculator, the
//! last compiled once to a small stack program.

use std::sync::Arc;

use pagebara_syntax::{Dict, Document, Object};

/// Depth limit for stitching functions.
const MAX_DEPTH: u32 = 8;
const MAX_IO: usize = 32;

#[derive(Clone, Debug)]
pub enum Function {
    Sampled(Arc<Sampled>),
    Exponential {
        c0: Vec<f32>,
        c1: Vec<f32>,
        n: f32,
        domain: [f32; 2],
        range: Option<Vec<f32>>,
    },
    Stitching {
        fns: Vec<Function>,
        bounds: Vec<f32>,
        encode: Vec<f32>,
        domain: [f32; 2],
        range: Option<Vec<f32>>,
    },
    PostScript(Arc<PsProgram>),
    /// An array of 1-output functions (shadings allow this).
    Array(Vec<Function>),
}

#[derive(Debug)]
pub struct Sampled {
    domain: Vec<f32>,
    range: Vec<f32>,
    size: Vec<usize>,
    encode: Vec<f32>,
    decode: Vec<f32>,
    /// Samples normalised to 0..1, `outputs` per grid point.
    samples: Vec<f32>,
    outputs: usize,
}

impl Function {
    /// Parses a function object (a reference, dictionary, stream or array of them).
    pub fn parse(doc: &Document, o: &Object) -> Option<Function> {
        Function::parse_depth(doc, o, 0)
    }

    fn parse_depth(doc: &Document, o: &Object, depth: u32) -> Option<Function> {
        if depth > MAX_DEPTH {
            return None;
        }
        let r = doc.resolve(o);
        if let Some(a) = r.as_array() {
            let fns = a
                .iter()
                .map(|f| Function::parse_depth(doc, f, depth + 1))
                .collect::<Option<Vec<_>>>()?;
            return (!fns.is_empty()).then_some(Function::Array(fns));
        }
        let (dict, stream) = match &*r {
            Object::Stream(s) => (&s.dict, Some(s)),
            Object::Dict(d) => (d, None),
            _ => return None,
        };
        let nums = |k: &[u8]| -> Option<Vec<f32>> { numbers(doc, dict, k) };
        let domain = nums(b"Domain").unwrap_or_else(|| vec![0.0, 1.0]);
        if domain.len() < 2 {
            return None;
        }
        let range = nums(b"Range");
        match doc.get_in(dict, b"FunctionType").as_int()? {
            0 => {
                let s = stream?;
                let data = doc.decode(s).ok()?.data;
                let range = range?;
                let m = domain.len() / 2;
                let n = range.len() / 2;
                if m == 0 || n == 0 || m > 8 || n > MAX_IO {
                    return None;
                }
                let size: Vec<usize> = nums(b"Size")?
                    .iter()
                    .map(|&v| (v.max(1.0) as usize).min(1 << 16))
                    .collect();
                if size.len() < m {
                    return None;
                }
                let bps = doc.get_in(dict, b"BitsPerSample").as_int()?;
                if ![1, 2, 4, 8, 12, 16, 24, 32].contains(&bps) {
                    return None;
                }
                let count = size[..m].iter().try_fold(n, |a, &s| a.checked_mul(s))?;
                if count > 1 << 24 {
                    return None;
                }
                let encode = nums(b"Encode")
                    .filter(|e| e.len() >= 2 * m)
                    .unwrap_or_else(|| {
                        size[..m]
                            .iter()
                            .flat_map(|&s| [0.0, (s - 1) as f32])
                            .collect()
                    });
                let decode = nums(b"Decode")
                    .filter(|d| d.len() >= 2 * n)
                    .unwrap_or_else(|| range.clone());
                let mut samples = Vec::with_capacity(count);
                let max = ((1u64 << bps) - 1) as f32;
                let mut bits = BitReader::new(&data);
                for _ in 0..count {
                    samples.push(bits.read(bps as u32).unwrap_or(0) as f32 / max);
                }
                Some(Function::Sampled(Arc::new(Sampled {
                    domain,
                    range,
                    size: size[..m].to_vec(),
                    encode,
                    decode,
                    samples,
                    outputs: n,
                })))
            }
            2 => {
                let c0 = nums(b"C0").unwrap_or_else(|| vec![0.0]);
                let c1 = nums(b"C1").unwrap_or_else(|| vec![1.0]);
                if c0.len() != c1.len() || c0.len() > MAX_IO {
                    return None;
                }
                let n = doc.get_in(dict, b"N").as_f64()? as f32;
                Some(Function::Exponential {
                    c0,
                    c1,
                    n,
                    domain: [domain[0], domain[1]],
                    range,
                })
            }
            3 => {
                let fa = doc.get_in(dict, b"Functions");
                let fns = fa
                    .as_array()?
                    .iter()
                    .map(|f| Function::parse_depth(doc, f, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                let bounds = nums(b"Bounds").unwrap_or_default();
                let encode = nums(b"Encode")?;
                if fns.is_empty() || bounds.len() + 1 != fns.len() || encode.len() < 2 * fns.len() {
                    return None;
                }
                Some(Function::Stitching {
                    fns,
                    bounds,
                    encode,
                    domain: [domain[0], domain[1]],
                    range,
                })
            }
            4 => {
                let s = stream?;
                let data = doc.decode(s).ok()?.data;
                let prog = PsProgram::compile(&data, domain, range?)?;
                Some(Function::PostScript(Arc::new(prog)))
            }
            _ => None,
        }
    }

    /// Number of outputs (for a 1-input evaluation).
    pub fn outputs(&self) -> usize {
        match self {
            Function::Sampled(s) => s.outputs,
            Function::Exponential { c0, .. } => c0.len(),
            Function::Stitching { fns, .. } => fns[0].outputs(),
            Function::PostScript(p) => p.range.len() / 2,
            Function::Array(a) => a.len(),
        }
    }

    /// Evaluates; `out` receives the outputs (at most its length). Returns the count.
    pub fn eval(&self, input: &[f32], out: &mut [f32]) -> usize {
        match self {
            Function::Sampled(s) => s.eval(input, out),
            Function::Exponential {
                c0,
                c1,
                n,
                domain,
                range,
            } => {
                let x = input
                    .first()
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(domain[0], domain[1]);
                let xn = if *n == 1.0 { x } else { x.powf(*n) };
                let k = c0.len().min(out.len());
                for i in 0..k {
                    out[i] = c0[i] + xn * (c1[i] - c0[i]);
                }
                clip_range(&mut out[..k], range.as_deref());
                k
            }
            Function::Stitching {
                fns,
                bounds,
                encode,
                domain,
                range,
            } => {
                let x = input
                    .first()
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(domain[0], domain[1]);
                let k = bounds.iter().take_while(|&&b| x >= b).count();
                let k = k.min(fns.len() - 1);
                let lo = if k == 0 { domain[0] } else { bounds[k - 1] };
                let hi = if k == bounds.len() {
                    domain[1]
                } else {
                    bounds[k]
                };
                let (e0, e1) = (encode[2 * k], encode[2 * k + 1]);
                let t = if hi > lo {
                    e0 + (x - lo) * (e1 - e0) / (hi - lo)
                } else {
                    e0
                };
                let n = fns[k].eval(&[t], out);
                clip_range(&mut out[..n], range.as_deref());
                n
            }
            Function::PostScript(p) => p.eval(input, out),
            Function::Array(fns) => {
                let mut tmp = [0.0f32; MAX_IO];
                let k = fns.len().min(out.len());
                for i in 0..k {
                    fns[i].eval(input, &mut tmp);
                    out[i] = tmp[0];
                }
                k
            }
        }
    }
}

fn clip_range(v: &mut [f32], range: Option<&[f32]>) {
    if let Some(r) = range {
        for (i, x) in v.iter_mut().enumerate() {
            if let (Some(&lo), Some(&hi)) = (r.get(2 * i), r.get(2 * i + 1)) {
                *x = x.clamp(lo.min(hi), hi.max(lo));
            }
        }
    }
}

pub fn numbers(doc: &Document, dict: &Dict, k: &[u8]) -> Option<Vec<f32>> {
    let a = doc.get_in(dict, k);
    a.as_array()?
        .iter()
        .map(|o| doc.resolve(o).as_f64().map(|v| v as f32))
        .collect()
}

impl Sampled {
    fn eval(&self, input: &[f32], out: &mut [f32]) -> usize {
        let m = self.size.len();
        let n = self.outputs;
        // Per input: lower grid index and weight.
        let mut idx = [0usize; 8];
        let mut frac = [0f32; 8];
        for i in 0..m {
            let (d0, d1) = (self.domain[2 * i], self.domain[2 * i + 1]);
            let x = input
                .get(i)
                .copied()
                .unwrap_or(0.0)
                .clamp(d0.min(d1), d1.max(d0));
            let (e0, e1) = (self.encode[2 * i], self.encode[2 * i + 1]);
            let e = if d1 != d0 {
                e0 + (x - d0) * (e1 - e0) / (d1 - d0)
            } else {
                e0
            };
            let e = e.clamp(0.0, (self.size[i] - 1) as f32);
            let f = e.floor();
            idx[i] = f as usize;
            frac[i] = e - f;
        }
        let k = n.min(out.len());
        out[..k].fill(0.0);
        // Multilinear interpolation over the 2^m corners (skipping zero weights).
        for corner in 0..(1usize << m) {
            let mut w = 1.0f32;
            let mut offset = 0usize;
            let mut stride = 1usize;
            for i in 0..m {
                let hi = corner >> i & 1 == 1;
                let fw = if hi { frac[i] } else { 1.0 - frac[i] };
                if fw == 0.0 {
                    w = 0.0;
                    break;
                }
                w *= fw;
                let j = (idx[i] + usize::from(hi)).min(self.size[i] - 1);
                offset += j * stride;
                stride *= self.size[i];
            }
            if w == 0.0 {
                continue;
            }
            let base = offset * n;
            for (j, o) in out[..k].iter_mut().enumerate() {
                *o += w * self.samples.get(base + j).copied().unwrap_or(0.0);
            }
        }
        for (j, o) in out[..k].iter_mut().enumerate() {
            let (d0, d1) = (self.decode[2 * j], self.decode[2 * j + 1]);
            *o = d0 + *o * (d1 - d0);
        }
        clip_range(&mut out[..k], Some(&self.range));
        k
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    fn read(&mut self, bits: u32) -> Option<u64> {
        let mut v = 0u64;
        for _ in 0..bits {
            let byte = *self.data.get(self.pos / 8)?;
            let bit = byte >> (7 - self.pos % 8) & 1;
            v = v << 1 | u64::from(bit);
            self.pos += 1;
        }
        Some(v)
    }
}

// PostScript calculator (§7.10.5).

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Push(f32),
    Abs,
    Add,
    Atan,
    Ceiling,
    Cos,
    Cvi,
    Cvr,
    Div,
    Exp,
    Floor,
    Idiv,
    Ln,
    Log,
    Mod,
    Mul,
    Neg,
    Round,
    Sin,
    Sqrt,
    Sub,
    Truncate,
    And,
    Bitshift,
    Eq,
    False,
    Ge,
    Gt,
    Le,
    Lt,
    Ne,
    Not,
    Or,
    True,
    Xor,
    Copy,
    Dup,
    Exch,
    Index,
    Pop,
    Roll,
    /// Jump to the target when the popped value is false.
    JumpIfNot(u32),
    Jump(u32),
}

#[derive(Debug)]
pub struct PsProgram {
    ops: Vec<Op>,
    domain: Vec<f32>,
    range: Vec<f32>,
}

const STACK: usize = 100;

impl PsProgram {
    pub fn compile(src: &[u8], domain: Vec<f32>, range: Vec<f32>) -> Option<PsProgram> {
        let toks = tokenize(src);
        let mut pos = 0;
        // The outer braces.
        while pos < toks.len() && toks[pos] != Tok::Open {
            pos += 1;
        }
        pos += 1;
        let mut ops = Vec::new();
        compile_block(&toks, &mut pos, &mut ops, 0)?;
        Some(PsProgram { ops, domain, range })
    }

    pub fn eval(&self, input: &[f32], out: &mut [f32]) -> usize {
        let mut st = [0f32; STACK];
        let mut sp = 0usize;
        for (i, x) in input.iter().take(self.domain.len() / 2).enumerate() {
            let (d0, d1) = (self.domain[2 * i], self.domain[2 * i + 1]);
            st[sp] = x.clamp(d0.min(d1), d1.max(d0));
            sp += 1;
        }
        run(&self.ops, &mut st, &mut sp);
        let n = self.range.len() / 2;
        let k = n.min(out.len());
        // Outputs are the top `n` entries, bottom first.
        let base = sp.saturating_sub(n);
        for j in 0..k {
            let v = if base + j < sp { st[base + j] } else { 0.0 };
            let (r0, r1) = (self.range[2 * j], self.range[2 * j + 1]);
            out[j] = if v.is_finite() {
                v.clamp(r0.min(r1), r1.max(r0))
            } else {
                r0
            };
        }
        k
    }
}

fn run(ops: &[Op], st: &mut [f32; STACK], sp: &mut usize) {
    let mut pc = 0usize;
    let mut steps = 0u32;
    macro_rules! pop {
        () => {{
            if *sp == 0 {
                return;
            }
            *sp -= 1;
            st[*sp]
        }};
    }
    macro_rules! push {
        ($v:expr) => {{
            if *sp >= STACK {
                return;
            }
            st[*sp] = $v;
            *sp += 1;
        }};
    }
    let b = |v: bool| if v { 1.0 } else { 0.0 };
    while pc < ops.len() {
        steps += 1;
        if steps > 100_000 {
            return;
        }
        let op = ops[pc];
        pc += 1;
        match op {
            Op::Push(v) => push!(v),
            Op::Abs => {
                let a = pop!();
                push!(a.abs())
            }
            Op::Add => {
                let (y, x) = (pop!(), pop!());
                push!(x + y)
            }
            Op::Sub => {
                let (y, x) = (pop!(), pop!());
                push!(x - y)
            }
            Op::Mul => {
                let (y, x) = (pop!(), pop!());
                push!(x * y)
            }
            Op::Div => {
                let (y, x) = (pop!(), pop!());
                push!(if y == 0.0 { 0.0 } else { x / y })
            }
            Op::Idiv => {
                let (y, x) = (pop!(), pop!());
                let (y, x) = (y as i64, x as i64);
                push!(if y == 0 { 0.0 } else { (x / y) as f32 })
            }
            Op::Mod => {
                let (y, x) = (pop!(), pop!());
                let (y, x) = (y as i64, x as i64);
                push!(if y == 0 { 0.0 } else { (x % y) as f32 })
            }
            Op::Atan => {
                let (den, num) = (pop!(), pop!());
                let mut a = num.atan2(den).to_degrees();
                if a < 0.0 {
                    a += 360.0;
                }
                push!(a)
            }
            Op::Ceiling => {
                let a = pop!();
                push!(a.ceil())
            }
            Op::Floor => {
                let a = pop!();
                push!(a.floor())
            }
            Op::Round => {
                let a = pop!();
                push!((a + 0.5).floor())
            }
            Op::Truncate | Op::Cvi => {
                let a = pop!();
                push!(a.trunc())
            }
            Op::Cvr => {}
            Op::Cos => {
                let a = pop!();
                push!(a.to_radians().cos())
            }
            Op::Sin => {
                let a = pop!();
                push!(a.to_radians().sin())
            }
            Op::Exp => {
                let (e, base) = (pop!(), pop!());
                push!(base.powf(e))
            }
            Op::Ln => {
                let a = pop!();
                push!(a.ln())
            }
            Op::Log => {
                let a = pop!();
                push!(a.log10())
            }
            Op::Sqrt => {
                let a = pop!();
                push!(a.max(0.0).sqrt())
            }
            Op::Neg => {
                let a = pop!();
                push!(-a)
            }
            Op::And => {
                let (y, x) = (pop!(), pop!());
                push!((x as i64 & y as i64) as f32)
            }
            Op::Or => {
                let (y, x) = (pop!(), pop!());
                push!((x as i64 | y as i64) as f32)
            }
            Op::Xor => {
                let (y, x) = (pop!(), pop!());
                push!((x as i64 ^ y as i64) as f32)
            }
            Op::Not => {
                let a = pop!();
                // Booleans are 0/1; integers get a bitwise not.
                push!(if a == 0.0 || a == 1.0 {
                    1.0 - a
                } else {
                    !(a as i64) as f32
                })
            }
            Op::Bitshift => {
                let (s, x) = (pop!() as i64, pop!() as i64);
                let v = if s >= 0 {
                    x.checked_shl(s as u32).unwrap_or(0)
                } else {
                    x.checked_shr((-s) as u32).unwrap_or(0)
                };
                push!(v as f32)
            }
            Op::Eq => {
                let (y, x) = (pop!(), pop!());
                push!(b(x == y))
            }
            Op::Ne => {
                let (y, x) = (pop!(), pop!());
                push!(b(x != y))
            }
            Op::Ge => {
                let (y, x) = (pop!(), pop!());
                push!(b(x >= y))
            }
            Op::Gt => {
                let (y, x) = (pop!(), pop!());
                push!(b(x > y))
            }
            Op::Le => {
                let (y, x) = (pop!(), pop!());
                push!(b(x <= y))
            }
            Op::Lt => {
                let (y, x) = (pop!(), pop!());
                push!(b(x < y))
            }
            Op::True => push!(1.0),
            Op::False => push!(0.0),
            Op::Dup => {
                let a = pop!();
                push!(a);
                push!(a)
            }
            Op::Pop => {
                pop!();
            }
            Op::Exch => {
                let (y, x) = (pop!(), pop!());
                push!(y);
                push!(x)
            }
            Op::Copy => {
                let n = pop!().max(0.0) as usize;
                if n > *sp || *sp + n > STACK {
                    return;
                }
                let s = *sp;
                st.copy_within(s - n..s, s);
                *sp += n;
            }
            Op::Index => {
                let n = pop!().max(0.0) as usize;
                if n >= *sp {
                    return;
                }
                let v = st[*sp - 1 - n];
                push!(v)
            }
            Op::Roll => {
                let (j, n) = (pop!() as i64, pop!().max(0.0) as usize);
                if n > *sp || n == 0 {
                    continue;
                }
                let s = *sp;
                let slice = &mut st[s - n..s];
                let j = j.rem_euclid(n as i64) as usize;
                slice.rotate_right(j);
            }
            Op::JumpIfNot(t) => {
                if pop!() == 0.0 {
                    pc = t as usize;
                }
            }
            Op::Jump(t) => pc = t as usize,
        }
    }
}

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    Num(f32),
    Word(&'a [u8]),
}

fn tokenize(src: &[u8]) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        match c {
            b'{' => {
                out.push(Tok::Open);
                i += 1;
            }
            b'}' => {
                out.push(Tok::Close);
                i += 1;
            }
            b'%' => {
                while i < src.len() && src[i] != b'\n' && src[i] != b'\r' {
                    i += 1;
                }
            }
            c if c.is_ascii_whitespace() || c == 0 => i += 1,
            _ => {
                let s = i;
                while i < src.len()
                    && !src[i].is_ascii_whitespace()
                    && !matches!(src[i], b'{' | b'}' | b'%')
                {
                    i += 1;
                }
                let w = &src[s..i];
                match std::str::from_utf8(w)
                    .ok()
                    .and_then(|t| t.parse::<f32>().ok())
                {
                    Some(v) => out.push(Tok::Num(v)),
                    None => out.push(Tok::Word(w)),
                }
            }
        }
    }
    out
}

/// Compiles tokens up to the matching `}`; `if`/`ifelse` become jumps.
fn compile_block(toks: &[Tok<'_>], pos: &mut usize, ops: &mut Vec<Op>, depth: u32) -> Option<()> {
    if depth > 64 {
        return None;
    }
    // Pending procedure blocks awaiting `if`/`ifelse`: (start, end) op ranges.
    let mut blocks: Vec<Vec<Op>> = Vec::new();
    while *pos < toks.len() {
        let t = &toks[*pos];
        *pos += 1;
        match t {
            Tok::Close => {
                for b in blocks.drain(..) {
                    ops.extend(b);
                }
                return Some(());
            }
            Tok::Open => {
                let mut b = Vec::new();
                compile_block(toks, pos, &mut b, depth + 1)?;
                blocks.push(b);
            }
            Tok::Num(v) => {
                for b in blocks.drain(..) {
                    ops.extend(b);
                }
                ops.push(Op::Push(*v));
            }
            Tok::Word(w) => {
                match *w {
                    b"if" => {
                        let body = blocks.pop()?;
                        flush(&mut blocks, ops);
                        let base = ops.len();
                        ops.push(Op::JumpIfNot(0));
                        append(ops, body, base + 1);
                        let end = ops.len() as u32;
                        ops[base] = Op::JumpIfNot(end);
                        continue;
                    }
                    b"ifelse" => {
                        let no = blocks.pop()?;
                        let yes = blocks.pop()?;
                        flush(&mut blocks, ops);
                        let base = ops.len();
                        ops.push(Op::JumpIfNot(0));
                        append(ops, yes, base + 1);
                        let jmp = ops.len();
                        ops.push(Op::Jump(0));
                        let else_start = ops.len();
                        append(ops, no, else_start);
                        let end = ops.len() as u32;
                        ops[base] = Op::JumpIfNot(else_start as u32);
                        ops[jmp] = Op::Jump(end);
                        continue;
                    }
                    _ => {}
                }
                flush(&mut blocks, ops);
                let op = match *w {
                    b"abs" => Op::Abs,
                    b"add" => Op::Add,
                    b"atan" => Op::Atan,
                    b"ceiling" => Op::Ceiling,
                    b"cos" => Op::Cos,
                    b"cvi" => Op::Cvi,
                    b"cvr" => Op::Cvr,
                    b"div" => Op::Div,
                    b"exp" => Op::Exp,
                    b"floor" => Op::Floor,
                    b"idiv" => Op::Idiv,
                    b"ln" => Op::Ln,
                    b"log" => Op::Log,
                    b"mod" => Op::Mod,
                    b"mul" => Op::Mul,
                    b"neg" => Op::Neg,
                    b"round" => Op::Round,
                    b"sin" => Op::Sin,
                    b"sqrt" => Op::Sqrt,
                    b"sub" => Op::Sub,
                    b"truncate" => Op::Truncate,
                    b"and" => Op::And,
                    b"bitshift" => Op::Bitshift,
                    b"eq" => Op::Eq,
                    b"false" => Op::False,
                    b"ge" => Op::Ge,
                    b"gt" => Op::Gt,
                    b"le" => Op::Le,
                    b"lt" => Op::Lt,
                    b"ne" => Op::Ne,
                    b"not" => Op::Not,
                    b"or" => Op::Or,
                    b"true" => Op::True,
                    b"xor" => Op::Xor,
                    b"copy" => Op::Copy,
                    b"dup" => Op::Dup,
                    b"exch" => Op::Exch,
                    b"index" => Op::Index,
                    b"pop" => Op::Pop,
                    b"roll" => Op::Roll,
                    _ => return None,
                };
                ops.push(op);
            }
        }
    }
    flush(&mut blocks, ops);
    Some(())
}

fn flush(blocks: &mut Vec<Vec<Op>>, ops: &mut Vec<Op>) {
    for b in blocks.drain(..) {
        ops.extend(b);
    }
}

/// Appends a compiled block whose jump targets are relative to its start.
fn append(ops: &mut Vec<Op>, block: Vec<Op>, at: usize) {
    for op in block {
        ops.push(match op {
            Op::Jump(t) => Op::Jump(t + at as u32),
            Op::JumpIfNot(t) => Op::JumpIfNot(t + at as u32),
            o => o,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(src: &str, input: &[f32], n: usize) -> Vec<f32> {
        let p = PsProgram::compile(
            src.as_bytes(),
            vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            [-100.0, 100.0].repeat(n),
        )
        .unwrap();
        let mut out = vec![0.0; n];
        p.eval(input, &mut out);
        out
    }

    #[test]
    fn calculator() {
        assert_eq!(ps("{ 2 mul 1 add }", &[0.5], 1), vec![2.0]);
        assert_eq!(
            ps("{ dup 0.5 gt { pop 1 } { 0 } ifelse }", &[0.7], 1),
            vec![1.0]
        );
        assert_eq!(
            ps("{ dup 0.5 gt { pop 1 } { 0 } ifelse }", &[0.2], 1)[0],
            0.0
        );
        assert_eq!(ps("{ 1 exch sub }", &[0.25], 1), vec![0.75]);
        assert_eq!(ps("{ 3 1 roll }", &[0.1, 0.2, 0.3], 3), vec![0.3, 0.1, 0.2]);
        assert_eq!(ps("{ dup 0.5 lt { 2 mul } if }", &[0.25], 1), vec![0.5]);
        assert_eq!(
            ps("{ 0 0 0 4 3 roll }", &[0.4], 4),
            vec![0.0, 0.0, 0.0, 0.4]
        );
    }
}
