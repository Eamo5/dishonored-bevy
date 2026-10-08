//! Direct3D 9 shader model 3 bytecode (as compiled into UE3's shader caches) and its
//! translation to WGSL.
//!
//! The token stream is parsed into instructions; the constant table (CTAB comment) names
//! the constant and sampler registers. The WGSL translation keeps the original register
//! model: temporaries are `vec4<f32>` variables, writes honour the destination mask and
//! saturation, sources their swizzles and modifiers. Float constants come from `def`s or
//! the caller's constant arrays (`<prefix>kc`, `<prefix>ki`, `<prefix>kb`); textures are read
//! through caller-provided sampling functions, one per sampler register.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Pixel,
}

// register types
pub const TEMP: u32 = 0;
pub const INPUT: u32 = 1;
pub const CONST: u32 = 2;
pub const ADDR: u32 = 3; // vs a0 / ps t#
pub const RASTOUT: u32 = 4;
pub const ATTROUT: u32 = 5;
pub const OUTPUT: u32 = 6;
pub const CONSTINT: u32 = 7;
pub const COLOROUT: u32 = 8;
pub const DEPTHOUT: u32 = 9;
pub const SAMPLER: u32 = 10;
pub const CONSTBOOL: u32 = 14;
pub const LOOP: u32 = 15;
pub const MISCTYPE: u32 = 17;
pub const PREDICATE: u32 = 19;

