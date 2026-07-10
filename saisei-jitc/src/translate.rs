//! Shared translation front-half: IR instruction utilities, operand rewriting
//! (`rewrite_mem_op`, RCB/exec_params naming, stack-var decoding), flag
//! normalization, basic-block construction, and CFG successors. `codegen.rs`
//! (the chunk emitter) builds on everything here; nothing in this module
//! renders output text itself.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::OnceLock;

use regex::Regex;

pub type Insn = serde_json::Map<String, Value>;

fn s<'a>(i: &'a Insn, k: &str) -> &'a str {
    i.get(k).and_then(Value::as_str).unwrap_or("")
}
fn i64f(i: &Insn, k: &str) -> Option<i64> {
    i.get(k).and_then(Value::as_i64)
}
fn isize_bytes(i: &Insn) -> i64 {
    (s(i, "bytes").len() / 2) as i64
}
fn set_str(i: &mut Insn, k: &str, v: &str) {
    i.insert(k.into(), Value::String(v.into()));
}

/// the reference truthiness test (for `if not insn.get("skip")`): keep an insn when
/// its "skip" is absent or falsy.
fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// the reference `int(s, 16)`: hex parse allowing an optional 0x/0X prefix (and sign),
/// rejecting any trailing junk (e.g. a `…h` suffix) — used by handle_jmp/jcc.
fn parse_hex(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, t) = t.strip_prefix('-').map_or((false, t), |r| (true, r));
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    i64::from_str_radix(t, 16)
        .ok()
        .map(|v| if neg { -v } else { v })
}

// ===================== module-level helpers =====================

/// the source `_parse_imm` (bare decimal stays DECIMAL — differs from disasm's).
pub fn parse_imm(value: &str) -> Option<i64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:short|near)?\s*([+-]?(?:0x[0-9a-f]+|[0-9a-f]+h|[0-9a-f]+))$").unwrap()
    });
    let value = value.trim();
    if value.contains('[') || value.contains(']') || value.contains(':') {
        return None;
    }
    let caps = re.captures(value)?;
    let mut token = caps.get(1)?.as_str().to_string();
    let mut sign = String::new();
    if token.starts_with(['+', '-']) {
        sign = token[..1].to_string();
        token = token[1..].to_string();
    }
    if token.ends_with(['h', 'H']) {
        token = format!("0x{}", &token[..token.len() - 1]);
    } else if token.to_lowercase().starts_with("0x") {
    } else if token.chars().all(|c| c.is_ascii_digit()) {
        // bare decimal -> decimal
    } else {
        token = format!("0x{token}");
    }
    let combined = format!("{sign}{token}");
    let (neg, rest) = match combined.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, combined.strip_prefix('+').unwrap_or(&combined)),
    };
    let v = match rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        Some(h) => i64::from_str_radix(h, 16).ok()?,
        None => {
            // Mirror the reference `_parse_imm` bare-decimal: a leading-zero NON-zero
            // token (e.g. "0109", "0042") -> None, but an all-zero token
            // ("0", "00", "0000") -> 0, and "42" -> 42.
            if rest.len() > 1 && rest.starts_with('0') && rest.bytes().any(|b| b != b'0') {
                return None;
            }
            rest.parse::<i64>().ok()?
        }
    };
    Some(if neg { -v } else { v })
}

/// the reference `int(s, 16)` — optional sign, optional 0x, base-16.
fn parse_hex16(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let rest = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))
        .unwrap_or(rest);
    let v = i64::from_str_radix(rest, 16).ok()?;
    Some(if neg { -v } else { v })
}

fn mem_ptr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(byte|word) ptr (?:(cs|ds|es|ss):)?\[(.+)\]$").unwrap())
}