#[derive(Clone, Copy, Debug)]
pub struct Dst {
    pub ty: u32,
    pub reg: u32,
    pub mask: u32,
    pub saturate: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Src {
    pub ty: u32,
    pub reg: u32,
    pub swizzle: [u8; 4],
    pub modifier: u32,
    /// relative addressing: (register type, register, component)
    pub rel: Option<(u32, u32, u8)>,
}

#[derive(Clone, Debug)]
pub struct Instr {
    pub op: u32,
    pub control: u32,
    pub dst: Option<Dst>,
    pub src: Vec<Src>,
    /// predicate register source (if predicated)
    pub pred: Option<Src>,
}

/// A declared input/output/sampler.
#[derive(Clone, Copy, Debug)]
pub struct Decl {
    pub ty: u32,
    pub reg: u32,
    pub usage: u32,
    pub index: u32,
    /// sampler texture type (2 = 2D, 3 = cube, 4 = volume)
    pub tex: u32,
    pub mask: u32,
}

/// A named entry of the constant table.
#[derive(Clone, Debug)]
pub struct CtabEntry {
    pub name: String,
    /// 0 bool, 1 int4, 2 float4, 3 sampler
    pub set: u16,
    pub reg: u16,
    pub count: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Shader {
    pub stage: Option<Stage>,
    pub decls: Vec<Decl>,
    pub defs: BTreeMap<u32, [f32; 4]>,
    pub defi: BTreeMap<u32, [i32; 4]>,
    pub defb: BTreeMap<u32, bool>,
    pub instrs: Vec<Instr>,
    pub ctab: Vec<CtabEntry>,
}

pub mod op {
    pub const NOP: u32 = 0;
    pub const MOV: u32 = 1;
    pub const ADD: u32 = 2;
    pub const SUB: u32 = 3;
    pub const MAD: u32 = 4;
    pub const MUL: u32 = 5;
    pub const RCP: u32 = 6;
    pub const RSQ: u32 = 7;
    pub const DP3: u32 = 8;
    pub const DP4: u32 = 9;
    pub const MIN: u32 = 10;
    pub const MAX: u32 = 11;
    pub const SLT: u32 = 12;
    pub const SGE: u32 = 13;
    pub const EXP: u32 = 14;
    pub const LOG: u32 = 15;
    pub const LIT: u32 = 16;
    pub const DST: u32 = 17;
    pub const LRP: u32 = 18;
    pub const FRC: u32 = 19;
    pub const M4X4: u32 = 20;
    pub const M4X3: u32 = 21;
    pub const M3X4: u32 = 22;
    pub const M3X3: u32 = 23;
    pub const M3X2: u32 = 24;
    pub const CALL: u32 = 25;
    pub const CALLNZ: u32 = 26;
    pub const LOOP: u32 = 27;
    pub const RET: u32 = 28;
    pub const ENDLOOP: u32 = 29;
    pub const LABEL: u32 = 30;
    pub const DCL: u32 = 31;
    pub const POW: u32 = 32;
    pub const CRS: u32 = 33;
    pub const SGN: u32 = 34;
    pub const ABS: u32 = 35;
    pub const NRM: u32 = 36;
    pub const SINCOS: u32 = 37;
    pub const REP: u32 = 38;
    pub const ENDREP: u32 = 39;
    pub const IF: u32 = 40;
    pub const IFC: u32 = 41;
    pub const ELSE: u32 = 42;
    pub const ENDIF: u32 = 43;
    pub const BREAK: u32 = 44;
    pub const BREAKC: u32 = 45;
    pub const MOVA: u32 = 46;
    pub const DEFB: u32 = 47;
    pub const DEFI: u32 = 48;
    pub const TEXKILL: u32 = 65;
    pub const TEX: u32 = 66;
    pub const EXPP: u32 = 78;
    pub const LOGP: u32 = 79;
    pub const CND: u32 = 80;
    pub const DEF: u32 = 81;
    pub const DSX: u32 = 91;
    pub const DSY: u32 = 92;
    pub const CMP: u32 = 88;
    pub const DP2ADD: u32 = 90;
    pub const TEXLDD: u32 = 93;
    pub const SETP: u32 = 94;
    pub const TEXLDL: u32 = 95;
    pub const BREAKP: u32 = 96;
    pub const COMMENT: u32 = 0xFFFE;
    pub const END: u32 = 0xFFFF;
}

/// (destination count, source count) of an opcode.
fn arity(o: u32) -> Option<(usize, usize)> {
    use op::*;
    Some(match o {
        NOP => (0, 0),
        MOV | RCP | RSQ | EXP | LOG | LIT | FRC | SGN | ABS | NRM | MOVA | EXPP | LOGP | DSX | DSY => (1, 1),
        ADD | SUB | MUL | DP3 | DP4 | MIN | MAX | SLT | SGE | DST | POW | CRS | M4X4 | M4X3 | M3X4 | M3X3 | M3X2 => (1, 2),
        MAD | LRP | CND | CMP | DP2ADD => (1, 3),
        SINCOS => (1, 1), // sm3: one source
        TEX => (1, 2),
        TEXLDL => (1, 2),
        TEXLDD => (1, 4),
        TEXKILL => (1, 0),
        SETP => (1, 2),
        CALL => (0, 1),
        CALLNZ => (0, 2),
        LOOP => (0, 2),
        RET | ENDLOOP | ENDREP | ELSE | ENDIF | BREAK => (0, 0),
        LABEL => (0, 1),
        REP => (0, 1),
        IF => (0, 1),
        IFC | BREAKC => (0, 2),
        BREAKP => (0, 1),
        _ => return None,
    })
}

fn reg_type(t: u32) -> u32 {
    ((t >> 28) & 7) | ((t >> 8) & 0x18)
}

fn parse_ctab(words: &[u32]) -> Vec<CtabEntry> {
    // words start after the "CTAB" fourcc
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let rd16 = |o: usize| bytes.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
    let rd32 = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
    let cstr = |o: usize| -> String {
        let mut s = String::new();
        let mut i = o;
        while let Some(&c) = bytes.get(i) {
            if c == 0 {
                break;
            }
            s.push(c as char);
            i += 1;
        }
        s
    };
    let n = rd32(12) as usize;
    let info = rd32(16) as usize;
    let mut out = Vec::new();
    for k in 0..n.min(512) {
        let e = info + k * 20;
        out.push(CtabEntry { name: cstr(rd32(e) as usize), set: rd16(e + 4), reg: rd16(e + 6), count: rd16(e + 8) });
    }
    out
}

/// Parse SM3 bytecode (little-endian tokens).
pub fn parse(code: &[u8]) -> Result<Shader> {
    if code.len() < 8 || code.len() % 4 != 0 {
        bail!("bad bytecode size");
    }
    let t: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let mut sh = Shader {
        stage: match t[0] {
            0xFFFE0300 => Some(Stage::Vertex),
            0xFFFF0300 => Some(Stage::Pixel),
            v => bail!("unsupported shader version {v:#x}"),
        },
        ..Default::default()
    };
    let mut p = 1;
    let read_src = |t: &[u32], p: &mut usize| -> Src {
        let tok = t[*p];
        *p += 1;
        let sw = (tok >> 16) & 0xff;
        let mut s = Src {
            ty: reg_type(tok),
            reg: tok & 0x7ff,
            swizzle: [(sw & 3) as u8, ((sw >> 2) & 3) as u8, ((sw >> 4) & 3) as u8, ((sw >> 6) & 3) as u8],
            modifier: (tok >> 24) & 0xf,
            rel: None,
        };
        if tok & (1 << 13) != 0 {
            let a = t.get(*p).copied().unwrap_or(0);
            *p += 1;
            s.rel = Some((reg_type(a), a & 0x7ff, ((a >> 16) & 3) as u8));
        }
        s
    };
    while p < t.len() {
        let tok = t[p];
        let o = tok & 0xffff;
        if o == op::END {
            break;
        }
        if o == op::COMMENT {
            let len = ((tok >> 16) & 0x7fff) as usize;
            if len >= 1 && t.get(p + 1) == Some(&0x4241_5443) {
                sh.ctab = parse_ctab(&t[p + 2..(p + 1 + len).min(t.len())]);
            }
            p += 1 + len;
            continue;
        }
        let len = ((tok >> 24) & 0xf) as usize;
        let next = p + 1 + len;
        p += 1;
        match o {
            op::DCL => {
                let d = t[p];
                let r = t[p + 1];
                sh.decls.push(Decl {
                    ty: reg_type(r),
                    reg: r & 0x7ff,
                    usage: d & 0x1f,
                    index: (d >> 16) & 0xf,
                    tex: (d >> 27) & 0xf,
                    mask: (r >> 16) & 0xf,
                });
            }
            op::DEF => {
                let r = t[p] & 0x7ff;
                sh.defs.insert(r, [f32::from_bits(t[p + 1]), f32::from_bits(t[p + 2]), f32::from_bits(t[p + 3]), f32::from_bits(t[p + 4])]);
            }
            op::DEFI => {
                let r = t[p] & 0x7ff;
                sh.defi.insert(r, [t[p + 1] as i32, t[p + 2] as i32, t[p + 3] as i32, t[p + 4] as i32]);
            }
            op::DEFB => {
                let r = t[p] & 0x7ff;
                sh.defb.insert(r, t[p + 1] != 0);
            }
            _ => {
                let Some((nd, ns)) = arity(o) else { bail!("unsupported opcode {o}") };
                let mut ins = Instr { op: o, control: (tok >> 16) & 0xff, dst: None, src: Vec::new(), pred: None };
                if nd == 1 {
                    let d = t[p];
                    p += 1;
                    if d & (1 << 13) != 0 {
                        p += 1; // relative destination (output arrays); unused here
                    }
                    ins.dst = Some(Dst { ty: reg_type(d), reg: d & 0x7ff, mask: (d >> 16) & 0xf, saturate: (d >> 20) & 1 != 0 });
                }
                if tok & (1 << 28) != 0 {
                    ins.pred = Some(read_src(&t, &mut p));
                }
                for _ in 0..ns {
                    if p >= next {
                        break;
                    }
                    ins.src.push(read_src(&t, &mut p));
                }
                sh.instrs.push(ins);
            }
        }
        p = next;
    }
    Ok(sh)
}

// ------------------------------------------------------------------ WGSL

/// Location of a varying given its D3D usage and index (shared by both stages).
pub fn varying_location(usage: u32, index: u32) -> u32 {
    match usage {
        5 => index,           // texcoord 0..15
        10 => 16 + index,     // color 0..1
        11 => 18,             // fog
        3 => 19 + index,      // normal
        6 => 21 + index,      // tangent
        7 => 23 + index,      // binormal
        4 => 25,              // psize
        _ => 26 + index,
    }
}

/// Inputs/outputs of a translated shader.
#[derive(Clone, Debug, Default)]
pub struct Interface {
    /// (location, register type, register, usage, usage index)
    pub inputs: Vec<(u32, u32, u32, u32, u32)>,
    pub outputs: Vec<(u32, u32, u32, u32, u32)>,
    /// sampler register -> texture dimension (2 / 3 cube / 4 volume)
    pub samplers: BTreeMap<u32, u32>,
    /// float constant registers read from the uniform array (not `def`s)
    pub consts: BTreeSet<u32>,
    pub uses_rel_const: bool,
    pub max_const: u32,
    /// sampling variants used per sampler register (bit 0 implicit, 1 level, 2 bias, 3 grad)
    pub sample_kinds: BTreeMap<u32, u8>,
    pub max_int: u32,
    pub max_bool: u32,
}

struct Emitter<'a> {
    sh: &'a Shader,
    /// name prefix of everything the translation refers to (`vs_`, `ps_`, ...)
    p: String,
    out: String,
    indent: usize,
    /// inside non-uniform control flow (no implicit-derivative sampling)
    branch_depth: usize,
    loop_depth: usize,
    iface: Interface,
}

const COMP: [char; 4] = ['x', 'y', 'z', 'w'];

impl<'a> Emitter<'a> {
    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn reg_name(&mut self, ty: u32, reg: u32) -> String {
        match ty {
            TEMP => format!("r{reg}"),
            INPUT => format!("v{reg}"),
            CONST => {
                if self.sh.defs.contains_key(&reg) {
                    format!("c{reg}")
                } else {
                    self.iface.consts.insert(reg);
                    self.iface.max_const = self.iface.max_const.max(reg);
                    format!("{}kc[{reg}]", self.p)
                }
            }
            ADDR => {
                if self.sh.stage == Some(Stage::Vertex) {
                    "a0".into()
                } else {
                    format!("t{reg}")
                }
            }
            RASTOUT => format!("orast{reg}"),
            ATTROUT => format!("oattr{reg}"),
            OUTPUT => format!("o{reg}"),
            COLOROUT => format!("oc{reg}"),
            DEPTHOUT => "odepth".into(),
            CONSTINT => {
                if self.sh.defi.contains_key(&reg) {
                    format!("vec4<f32>(i{reg})")
                } else {
                    self.iface.max_int = self.iface.max_int.max(reg);
                    format!("vec4<f32>({}ki[{reg}])", self.p)
                }
            }
            CONSTBOOL => format!("b{reg}"),
            LOOP => "vec4<f32>(f32(aL))".into(),
            MISCTYPE => {
                if reg == 0 {
                    "vpos".into()
                } else {
                    "vface".into()
                }
            }
            PREDICATE => "p0".into(),
            _ => format!("unknown{ty}_{reg}"),
        }
    }

    /// A source operand as a vec4 expression.
    fn src(&mut self, s: &Src) -> String {
        let base = if let (CONST, Some((rty, rreg, rc))) = (s.ty, s.rel) {
            self.iface.uses_rel_const = true;
            let idx = if rty == LOOP { "aL".to_string() } else { format!("i32({}.{})", self.reg_name(rty, rreg), COMP[rc as usize]) };
            format!("{}kc[{} + {}]", self.p, s.reg, idx)
        } else {
            self.reg_name(s.ty, s.reg)
        };
        let sw: String = s.swizzle.iter().map(|&c| COMP[c as usize]).collect();
        let v = if sw == "xyzw" { base } else { format!("{base}.{sw}") };
        match s.modifier {
            0 => v,
            1 => format!("(-{v})"),
            2 => format!("({v} - vec4<f32>(0.5))"),
            3 => format!("(vec4<f32>(0.5) - {v})"),
            4 => format!("({v} * 2.0 - vec4<f32>(1.0))"),
            5 => format!("(vec4<f32>(1.0) - {v} * 2.0)"),
            6 => format!("(vec4<f32>(1.0) - {v})"),
            7 => format!("({v} * 2.0)"),
            8 => format!("({v} * -2.0)"),
            9 => format!("({v} / {v}.zzzz)"),
            10 => format!("({v} / {v}.wwww)"),
            11 => format!("abs({v})"),
            12 => format!("(-abs({v}))"),
            13 => format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {v} == vec4<f32>(0.0))"),
            _ => v,
        }
    }

    /// Scalar from the first selected component.
    fn src1(&mut self, s: &Src) -> String {
        format!("{}.x", self.src(s))
    }

    fn assign(&mut self, d: &Dst, expr: &str, pred: Option<&Src>) {
        let name = self.reg_name(d.ty, d.reg);
        let full = d.mask == 0xf || d.mask == 0;
        let e = if d.saturate { format!("clamp({expr}, vec4<f32>(0.0), vec4<f32>(1.0))") } else { expr.to_string() };
        let e = if let Some(p) = pred {
            // predicated write: keep components whose predicate is false
            let ps = self.src(p);
            let cond = if p.modifier == 13 { format!("{ps} != vec4<f32>(0.0)") } else { format!("{ps} != vec4<f32>(0.0)") };
            format!("select({name}, {e}, {cond})")
        } else {
            e
        };
        if full {
            self.line(&format!("{name} = {e};"));
        } else {
            let m: Vec<&str> = (0..4).map(|k| if d.mask & (1 << k) != 0 { "true" } else { "false" }).collect();
            self.line(&format!("{name} = select({name}, {e}, vec4<bool>({}));", m.join(", ")));
        }
    }