/// _decode_variables — rewrite bp/sp-relative operands to var_X pseudo-names.
pub fn decode_variables(op_str: &str) -> String {
    fn bp_name(sign: &str, num: &str) -> String {
        let parsed = if num.to_lowercase().starts_with("0x") {
            i64::from_str_radix(&num[2..], 16)
        } else {
            num.parse::<i64>()
        };
        match parsed {
            Ok(off) => format!("var_{off:X}"),
            Err(_) => format!("bp {sign} {num}"),
        }
    }
    fn bp_var_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"(?i)^\[(?:[bs]p)\s*([+-])\s*(0x[0-9a-fA-F]+|\d+)\]$").unwrap()
        })
    }
    fn bp_inner_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"(?i)^(?:[bs]p)\s*([+-])\s*(0x[0-9a-fA-F]+|\d+)$").unwrap())
    }
    fn recurse(expr: &str) -> String {
        let b = expr.as_bytes();
        let mut result = String::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'[' {
                let mut depth = 1;
                let mut j = i + 1;
                while j < b.len() && depth > 0 {
                    if b[j] == b'[' {
                        depth += 1;
                    } else if b[j] == b']' {
                        depth -= 1;
                    }
                    j += 1;
                }
                let segment = &expr[i..j];
                if let Some(c) = bp_var_re().captures(segment) {
                    let sign = c.get(1).unwrap().as_str();
                    if sign == "-" {
                        result.push_str(&bp_name(sign, c.get(2).unwrap().as_str()));
                    } else {
                        result.push_str(segment);
                    }
                } else {
                    let inner = recurse(&segment[1..segment.len() - 1]);
                    result.push('[');
                    result.push_str(&inner);
                    result.push(']');
                }
                i = j;
            } else {
                result.push(b[i] as char);
                i += 1;
            }
        }
        if let Some(c) = bp_inner_re().captures(result.trim()) {
            if c.get(1).unwrap().as_str() == "-" {
                return bp_name("-", c.get(2).unwrap().as_str());
            }
            return format!("bp + {}", c.get(2).unwrap().as_str());
        }
        result
    }
    recurse(op_str)
}

/// _rewrite_mem_op — mem operand → `memb`/`memw` accessor text, with the
/// exec_params (cs:0x8C2/0x8C4) and RCB (es:0xFFxx) named-field rewrites.
pub fn rewrite_mem_op(op: &str, seg: Option<&str>) -> String {
    if let Some(c) = mem_ptr_re().captures(op) {
        let size = c.get(1).unwrap().as_str();
        let seg_prefix = c.get(2).map(|m| m.as_str());
        let expr = c.get(3).unwrap().as_str();
        let expr_l = expr.to_lowercase();
        let seg_final: String = if let Some(sp) = seg_prefix {
            sp.to_lowercase()
        } else if let Some(sg) = seg {
            sg.to_lowercase()
        } else if Regex::new(r"\b(?:bp|sp)\b").unwrap().is_match(&expr_l) {
            "ss".into()
        } else {
            "ds".into()
        };
        // Constant-offset field maps (exec params; RCB es:0xFF.. still TODO).
        let off_const: Option<i64> = if expr_l.starts_with("0x") || expr_l.starts_with("-0x") {
            parse_hex16(expr).map(|v| v & 0xFFFF)
        } else {
            expr.parse::<i64>().ok().map(|v| v & 0xFFFF)
        };
        if let Some(off) = off_const {
            if seg_final == "cs" && off == 0x8C2 {
                return "exec_params.saved_sp".to_string();
            }
            if seg_final == "cs" && off == 0x8C4 {
                return "exec_params.saved_ss".to_string();
            }
            if seg_final == "es" {
                if let Some(macro_) = rcb_macro(off) {
                    let (aliases, _) = rcb_aliases();
                    let addr = aliases
                        .get(&off)
                        .cloned()
                        .unwrap_or_else(|| format!("0x{off:04X}"));
                    return format!("{macro_}(es, {addr})");
                }
            }
        }
        let macro_ = if size.to_lowercase() == "byte" {
            "memb"
        } else {
            "memw"
        };
        if expr.contains('+') || expr.contains('-') {
            return format!("{macro_}({seg_final}, ({expr}) & 0xFFFF)");
        }
        return format!("{macro_}({seg_final}, {expr})");
    }
    static SIMPLE: OnceLock<Regex> = OnceLock::new();
    let simple = SIMPLE.get_or_init(|| Regex::new(r"^(?:byte|word) ptr (.+)$").unwrap());
    if let Some(c) = simple.captures(op) {
        return c.get(1).unwrap().as_str().to_string();
    }
    op.to_string()
}

fn simple_ptr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(byte|word) ptr (.+)$").unwrap())
}
fn var_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^var_([0-9a-f]+)$").unwrap())
}

const BYTE_REGS: &[&str] = &["al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"];
const WORD_REGS: &[&str] = &["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"];

/// _operand_width_from_str.
fn operand_width_from_str(expr: &str) -> Option<i64> {
    let e = expr.trim().to_lowercase();
    if BYTE_REGS.contains(&e.as_str()) {
        return Some(8);
    }
    if WORD_REGS.contains(&e.as_str()) {
        return Some(16);
    }
    if e.starts_with("memb(") {
        return Some(8);
    }
    if e.starts_with("memw(") {
        return Some(16);
    }
    None
}
/// _operand_width.
fn operand_width(op: &str) -> i64 {
    let op_l = op.to_lowercase();
    if op_l.starts_with("memb(") {
        return 8;
    }
    if op_l.starts_with("memw(") {
        return 16;
    }
    if BYTE_REGS.contains(&op_l.as_str()) {
        return 8;
    }
    16
}
/// _fix_negative_imm.
pub(crate) fn fix_negative_imm(op: &str, other: &str) -> String {
    match parse_imm(op) {
        Some(v) if v < 0 => {
            let width = operand_width(other);
            let mask = (1i64 << width) - 1;
            let imm = v & mask;
            let digits = (width / 4) as usize;
            format!("0x{imm:0digits$X}")
        }
        _ => op.to_string(),
    }
}
/// Resident Control Block field layout — the single source of truth (was
/// the retired rcb_fields.h). Mirrored as `pub const`s in
/// rt/saisei_rt.rs; the `rcb_prelude_consts_match` test keeps them in sync.
/// Word-sized fields are listed in `rcb_macro` below.
pub(crate) const RCB_FIELDS: &[(i64, &str)] = &[
    (0xFF00, "FIELD_1"),
    (0xFF02, "PROGRAM_SEG"),
    (0xFF04, "PREV_TIMER_VECTOR_OFF"),
    (0xFF06, "PREV_TIMER_VECTOR_SEG"),
    (0xFF08, "FIELD_5"),
    (0xFF09, "FIELD_6"),
    (0xFF0A, "JOYSTICK_FLAG"),
    (0xFF0B, "FIELD_8"),
    (0xFF0C, "DATA_BUF1_OFF"),
    (0xFF0E, "DATA_BUF1_SEG"),
    (0xFF10, "DATA_BUF2_OFF"),
    (0xFF12, "DATA_BUF2_SEG"),
    (0xFF14, "VIDEO_DRIVER_INDEX"),
    (0xFF15, "MUSIC_DRIVER_FLAG"),
    (0xFF16, "FIELD_15"),
    (0xFF17, "FIELD_16"),
    (0xFF18, "FIELD_17"),
    (0xFF1D, "FIELD_18"),
    (0xFF1E, "FIELD_19"),
    (0xFF1F, "FIELD_20"),
    (0xFF26, "FIELD_21"),
    (0xFF27, "FIELD_22"),
    (0xFF28, "FIELD_23"),
    (0xFF2C, "DATA_BASE_SEG"),
    (0xFF33, "FIELD_25"),
    (0xFF34, "FIELD_26"),
    (0xFF38, "FIELD_27"),
    (0xFF39, "FIELD_28"),
    (0xFF3A, "FIELD_29"),
    (0xFF3B, "FIELD_30"),
    (0xFF3C, "FIELD_31"),
    (0xFF40, "FIELD_32"),
    (0xFF42, "FIELD_33"),
    (0xFF43, "FIELD_34"),
    (0xFF74, "FIELD_35"),
    (0xFF75, "FIELD_36"),
    (0xFF78, "FIELD_37"),
    (0xFF79, "PREV_KEYBOARD_VECTOR_OFF"),
    (0xFF7B, "PREV_KEYBOARD_VECTOR_SEG"),
];