    fn cmp_op(control: u32) -> &'static str {
        match control & 7 {
            1 => ">",
            2 => "==",
            3 => ">=",
            4 => "<",
            5 => "!=",
            6 => "<=",
            _ => "!=",
        }
    }

    fn tex_expr(&mut self, ins: &Instr) -> Result<String> {
        let coord = self.src(&ins.src[0]);
        let s = ins.src[1].reg;
        let dim = *self.iface.samplers.get(&s).unwrap_or(&2);
        let sw = if dim == 2 { "xy" } else { "xyz" };
        let uv = if ins.op == op::TEX && ins.control == 1 {
            // projective
            format!("({coord}.{sw} / {coord}.w)")
        } else {
            format!("{coord}.{sw}")
        };
        let f = format!("{}smp{s}", self.p);
        let (kind, e) = match ins.op {
            op::TEXLDL => (1, format!("{f}_l({uv}, {coord}.w)")),
            op::TEXLDD => {
                let gx = self.src(&ins.src[2]);
                let gy = self.src(&ins.src[3]);
                (3, format!("{f}_g({uv}, {gx}.{sw}, {gy}.{sw})"))
            }
            _ if self.branch_depth > 0 || self.loop_depth > 0 || self.sh.stage == Some(Stage::Vertex) => (1, format!("{f}_l({uv}, 0.0)")),
            _ if ins.control == 2 => (2, format!("{f}_b({uv}, {coord}.w)")),
            _ => (0, format!("{f}({uv})")),
        };
        *self.iface.sample_kinds.entry(s).or_default() |= 1 << kind;
        Ok(e)
    }

    fn instr(&mut self, ins: &Instr) -> Result<()> {
        use op::*;
        let d = ins.dst;
        let pred = ins.pred;
        let a = |e: &mut Self, i: usize| e.src(&ins.src[i]);
        let expr: Option<String> = match ins.op {
            NOP => None,
            MOV => Some(a(self, 0)),
            MOVA => Some(format!("round({})", a(self, 0))),
            ADD => Some(format!("({} + {})", a(self, 0), a(self, 1))),
            SUB => Some(format!("({} - {})", a(self, 0), a(self, 1))),
            MUL => Some(format!("({} * {})", a(self, 0), a(self, 1))),
            MAD => Some(format!("fma({}, {}, {})", a(self, 0), a(self, 1), a(self, 2))),
            RCP => Some(format!("vec4<f32>(1.0 / {})", self.src1(&ins.src[0]))),
            RSQ => Some(format!("vec4<f32>(inverseSqrt(abs({})))", self.src1(&ins.src[0]))),
            EXP | EXPP => Some(format!("vec4<f32>(exp2({}))", self.src1(&ins.src[0]))),
            LOG | LOGP => Some(format!("vec4<f32>(log2(abs({})))", self.src1(&ins.src[0]))),
            POW => Some(format!("vec4<f32>(pow(abs({}), {}))", self.src1(&ins.src[0]), self.src1(&ins.src[1]))),
            DP3 => Some(format!("vec4<f32>(dot({}.xyz, {}.xyz))", a(self, 0), a(self, 1))),
            DP4 => Some(format!("vec4<f32>(dot({}, {}))", a(self, 0), a(self, 1))),
            DP2ADD => Some(format!("vec4<f32>(dot({}.xy, {}.xy) + {}.x)", a(self, 0), a(self, 1), a(self, 2))),
            MIN => Some(format!("min({}, {})", a(self, 0), a(self, 1))),
            MAX => Some(format!("max({}, {})", a(self, 0), a(self, 1))),
            SLT => Some(format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {} < {})", a(self, 0), a(self, 1))),
            SGE => Some(format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {} >= {})", a(self, 0), a(self, 1))),
            FRC => Some(format!("fract({})", a(self, 0))),
            ABS => Some(format!("abs({})", a(self, 0))),
            SGN => Some(format!("sign({})", a(self, 0))),
            NRM => {
                let s = a(self, 0);
                Some(format!("({s} * inverseSqrt(max(dot({s}.xyz, {s}.xyz), 1e-30)))"))
            }
            LRP => Some(format!("mix({}, {}, {})", a(self, 2), a(self, 1), a(self, 0))),
            CMP => Some(format!("select({}, {}, {} >= vec4<f32>(0.0))", a(self, 2), a(self, 1), a(self, 0))),
            CND => Some(format!("select({}, {}, {} > vec4<f32>(0.5))", a(self, 2), a(self, 1), a(self, 0))),
            CRS => Some(format!("vec4<f32>(cross({}.xyz, {}.xyz), 0.0)", a(self, 0), a(self, 1))),
            SINCOS => {
                let s = self.src1(&ins.src[0]);
                Some(format!("vec4<f32>(cos({s}), sin({s}), 0.0, 0.0)"))
            }
            LIT => {
                let s = a(self, 0);
                Some(format!(
                    "vec4<f32>(1.0, max({s}.x, 0.0), select(0.0, pow(max({s}.y, 0.0), clamp({s}.w, -127.9961, 127.9961)), {s}.x > 0.0), 1.0)"
                ))
            }
            DST => Some(format!("vec4<f32>(1.0, {0}.y * {1}.y, {0}.z, {1}.w)", a(self, 0), a(self, 1))),
            DSX => Some(format!("dpdx({})", a(self, 0))),
            DSY => Some(format!("dpdy({})", a(self, 0))),
            M4X4 | M4X3 | M3X4 | M3X3 | M3X2 => {
                let v = a(self, 0);
                let (rows, cols) = match ins.op {
                    M4X4 => (4, 4),
                    M4X3 => (3, 4),
                    M3X4 => (4, 3),
                    M3X3 => (3, 3),
                    _ => (2, 3),
                };
                let mut comps = Vec::new();
                for r in 0..rows {
                    let mut m = ins.src[1];
                    m.reg += r as u32;
                    let ms = self.src(&m);
                    comps.push(if cols == 4 { format!("dot({v}, {ms})") } else { format!("dot({v}.xyz, {ms}.xyz)") });
                }
                while comps.len() < 4 {
                    comps.push("0.0".into());
                }
                Some(format!("vec4<f32>({})", comps.join(", ")))
            }
            TEX | TEXLDL | TEXLDD => Some(self.tex_expr(ins)?),
            TEXKILL => {
                let dd = d.unwrap();
                let name = self.reg_name(dd.ty, dd.reg);
                let comps: Vec<String> = (0..4).filter(|k| dd.mask == 0 || dd.mask & (1 << k) != 0).map(|k| format!("{name}.{} < 0.0", COMP[k])).collect();
                let comps = if comps.is_empty() { vec![format!("{name}.x < 0.0")] } else { comps };
                self.line(&format!("if ({}) {{ discard; }}", comps.join(" || ")));
                None
            }
            SETP => {
                let c = Self::cmp_op(ins.control);
                Some(format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {} {c} {})", a(self, 0), a(self, 1)))
            }
            IF => {
                let s = &ins.src[0];
                let cond = if s.ty == CONSTBOOL {
                    let b = if self.sh.defb.contains_key(&s.reg) {
                        format!("b{}", s.reg)
                    } else {
                        self.iface.max_bool = self.iface.max_bool.max(s.reg);
                        format!("({}kb[{}] != 0u)", self.p, s.reg)
                    };
                    if s.modifier == 13 { format!("!{b}") } else { b }
                } else {
                    // predicate register
                    let ps = self.src(s);
                    format!("{ps}.x != 0.0")
                };
                self.line(&format!("if ({cond}) {{"));
                self.indent += 1;
                self.branch_depth += 1;
                None
            }
            IFC => {
                let c = Self::cmp_op(ins.control);
                let (x, y) = (self.src1(&ins.src[0]), self.src1(&ins.src[1]));
                self.line(&format!("if ({x} {c} {y}) {{"));
                self.indent += 1;
                self.branch_depth += 1;
                None
            }
            ELSE => {
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
                None
            }
            ENDIF => {
                self.indent -= 1;
                self.branch_depth -= 1;
                self.line("}");
                None
            }
            REP => {
                let s = &ins.src[0];
                let count = if self.sh.defi.contains_key(&s.reg) {
                    format!("i{}.x", s.reg)
                } else {
                    self.iface.max_int = self.iface.max_int.max(s.reg);
                    format!("{}ki[{}].x", self.p, s.reg)
                };
                self.line(&format!("for (var rep{0}: i32 = 0; rep{0} < {count}; rep{0}++) {{", self.loop_depth));
                self.indent += 1;
                self.loop_depth += 1;
                None
            }
            LOOP => {
                let s = &ins.src[1];
                let iv = if self.sh.defi.contains_key(&s.reg) {
                    format!("i{}", s.reg)
                } else {
                    self.iface.max_int = self.iface.max_int.max(s.reg);
                    format!("{}ki[{}]", self.p, s.reg)
                };
                self.line(&format!("aL = {iv}.y;"));
                self.line(&format!("for (var rep{0}: i32 = 0; rep{0} < {iv}.x; rep{0}++) {{", self.loop_depth));
                self.indent += 1;
                self.loop_depth += 1;
                self.line(&format!("let step{} = {iv}.z;", self.loop_depth));
                None
            }
            ENDREP => {
                self.indent -= 1;
                self.loop_depth -= 1;
                self.line("}");
                None
            }
            ENDLOOP => {
                self.line(&format!("aL = aL + step{};", self.loop_depth));
                self.indent -= 1;
                self.loop_depth -= 1;
                self.line("}");
                None
            }
            BREAK => {
                self.line("break;");
                None
            }
            BREAKC => {
                let c = Self::cmp_op(ins.control);
                let (x, y) = (self.src1(&ins.src[0]), self.src1(&ins.src[1]));
                self.line(&format!("if ({x} {c} {y}) {{ break; }}"));
                None
            }
            BREAKP => {
                let ps = self.src(&ins.src[0]);
                self.line(&format!("if ({ps}.x != 0.0) {{ break; }}"));
                None
            }
            o => bail!("unhandled opcode {o}"),
        };
        if let (Some(e), Some(dd)) = (expr, d) {
            if ins.op == SETP {
                self.assign(&Dst { ty: PREDICATE, ..dd }, &e, None);
            } else {
                self.assign(&dd, &e, pred.as_ref());
            }
        }
        Ok(())
    }
}