/// _RCB_FIELD_TYPES -> "memb"/"memw" (None if off not an RCB field).
fn rcb_macro(off: i64) -> Option<&'static str> {
    match off {
        0xFF00 | 0xFF02 | 0xFF04 | 0xFF06 | 0xFF0C | 0xFF0E | 0xFF10 | 0xFF12 | 0xFF18 | 0xFF1F
        | 0xFF2C | 0xFF79 | 0xFF7B => Some("memw"),
        0xFF08 | 0xFF09 | 0xFF0A | 0xFF0B | 0xFF14 | 0xFF15 | 0xFF16 | 0xFF17 | 0xFF1D | 0xFF1E
        | 0xFF26 | 0xFF27 | 0xFF28 | 0xFF33 | 0xFF34 | 0xFF38 | 0xFF39 | 0xFF3A | 0xFF3B
        | 0xFF3C | 0xFF40 | 0xFF42 | 0xFF43 | 0xFF74 | 0xFF75 | 0xFF78 => Some("memb"),
        _ => None,
    }
}

/// _RCB_FIELD_ALIASES / _RCB_FIELD_NAMES, from the RCB_FIELDS table.
fn rcb_aliases() -> &'static (HashMap<i64, String>, std::collections::HashSet<String>) {
    static A: OnceLock<(HashMap<i64, String>, std::collections::HashSet<String>)> = OnceLock::new();
    A.get_or_init(|| {
        let mut map = HashMap::new();
        let mut names = std::collections::HashSet::new();
        for &(off, name) in RCB_FIELDS {
            names.insert(name.to_string());
            map.insert(off, name.to_string());
        }
        (map, names)
    })
}

fn infer_stack_var_width(operands: &[String], index: usize) -> i64 {
    for (i, other) in operands.iter().enumerate() {
        if i == index {
            continue;
        }
        if let Some(w) = operand_width_from_str(other) {
            return w;
        }
    }
    16
}

pub fn jcc_condition(mnemonic: &str, _prev: Option<&Insn>, address: Option<i64>) -> String {
    let m = mnemonic.to_lowercase();
    match m.as_str() {
        "jmp" => return "1".into(),
        "loop" => return "--cx != 0".into(),
        "loopne" | "loopnz" => return format!("--cx != 0 && {}", jcc_condition("jnz", None, None)),
        "loope" | "loopz" => return format!("--cx != 0 && {}", jcc_condition("jz", None, None)),
        "jcxz" => return "cx == 0".into(),
        "jecxz" => return "ecx == 0".into(),
        _ => {}
    }
    let cond = match m.as_str() {
        "jz" | "je" => "ZF == 1",
        "jnz" | "jne" => "ZF == 0",
        "jc" => "CF == 1",
        "jnc" => "CF == 0",
        "js" => "SF == 1",
        "jns" => "SF == 0",
        "jo" => "OF == 1",
        "jno" => "OF == 0",
        "jpo" | "jnp" => "PF == 0",
        "jpe" | "jp" => "PF == 1",
        "jg" => "ZF == 0 && SF == OF",
        "jge" => "SF == OF",
        "jl" => "SF != OF",
        "jle" => "ZF == 1 || SF != OF",
        "ja" => "CF == 0 && ZF == 0",
        "jae" => "CF == 0",
        "jb" => "CF == 1",
        "jbe" | "jna" => "CF == 1 || ZF == 1",
        "jnbe" => "CF == 0 && ZF == 0",
        "jnb" => "CF == 0",
        "jnae" => "CF == 1",
        "jnge" => "SF != OF",
        "jng" => "ZF == 1 || SF != OF",
        "jnl" => "SF == OF",
        "jnle" => "ZF == 0 && SF == OF",
        _ => {
            return match address {
                Some(a) => format!("/* unsupported jcc at 0x{a:04X} */"),
                None => "/* unsupported jcc */".into(),
            }
        }
    };
    cond.to_string()
}

fn is_jump_family(mnem: &str) -> bool {
    mnem.starts_with('j')
        || mnem == "call"
        || mnem == "lcall"
        || mnem.starts_with("loop")
        || mnem == "ljmp"
}

// ===================== basic blocks / normalize / cfg =====================

#[derive(Clone)]
pub struct BasicBlock {
    pub start: i64,
    pub instructions: Vec<Insn>,
}

/// build_basic_blocks. Also stamps `_next_addr` on each insn.
pub fn build_basic_blocks(
    instrs: &[Insn],
    extra_leaders: &BTreeSet<i64>,
    func_start: Option<i64>,
) -> BTreeMap<i64, BasicBlock> {
    if instrs.is_empty() {
        return BTreeMap::new();
    }
    // targets for the overlap cleaner
    let mut targets: BTreeSet<i64> = BTreeSet::new();
    if let Some(fs) = func_start {
        targets.insert(fs);
    }
    for insn in instrs {
        let mnem = s(insn, "mnemonic");
        if is_jump_family(mnem) {
            let tgt = i64f(insn, "target").or_else(|| parse_imm(s(insn, "op_str")));
            if let Some(t) = tgt {
                targets.insert(t);
            }
        }
    }
    // overlap cleaner
    let mut cleaned: Vec<Insn> = Vec::new();
    let mut prev_addr: Option<i64> = None;
    let mut prev_size: i64 = 0;
    for insn in instrs {
        let addr = i64f(insn, "address").unwrap_or(0);
        let size = isize_bytes(insn);
        if let Some(pa) = prev_addr {
            if prev_size > 1 && addr < pa + prev_size && !targets.contains(&addr) {
                continue;
            }
        }
        cleaned.push(insn.clone());
        prev_addr = Some(addr);
        prev_size = size;
    }
    // stamp _next_addr
    for i in 0..cleaned.len().saturating_sub(1) {
        let next = i64f(&cleaned[i + 1], "address").unwrap_or(0);
        cleaned[i].insert("_next_addr".into(), Value::from(next));
    }

    let addr_set: BTreeSet<i64> = cleaned.iter().filter_map(|i| i64f(i, "address")).collect();
    let mut leaders: BTreeSet<i64> = BTreeSet::new();
    leaders.insert(i64f(&cleaned[0], "address").unwrap_or(0));
    if let Some(fs) = func_start {
        if addr_set.contains(&fs) {
            leaders.insert(fs);
        }
    }
    leaders.extend(extra_leaders.iter().cloned());

    for insn in &cleaned {
        let mnem = s(insn, "mnemonic");
        let size = isize_bytes(insn);
        let next_addr = i64f(insn, "address").unwrap_or(0) + size;
        if is_jump_family(mnem) {
            let target = i64f(insn, "target").or_else(|| parse_imm(s(insn, "op_str")));
            if let Some(t) = target {
                if addr_set.contains(&t) {
                    leaders.insert(t);
                }
            }
            if addr_set.contains(&next_addr) {
                leaders.insert(next_addr);
            }
        }
        // ret/hlt/iret: no leader added (continue)
    }

    let leaders_sorted: Vec<i64> = leaders.into_iter().collect();
    let mut blocks: BTreeMap<i64, BasicBlock> = BTreeMap::new();
    for (i, &start) in leaders_sorted.iter().enumerate() {
        let end = leaders_sorted.get(i + 1).cloned();
        let mut block = BasicBlock {
            start,
            instructions: Vec::new(),
        };
        for insn in &cleaned {
            let addr = i64f(insn, "address").unwrap_or(0);
            if addr < start {
                continue;
            }
            if let Some(e) = end {
                if addr >= e {
                    break;
                }
            }
            block.instructions.push(insn.clone());
        }
        if !block.instructions.is_empty() {
            blocks.insert(start, block);
        }
    }
    blocks
}

const FLAG_SETTERS: &[&str] = &[
    "cmp",
    "test",
    "add",
    "sub",
    "xor",
    "adc",
    "sbb",
    "shl",
    "shr",
    "sal",
    "sar",
    "rol",
    "ror",
    "rcl",
    "rcr",
    "or",
    "and",
    "stc",
    "clc",
    "cmc",
    "repe cmpsb",
    "scasb",
    "dec",
    "inc",
];

const REG_ALIASES: &[(&str, &[&str])] = &[
    ("al", &["al", "ax"]),
    ("ah", &["ah", "ax"]),
    ("ax", &["al", "ah", "ax"]),
    ("bl", &["bl", "bx"]),
    ("bh", &["bh", "bx"]),
    ("bx", &["bl", "bh", "bx"]),
    ("cl", &["cl", "cx"]),
    ("ch", &["ch", "cx"]),
    ("cx", &["cl", "ch", "cx"]),
    ("dl", &["dl", "dx"]),
    ("dh", &["dh", "dx"]),
    ("dx", &["dl", "dh", "dx"]),
];