/// Translate to WGSL: a function `{prefix}main(io: ptr<function, {prefix}Io>)` operating
/// on the register file. The caller declares the `{prefix}Io` struct (inputs `vN` / `tN`,
/// outputs `oN` / `ocN`, `vpos`, `vface`, `odepth`), the constant arrays `{prefix}kc`
/// (vec4<f32>), `{prefix}ki` (vec4<i32>) and `{prefix}kb` (u32), and the sampling helpers
/// `{prefix}smpN(uv)`, `_l(uv, lod)`, `_b(uv, bias)`, `_g(uv, ddx, ddy)` of the kinds
/// listed in the interface.
pub fn translate(sh: &Shader, prefix: &str) -> Result<(String, Interface)> {
    let mut e = Emitter { sh, p: prefix.to_string(), out: String::new(), indent: 1, branch_depth: 0, loop_depth: 0, iface: Interface::default() };
    for d in &sh.decls {
        match d.ty {
            SAMPLER => {
                e.iface.samplers.insert(d.reg, d.tex.max(2));
            }
            INPUT if sh.stage == Some(Stage::Pixel) => e.iface.inputs.push((varying_location(d.usage, d.index), d.ty, d.reg, d.usage, d.index)),
            INPUT => e.iface.inputs.push((d.reg, d.ty, d.reg, d.usage, d.index)),
            OUTPUT => e.iface.outputs.push((varying_location(d.usage, d.index), d.ty, d.reg, d.usage, d.index)),
            MISCTYPE => {}
            _ => {}
        }
    }
    // registers
    let mut temps = BTreeSet::new();
    let mut outs = BTreeSet::new();
    let mut preds = false;
    let mut addr = false;
    let mut colors = BTreeSet::new();
    let mut depth = false;
    let mut loops = false;
    for ins in &sh.instrs {
        if let Some(d) = ins.dst {
            match d.ty {
                TEMP => {
                    temps.insert(d.reg);
                }
                OUTPUT => {
                    outs.insert(d.reg);
                }
                PREDICATE => preds = true,
                ADDR if sh.stage == Some(Stage::Vertex) => addr = true,
                COLOROUT => {
                    colors.insert(d.reg);
                }
                DEPTHOUT => depth = true,
                _ => {}
            }
        }
        if ins.op == op::SETP {
            preds = true;
        }
        if ins.op == op::LOOP {
            loops = true;
        }
        for s in &ins.src {
            if s.ty == TEMP {
                temps.insert(s.reg);
            }
        }
    }
    let mut decl = String::new();
    for r in &temps {
        let _ = writeln!(decl, "    var r{r}: vec4<f32> = vec4<f32>(0.0);");
    }
    for (r, v) in &sh.defs {
        let _ = writeln!(decl, "    let c{r} = vec4<f32>({:?}, {:?}, {:?}, {:?});", v[0], v[1], v[2], v[3]);
    }
    for (r, v) in &sh.defi {
        let _ = writeln!(decl, "    let i{r} = vec4<i32>({}, {}, {}, {});", v[0], v[1], v[2], v[3]);
    }
    for (r, v) in &sh.defb {
        let _ = writeln!(decl, "    let b{r} = {v};");
    }
    if preds {
        decl.push_str("    var p0: vec4<f32> = vec4<f32>(0.0);\n");
    }
    if addr {
        decl.push_str("    var a0: vec4<f32> = vec4<f32>(0.0);\n");
    }
    if loops {
        decl.push_str("    var aL: i32 = 0;\n");
    }
    // inputs / outputs are fields of `io`
    for &(_, ty, reg, _, _) in &e.iface.inputs.clone() {
        let n = if ty == INPUT { format!("v{reg}") } else { format!("t{reg}") };
        let _ = writeln!(decl, "    let {n} = (*io).{n};");
    }
    if sh.decls.iter().any(|d| d.ty == MISCTYPE && d.reg == 0) {
        decl.push_str("    let vpos = (*io).vpos;\n");
    }
    if sh.decls.iter().any(|d| d.ty == MISCTYPE && d.reg == 1) {
        decl.push_str("    let vface = (*io).vface;\n");
    }
    for r in &outs {
        let _ = writeln!(decl, "    var o{r}: vec4<f32> = vec4<f32>(0.0);");
    }
    for r in &colors {
        let _ = writeln!(decl, "    var oc{r}: vec4<f32> = vec4<f32>(0.0);");
    }
    if depth {
        decl.push_str("    var odepth: vec4<f32> = vec4<f32>(0.0);\n");
    }
    for ins in &sh.instrs {
        e.instr(ins)?;
    }
    let mut body = format!("fn {prefix}main(io: ptr<function, {prefix}Io>) {{\n");
    body.push_str(&decl);
    body.push_str(&e.out);
    for r in &outs {
        let _ = writeln!(body, "    (*io).o{r} = o{r};");
    }
    for r in &colors {
        let _ = writeln!(body, "    (*io).oc{r} = oc{r};");
    }
    if depth {
        body.push_str("    (*io).odepth = odepth.x;\n");
    }
    body.push_str("}\n");
    Ok((body, e.iface))
}

/// Declarations a translated stage needs: its `Io` struct and constant arrays.
pub fn stage_decls(sh: &Shader, iface: &Interface, prefix: &str) -> String {
    let mut m = String::new();
    let n_const = if iface.uses_rel_const { 256 } else { iface.max_const + 1 };
    let _ = writeln!(m, "var<private> {prefix}kc: array<vec4<f32>, {n_const}>;");
    let _ = writeln!(m, "var<private> {prefix}ki: array<vec4<i32>, {}>;", iface.max_int + 1);
    let _ = writeln!(m, "var<private> {prefix}kb: array<u32, {}>;", iface.max_bool + 1);
    let _ = writeln!(m, "struct {prefix}Io {{");
    for &(_, ty, reg, _, _) in &iface.inputs {
        let n = if ty == INPUT { format!("v{reg}") } else { format!("t{reg}") };
        let _ = writeln!(m, "    {n}: vec4<f32>,");
    }
    m.push_str("    vpos: vec4<f32>,\n    vface: vec4<f32>,\n");
    for r in output_regs(sh, OUTPUT) {
        let _ = writeln!(m, "    o{r}: vec4<f32>,");
    }
    for r in output_regs(sh, COLOROUT) {
        let _ = writeln!(m, "    oc{r}: vec4<f32>,");
    }
    m.push_str("    odepth: f32,\n}\n");
    m
}

/// Registers of a type the shader writes.
pub fn output_regs(sh: &Shader, ty: u32) -> BTreeSet<u32> {
    sh.instrs.iter().filter_map(|i| i.dst).filter(|d| d.ty == ty).map(|d| d.reg).collect()
}