fn expand_reg_aliases(regs: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for r in regs {
        let mut found = false;
        for (k, vs) in REG_ALIASES {
            if k == r {
                for v in *vs {
                    out.insert(v.to_string());
                }
                found = true;
                break;
            }
        }
        if !found {
            out.insert(r.clone());
        }
    }
    out
}

fn detail_regs(insn: &Insn, key: &str) -> BTreeSet<String> {
    insn.get("detail")
        .and_then(|d| d.get(key))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default()
}

fn clobbers_last_cond(insn: &Insn, last: Option<&Insn>) -> bool {
    let last = match last {
        Some(l) => l,
        None => return false,
    };
    let cond_regs = expand_reg_aliases(&detail_regs(last, "regs_read"));
    let insn_regs = expand_reg_aliases(&detail_regs(insn, "regs_write"));
    if !cond_regs.is_disjoint(&insn_regs) {
        return true;
    }
    let mem_key = |mr: &Value| -> String {
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}",
            mr.get("segment"),
            mr.get("base"),
            mr.get("index"),
            mr.get("scale"),
            mr.get("disp")
        )
    };
    let mem_of = |ins: &Insn, want: &[&str]| -> BTreeSet<String> {
        ins.get("detail")
            .and_then(|d| d.get("mem_refs"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|mr| {
                        mr.get("access")
                            .and_then(Value::as_str)
                            .map_or(false, |ac| want.contains(&ac))
                    })
                    .map(mem_key)
                    .collect()
            })
            .unwrap_or_default()
    };
    let cond_mem = mem_of(last, &["read", "readwrite"]);
    let insn_mem = mem_of(insn, &["write", "readwrite"]);
    !cond_mem.is_disjoint(&insn_mem)
}

/// normalize_flags.
pub fn normalize_flags(instrs: &[Insn]) -> Vec<Insn> {
    let mut normalized: Vec<Insn> = Vec::new();
    let mut last_cond: Option<Insn> = None;
    for insn in instrs {
        let mnem = s(insn, "mnemonic");
        if FLAG_SETTERS.contains(&mnem) {
            last_cond = Some(insn.clone());
            normalized.push(insn.clone());
            continue;
        }
        if (mnem.starts_with('j') || mnem.starts_with("loop"))
            && mnem != "jmp"
            && last_cond.is_some()
        {
            let mut new_insn = insn.clone();
            new_insn.insert(
                "cond_prev".into(),
                Value::Object(last_cond.clone().unwrap()),
            );
            normalized.push(new_insn);
            continue;
        }
        normalized.push(insn.clone());
        if clobbers_last_cond(insn, last_cond.as_ref()) || mnem == "int" || mnem == "popf" {
            last_cond = None;
        }
    }
    normalized
}

/// normalize_indirect_jumps.
pub fn normalize_indirect_jumps(instrs: &[Insn]) -> Vec<Insn> {
    const JMP_REGS: &[&str] = &["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"];
    let mut out = Vec::new();
    for insn in instrs {
        if s(insn, "mnemonic") == "jmp" {
            let op = s(insn, "op_str").to_lowercase();
            let op = op.trim();
            if let Some(c) = mem_ptr_re().captures(op) {
                let size = c.get(1).unwrap().as_str();
                let seg_prefix = c.get(2).map(|m| m.as_str());
                let expr = c.get(3).unwrap().as_str();
                let seg = if let Some(sp) = seg_prefix {
                    sp.to_lowercase()
                } else {
                    insn.get("detail")
                        .and_then(|d| d.get("mem_refs"))
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|m| m.get("segment"))
                        .and_then(Value::as_str)
                        .map(|s| s.to_lowercase())
                        .unwrap_or_else(|| "ds".into())
                };
                let width = if size.to_lowercase() == "byte" { 8 } else { 16 };
                let mut ni = insn.clone();
                set_str(&mut ni, "op", "INDIRECT_NEAR_JMP");
                set_str(&mut ni, "seg", &seg);
                set_str(&mut ni, "ea", expr);
                ni.insert("width".into(), Value::from(width));
                out.push(ni);
                continue;
            }
            if JMP_REGS.contains(&op) {
                let mut ni = insn.clone();
                set_str(&mut ni, "op", "INDIRECT_NEAR_JMP");
                set_str(&mut ni, "reg", op);
                out.push(ni);
                continue;
            }
        }
        out.push(insn.clone());
    }
    out
}

/// cfg.build_cfg successors (only what _render_block_state_machine needs), in
/// build_cfg edge-insertion order (fallthrough before target for jcc).
pub fn cfg_successors(blocks: &BTreeMap<i64, BasicBlock>) -> HashMap<i64, Vec<i64>> {
    let mut succ: HashMap<i64, Vec<i64>> = HashMap::new();
    for addr in blocks.keys() {
        succ.entry(*addr).or_default();
    }
    let push = |succ: &mut HashMap<i64, Vec<i64>>, a: i64, b: i64| {
        let v = succ.entry(a).or_default();
        if !v.contains(&b) {
            v.push(b);
        }
    };
    for (addr, block) in blocks {
        let last = match block.instructions.last() {
            Some(l) => l,
            None => continue,
        };
        let mnem = s(last, "mnemonic");
        let size = isize_bytes(last);
        let cur_end = i64f(last, "address").unwrap_or(0) + size;
        if mnem.starts_with('j') || mnem.starts_with("loop") || mnem == "ljmp" {
            let target = parse_imm(s(last, "op_str"));
            if mnem != "jmp" && mnem != "ljmp" && blocks.contains_key(&cur_end) {
                push(&mut succ, *addr, cur_end);
            }
            if let Some(t) = target {
                if blocks.contains_key(&t) {
                    push(&mut succ, *addr, t);
                }
            }
        } else {
            let mut is_exit = matches!(mnem, "ret" | "retn" | "retf" | "hlt" | "iret");
            if mnem == "int" {
                let op = s(last, "op_str");
                if op == "0x20" {
                    is_exit = true;
                } else if op == "0x21" {
                    if block.instructions.len() >= 2 {
                        let prev = &block.instructions[block.instructions.len() - 2];
                        if s(prev, "mnemonic") == "mov"
                            && matches!(s(prev, "op_str"), "ax, 0x4c00" | "ah, 0x4c")
                        {
                            is_exit = true;
                        }
                    }
                }
            }
            if !is_exit && blocks.contains_key(&cur_end) {
                push(&mut succ, *addr, cur_end);
            }
        }
    }
    succ
}

// ===================== operand-rewriting context =====================

/// Per-function operand-rewriting state the chunk emitter drives:
/// `rewrite_operands` (mem operands → memb/memw/var_N/RCB/exec_params forms)
/// and `match_rcb_access`. The name survives from the retired C renderer,
/// whose operand front-half this was.
pub struct Renderer {
    pub prefix: String,
    pub current_func_name: String,
    pub stack_var_sizes: HashMap<(String, String), i64>,
    pub reloc_offsets: BTreeSet<i64>,
    pub load_segment: i64,
}

impl Renderer {
    pub fn for_test(prefix: &str) -> Renderer {
        Renderer {
            prefix: prefix.to_string(),
            current_func_name: String::new(),
            stack_var_sizes: HashMap::new(),
            reloc_offsets: BTreeSet::new(),
            load_segment: 0x1010,
        }
    }
    /// parse_imm with per-renderer state (kept as a method for call-site symmetry).
    pub(crate) fn parse_imm(&self, value: &str) -> Option<i64> {
        parse_imm(value)
    }

    fn stack_var_width(
        &mut self,
        var_name: &str,
        size_prefix: Option<&str>,
        operands: &[String],
        index: usize,
    ) -> i64 {
        let key = (self.current_func_name.clone(), var_name.to_uppercase());
        let width = match size_prefix {
            Some("byte") => 8,
            Some("word") => 16,
            _ => match self.stack_var_sizes.get(&key) {
                Some(w) => *w,
                None => infer_stack_var_width(operands, index),
            },
        };
        self.stack_var_sizes.insert(key, width);
        width
    }