/// Sampling helper functions of a stage over the given texture / sampler expressions
/// (`tex(reg)` -> (texture, sampler, extra args such as an array layer, wrapper of the
/// result with `{}` for the sample or empty)).
pub fn sampling_helpers(iface: &Interface, prefix: &str, vertex: bool, tex: &dyn Fn(u32) -> (String, String, String, String)) -> String {
    let mut m = String::new();
    for (&s, &kinds) in &iface.sample_kinds {
        let dim = *iface.samplers.get(&s).unwrap_or(&2);
        let uvt = if dim == 2 { "vec2<f32>" } else { "vec3<f32>" };
        let (t, smp, extra, wrap) = tex(s);
        let f = format!("{prefix}smp{s}");
        let start = m.len();
        if kinds & 1 != 0 {
            if vertex {
                let _ = writeln!(m, "fn {f}(uv: {uvt}) -> vec4<f32> {{ return textureSampleLevel({t}, {smp}, uv{extra}, 0.0); }}");
            } else {
                let _ = writeln!(m, "fn {f}(uv: {uvt}) -> vec4<f32> {{ return textureSample({t}, {smp}, uv{extra}); }}");
            }
        }
        if kinds & 2 != 0 {
            let _ = writeln!(m, "fn {f}_l(uv: {uvt}, lod: f32) -> vec4<f32> {{ return textureSampleLevel({t}, {smp}, uv{extra}, lod); }}");
        }
        if kinds & 4 != 0 {
            if vertex {
                let _ = writeln!(m, "fn {f}_b(uv: {uvt}, bias: f32) -> vec4<f32> {{ return textureSampleLevel({t}, {smp}, uv{extra}, bias); }}");
            } else {
                let _ = writeln!(m, "fn {f}_b(uv: {uvt}, bias: f32) -> vec4<f32> {{ return textureSampleBias({t}, {smp}, uv{extra}, bias); }}");
            }
        }
        if kinds & 8 != 0 {
            let _ = writeln!(m, "fn {f}_g(uv: {uvt}, gx: {uvt}, gy: {uvt}) -> vec4<f32> {{ return textureSampleGrad({t}, {smp}, uv{extra}, gx, gy); }}");
        }
        if !wrap.is_empty() {
            // wrap each helper's result: "return X;" -> "return wrap(X);"
            let body = m.split_off(start);
            for line in body.lines() {
                let (head, rest) = line.split_once("return ").unwrap_or((line, ""));
                let expr = rest.trim_end_matches("; }");
                let _ = writeln!(m, "{head}return {}; }}", wrap.replace("{}", expr));
            }
        }
    }
    m
}

/// A self-contained module for validation: generic bindings and entry point.
pub fn standalone_module(sh: &Shader, body: &str, iface: &Interface) -> String {
    let mut m = String::new();
    let p = "sm3_";
    for (&s, &dim) in &iface.samplers {
        let ty = match dim {
            3 => "texture_cube<f32>",
            4 => "texture_3d<f32>",
            _ => "texture_2d<f32>",
        };
        let _ = writeln!(m, "@group(0) @binding({}) var t_{s}: {ty};", 1 + s * 2);
        let _ = writeln!(m, "@group(0) @binding({}) var s_{s}: sampler;", 2 + s * 2);
    }
    let vertex = sh.stage == Some(Stage::Vertex);
    m.push_str(&sampling_helpers(iface, p, vertex, &|s| (format!("t_{s}"), format!("s_{s}"), String::new(), String::new())));
    m.push_str(&stage_decls(sh, iface, p));
    m.push_str(body);
    let colors = output_regs(sh, COLOROUT);
    match sh.stage {
        Some(Stage::Pixel) => {
            m.push_str("struct FsIn {\n    @builtin(position) pos: vec4<f32>,\n    @builtin(front_facing) face: bool,\n");
            for &(loc, ty, reg, _, _) in &iface.inputs {
                let n = if ty == INPUT { format!("v{reg}") } else { format!("t{reg}") };
                let _ = writeln!(m, "    @location({loc}) {n}: vec4<f32>,");
            }
            m.push_str("};\n@fragment\nfn main(fin: FsIn) -> @location(0) vec4<f32> {\n    var io: sm3_Io;\n");
            for &(_, ty, reg, _, _) in &iface.inputs {
                let n = if ty == INPUT { format!("v{reg}") } else { format!("t{reg}") };
                let _ = writeln!(m, "    io.{n} = fin.{n};");
            }
            m.push_str("    io.vpos = fin.pos;\n    io.vface = vec4<f32>(select(-1.0, 1.0, fin.face));\n    sm3_main(&io);\n");
            if colors.contains(&0) {
                m.push_str("    return io.oc0;\n}\n");
            } else {
                m.push_str("    return vec4<f32>(0.0);\n}\n");
            }
        }
        _ => {
            m.push_str("struct VsIn {\n");
            for &(loc, _, reg, _, _) in &iface.inputs {
                let _ = writeln!(m, "    @location({loc}) v{reg}: vec4<f32>,");
            }
            m.push_str("};\nstruct VsOut {\n    @builtin(position) pos: vec4<f32>,\n");
            for &(loc, _, reg, usage, _) in &iface.outputs {
                if usage == 0 {
                    continue;
                }
                let _ = writeln!(m, "    @location({loc}) o{reg}: vec4<f32>,");
            }
            m.push_str("};\n@vertex\nfn main(vin: VsIn) -> VsOut {\n    var io: sm3_Io;\n");
            for &(_, _, reg, _, _) in &iface.inputs {
                let _ = writeln!(m, "    io.v{reg} = vin.v{reg};");
            }
            m.push_str("    sm3_main(&io);\n    var out: VsOut;\n");
            for &(_, _, reg, usage, _) in &iface.outputs {
                if usage == 0 {
                    let _ = writeln!(m, "    out.pos = io.o{reg};");
                } else {
                    let _ = writeln!(m, "    out.o{reg} = io.o{reg};");
                }
            }
            m.push_str("    return out;\n}\n");
        }
    }
    m
}