    /// _rewrite_operands.
    pub(crate) fn rewrite_operands(&mut self, insn: &Insn, operands: &[String]) -> Vec<String> {
        let mem_refs: Vec<Value> = insn
            .get("detail")
            .and_then(|d| d.get("mem_refs"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut mem_idx = 0usize;
        let mut result = Vec::new();
        for (index, original) in operands.iter().enumerate() {
            let simple = simple_ptr_re().captures(original);
            let (op, size_prefix): (String, Option<String>) = match &simple {
                Some(c) => (
                    c.get(2).unwrap().as_str().to_string(),
                    Some(c.get(1).unwrap().as_str().to_lowercase()),
                ),
                None => (original.clone(), None),
            };
            let op_l = op.to_lowercase();
            if let Some(m) = var_re().captures(&op_l) {
                let mem_ref = mem_refs.get(mem_idx);
                mem_idx += 1;
                let (seg, disp) = match mem_ref {
                    Some(mr) => (
                        mr.get("segment")
                            .and_then(Value::as_str)
                            .unwrap_or("ss")
                            .to_lowercase(),
                        mr.get("disp").and_then(Value::as_i64).unwrap_or(0),
                    ),
                    None => (
                        "ss".to_string(),
                        -i64::from_str_radix(m.get(1).unwrap().as_str(), 16).unwrap_or(0),
                    ),
                };
                let addr_expr = format!("((bp + 0x{:04X}) & 0xFFFF)", disp & 0xFFFF);
                let var_name = m.get(0).unwrap().as_str().to_string();
                let width =
                    self.stack_var_width(&var_name, size_prefix.as_deref(), operands, index);
                let macro_ = if width == 8 { "memb" } else { "memw" };
                result.push(format!("{macro_}({seg}, {addr_expr})"));
                continue;
            }
            if mem_ptr_re().is_match(&original.to_lowercase()) {
                let mem_ref = mem_refs.get(mem_idx);
                mem_idx += 1;
                let seg = mem_ref
                    .and_then(|mr| mr.get("segment").and_then(Value::as_str))
                    .map(|s| s.to_string());
                result.push(rewrite_mem_op(original, seg.as_deref()));
                continue;
            }
            result.push(op);
        }
        result
    }

    /// _match_rcb_access -> (size, field) for RCB es accesses.
    pub(crate) fn match_rcb_access(&self, op: &str) -> Option<(String, String)> {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            Regex::new(r"(?i)^mem([bw])\(es, (0x[0-9a-f]+|[a-z0-9_]+)\)$").unwrap()
        });
        let c = re.captures(op)?;
        let size = c.get(1).unwrap().as_str().to_lowercase();
        let field = c.get(2).unwrap().as_str().to_string();
        let field_upper = field.to_uppercase();
        let (aliases, names) = rcb_aliases();
        if names.contains(&field_upper) {
            return Some((size, field_upper));
        }
        let off = if field.to_lowercase().starts_with("0x") {
            i64::from_str_radix(&field[2..], 16).ok()?
        } else {
            field.parse::<i64>().ok()?
        };
        aliases.get(&off).map(|a| (size, a.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RCB_FIELDS is the translator's copy of the RCB layout; the chunk
    /// prelude (rt/saisei_rt.rs) carries the same fields as `pub const`s the
    /// generated rcb_read/write calls name. Keep the two in lockstep.
    #[test]
    fn rcb_prelude_consts_match() {
        let prelude = include_str!("../rt/saisei_rt.rs");
        for &(off, name) in RCB_FIELDS {
            let decl = format!("pub const {name}: c_int = 0x{off:X};");
            assert!(
                prelude.contains(&decl),
                "rt/saisei_rt.rs is missing `{decl}` (RCB_FIELDS and the prelude diverged)"
            );
        }
    }

    /// Every RCB field has a size class in rcb_macro, and rcb_macro names no
    /// offset outside the table.
    #[test]
    fn rcb_macro_covers_table_exactly() {
        for &(off, name) in RCB_FIELDS {
            assert!(
                rcb_macro(off).is_some(),
                "{name} (0x{off:X}) has no size class"
            );
        }
        for off in 0xFF00..=0xFFFF {
            if rcb_macro(off).is_some() {
                assert!(
                    RCB_FIELDS.iter().any(|&(o, _)| o == off),
                    "rcb_macro knows 0x{off:X} but RCB_FIELDS does not"
                );
            }
        }
    }
}