/// Parse and validate a WGSL module with naga; the error message on failure.
pub fn validate(src: &str) -> std::result::Result<naga::Module, String> {
    let module = naga::front::wgsl::parse_str(src).map_err(|e| e.emit_to_string(src))?;
    let mut v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
    v.validate(&module).map_err(|e| {
        let mut s = e.emit_to_string(src);
        let mut src_err: Option<&dyn std::error::Error> = std::error::Error::source(e.as_inner());
        while let Some(x) = src_err {
            s.push_str(&format!("\n  caused by: {x}"));
            src_err = x.source();
        }
        s
    })?;
    Ok(module)
}

/// Evaluate an ALU-only shader on the CPU (no texture sampling or flow control): inputs by
/// register (`v`), float constants by register (`c`); returns colour output 0.
pub fn eval(sh: &Shader, v: &[[f32; 4]], c: &BTreeMap<u32, [f32; 4]>) -> Result<[f32; 4]> {
    type V = [f32; 4];
    let mut temps: BTreeMap<u32, V> = BTreeMap::new();
    let mut out = [0.0f32; 4];
    let read = |temps: &BTreeMap<u32, V>, s: &Src| -> Result<V> {
        let base: V = match s.ty {
            TEMP => temps.get(&s.reg).copied().unwrap_or([0.0; 4]),
            INPUT => v.get(s.reg as usize).copied().unwrap_or([0.0; 4]),
            CONST => sh.defs.get(&s.reg).or_else(|| c.get(&s.reg)).copied().unwrap_or([0.0; 4]),
            t => bail!("eval: unsupported source register type {t}"),
        };
        let mut x = [0.0; 4];
        for k in 0..4 {
            x[k] = base[s.swizzle[k] as usize];
        }
        Ok(match s.modifier {
            0 => x,
            1 => x.map(|a| -a),
            11 => x.map(f32::abs),
            12 => x.map(|a| -a.abs()),
            6 => x.map(|a| 1.0 - a),
            m => bail!("eval: unsupported source modifier {m}"),
        })
    };
    for ins in &sh.instrs {
        let a = |i: usize| read(&temps, &ins.src[i]);
        let map2 = |x: V, y: V, f: &dyn Fn(f32, f32) -> f32| -> V { [f(x[0], y[0]), f(x[1], y[1]), f(x[2], y[2]), f(x[3], y[3])] };
        let splat = |x: f32| [x; 4];
        let r: V = match ins.op {
            op::NOP => continue,
            op::MOV => a(0)?,
            op::ADD => map2(a(0)?, a(1)?, &|x, y| x + y),
            op::SUB => map2(a(0)?, a(1)?, &|x, y| x - y),
            op::MUL => map2(a(0)?, a(1)?, &|x, y| x * y),
            op::MAD => {
                let (x, y, z) = (a(0)?, a(1)?, a(2)?);
                [x[0] * y[0] + z[0], x[1] * y[1] + z[1], x[2] * y[2] + z[2], x[3] * y[3] + z[3]]
            }
            op::MIN => map2(a(0)?, a(1)?, &|x, y| x.min(y)),
            op::MAX => map2(a(0)?, a(1)?, &|x, y| x.max(y)),
            op::RCP => splat(1.0 / a(0)?[0]),
            op::RSQ => splat(1.0 / a(0)?[0].abs().sqrt()),
            op::EXP | op::EXPP => splat(a(0)?[0].exp2()),
            op::LOG | op::LOGP => splat(a(0)?[0].abs().log2()),
            op::POW => splat(a(0)?[0].abs().powf(a(1)?[0])),
            op::FRC => a(0)?.map(|x| x - x.floor()),
            op::ABS => a(0)?.map(f32::abs),
            op::DP3 => {
                let (x, y) = (a(0)?, a(1)?);
                splat(x[0] * y[0] + x[1] * y[1] + x[2] * y[2])
            }
            op::DP4 => {
                let (x, y) = (a(0)?, a(1)?);
                splat(x[0] * y[0] + x[1] * y[1] + x[2] * y[2] + x[3] * y[3])
            }
            op::DP2ADD => {
                let (x, y, z) = (a(0)?, a(1)?, a(2)?);
                splat(x[0] * y[0] + x[1] * y[1] + z[0])
            }
            op::LRP => {
                let (t, x, y) = (a(0)?, a(1)?, a(2)?);
                [y[0] + (x[0] - y[0]) * t[0], y[1] + (x[1] - y[1]) * t[1], y[2] + (x[2] - y[2]) * t[2], y[3] + (x[3] - y[3]) * t[3]]
            }
            op::CMP => {
                let (t, x, y) = (a(0)?, a(1)?, a(2)?);
                [0, 1, 2, 3].map(|k| if t[k] >= 0.0 { x[k] } else { y[k] })
            }
            op::SLT => map2(a(0)?, a(1)?, &|x, y| if x < y { 1.0 } else { 0.0 }),
            op::SGE => map2(a(0)?, a(1)?, &|x, y| if x >= y { 1.0 } else { 0.0 }),
            o => bail!("eval: unsupported opcode {o}"),
        };
        let Some(d) = ins.dst else { continue };
        let r = if d.saturate { r.map(|x| x.clamp(0.0, 1.0)) } else { r };
        let target: &mut V = match d.ty {
            TEMP => temps.entry(d.reg).or_insert([0.0; 4]),
            COLOROUT if d.reg == 0 => &mut out,
            t => bail!("eval: unsupported destination register type {t}"),
        };
        for k in 0..4 {
            if d.mask == 0 || d.mask & (1 << k) != 0 {
                target[k] = r[k];
            }
        }
    }
    Ok(out)
}
