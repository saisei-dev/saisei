//! program IR -> C (JIT `PCSwitchRenderer` path
//! only; the base structured renderer + patterns/ are dead in the pipeline).
//!
//! Faithfulness bar: byte-identical `.c`/`.h` vs the reference.
//! STATUS: infrastructure + state machine + dispatch ported; per-instruction
//! handlers being filled in batch-by-batch (unported ones emit a TODO placeholder
//! so the .c shape can be diffed).

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use crate::ast::AstNode;
use crate::graph::DiGraph;
use indexmap::IndexSet;
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

/// _node_returns — node ends in an unconditional return.
fn node_returns(n: &AstNode) -> bool {
    match n {
        AstNode::Return { .. } => true,
        AstNode::BasicBlock { instructions, .. } => instructions.last().map_or(false, |li| {
            matches!(s(li, "mnemonic"), "ret" | "retn" | "retf")
        }),
        AstNode::IfElse {
            then_body,
            else_body: Some(eb),
            ..
        } if !then_body.is_empty() && !eb.is_empty() => {
            node_returns(then_body.last().unwrap()) && node_returns(eb.last().unwrap())
        }
        _ => false,
    }
}

/// propagate_flag_conditions — attach the prior flag-setting
/// instruction as `cond_prev` on a block-leading Jcc with a single predecessor.
pub fn propagate_flag_conditions(blocks: &mut BTreeMap<i64, BasicBlock>, graph: &DiGraph) {
    const SETTERS: &[&str] = &[
        "cmp", "test", "add", "sub", "xor", "adc", "sbb", "shl", "shr", "sal", "sar", "rol", "ror",
        "rcl", "rcr", "or", "and", "stc", "clc", "cmc",
    ];
    const SKIP: &[&str] = &["mov", "lea", "nop", "push", "pop", "jmp"];
    let mut updates: Vec<(i64, Insn)> = Vec::new();
    for (&a, block) in blocks.iter() {
        let first = match block.instructions.first() {
            Some(f) => f,
            None => continue,
        };
        let mnem = s(first, "mnemonic");
        if !(mnem.starts_with('j') && mnem != "jmp") {
            continue;
        }
        if first.contains_key("cond_prev") {
            continue;
        }
        let preds = graph.predecessors(a);
        if preds.len() != 1 {
            continue;
        }
        let pred_block = match blocks.get(&preds[0]) {
            Some(b) if !b.instructions.is_empty() => b,
            _ => continue,
        };
        let mut prev: Option<Insn> = None;
        for cand in pred_block.instructions.iter().rev() {
            let mc = s(cand, "mnemonic");
            if SETTERS.contains(&mc) {
                prev = Some(cand.clone());
                break;
            }
            if !SKIP.contains(&mc) {
                break;
            }
        }
        if let Some(p) = prev {
            updates.push((a, p));
        }
    }
    for (a, prev) in updates {
        if let Some(b) = blocks.get_mut(&a) {
            if let Some(first) = b.instructions.first_mut() {
                first.insert("cond_prev".into(), Value::Object(prev));
            }
        }
    }
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

/// _rewrite_mem_op (subset: exec-param + RCB field maps deferred; covers the
/// common memb/memw path). TODO(port): _MEMORY_FIELD_MAP + RCB aliases.
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
fn fix_negative_imm(op: &str, other: &str) -> String {
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
/// _split_mem_accessor -> (helper, inner) for memw()/memb().
fn split_mem_accessor(expr: &str) -> Option<(&'static str, &str)> {
    if expr.starts_with("memw(") && expr.ends_with(')') {
        return Some(("memw", &expr[5..expr.len() - 1]));
    }
    if expr.starts_with("memb(") && expr.ends_with(')') {
        return Some(("memb", &expr[5..expr.len() - 1]));
    }
    None
}
/// _value_expr.
fn value_expr(dest: &str) -> String {
    match split_mem_accessor(dest) {
        Some((h, inner)) => format!("{h}({inner})"),
        None => dest.to_string(),
    }
}
/// _emit_store.
fn emit_store(lines: &mut Vec<String>, dest: &str, value: &str, indent: &str) {
    match split_mem_accessor(dest) {
        Some((h, inner)) => lines.push(format!("{indent}{h}_write({inner}, {value});")),
        None => lines.push(format!("{indent}{dest} = {value};")),
    }
}
/// the reference int(s, 0) — 0x => hex, else decimal; leading +/- ok. Base-0 rejects a
/// leading-zero decimal (len > 1), so "0109" -> None while "0" and "109" parse.
fn int0(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let v = match rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        Some(h) => i64::from_str_radix(h, 16).ok()?,
        None => {
            if rest.len() > 1 && rest.starts_with('0') && rest.bytes().any(|b| b != b'0') {
                return None;
            }
            rest.parse::<i64>().ok()?
        }
    };
    Some(if neg { -v } else { v })
}

fn opt_hex(v: Option<i64>, w: usize, reg: &str) -> String {
    match v {
        Some(x) => format!("0x{x:0w$X}"),
        None => reg.to_string(),
    }
}
/// Drop pending_dos entries whose LHS register (before '=') is in `drop`.
fn filter_pending(lines: Vec<String>, drop: &[&str]) -> Vec<String> {
    lines
        .into_iter()
        .filter(|e| {
            let key = e.splitn(2, '=').next().unwrap_or("").trim().to_lowercase();
            !drop.contains(&key.as_str())
        })
        .collect()
}
/// _DOS_API_FUNCS with dos_api_ah_XX fallback.
fn dos_api_func(ah: i64) -> String {
    let n = match ah {
        0x01 => "dos_read_char",
        0x02 => "dos_write_char",
        0x06 => "dos_direct_console_io",
        0x07 => "dos_console_input_no_echo",
        0x2A => "dos_get_date",
        0x2C => "dos_get_time",
        0x0B => "dos_check_keyboard_status",
        0x0D => "dos_reset_disk",
        0x0E => "dos_select_drive",
        0x10 => "dos_close_fcb",
        0x09 => "dos_print_string",
        0x19 => "dos_get_current_drive",
        0x1A => "dos_set_dta",
        0x25 => "dos_set_interrupt_vector",
        0x36 => "dos_get_disk_free_space",
        0x30 => "dos_get_version",
        0x35 => "dos_get_interrupt_vector",
        0x39 => "dos_make_dir",
        0x3B => "dos_change_dir",
        0x0C => "dos_flush_and_read",
        0x2F => "dos_get_dta",
        0x3C => "dos_create_file",
        0x3D => "dos_open_file",
        0x3E => "dos_close_file",
        0x4F => "dos_find_next",
        0x3F => "dos_read_file",
        0x40 => "dos_write_file",
        0x41 => "dos_delete_file",
        0x43 => "dos_get_file_attributes",
        0x44 => "dos_ioctl",
        0x47 => "dos_get_current_dir",
        0x42 => "dos_lseek",
        0x4A => "dos_resize_mem",
        0x4E => "dos_find_first",
        0x48 => "dos_alloc_mem",
        0x49 => "dos_free_mem",
        0x4B => "dos_exec",
        0x4C => "dos_exit",
        0x4D => "dos_get_return_code",
        0x56 => "dos_rename",
        _ => return format!("dos_api_ah_{ah:02X}"),
    };
    n.to_string()
}
/// _BIOS_API_FUNCS[0x10].
fn bios_api_func(ah: i64) -> Option<&'static str> {
    Some(match ah {
        0x00 => "bios_set_video_mode",
        0x02 => "bios_set_cursor_position",
        0x09 => "bios_write_char_attr",
        0x0A => "bios_write_char_only",
        0x0B => "bios_set_cga_palette",
        0x0E => "bios_teletype_output",
        0x10 => "bios_set_palette",
        0x12 => "bios_video_alt_select",
        _ => return None,
    })
}

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
/// _RCB_FIELD_ALIASES / _RCB_FIELD_NAMES from runtime/include/rcb_fields.h.
fn rcb_aliases() -> &'static (HashMap<i64, String>, std::collections::HashSet<String>) {
    static A: OnceLock<(HashMap<i64, String>, std::collections::HashSet<String>)> = OnceLock::new();
    A.get_or_init(|| {
        let text = include_str!("../../runtime/include/rcb_fields.h");
        let re = Regex::new(r"(\w+)\s*=\s*(0x[0-9A-Fa-f]+)").unwrap();
        let mut map = HashMap::new();
        let mut names = std::collections::HashSet::new();
        for c in re.captures_iter(text) {
            let name = c.get(1).unwrap().as_str().to_string();
            let off = i64::from_str_radix(&c.get(2).unwrap().as_str()[2..], 16).unwrap_or(0);
            names.insert(name.clone());
            map.insert(off, name);
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

pub(crate) fn negate_condition(cond: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(.*)\s(==|!=|<=|>=|<|>)\s(.*)$").unwrap());
    if let Some(c) = re.captures(cond) {
        let left = c.get(1).unwrap().as_str();
        let op = c.get(2).unwrap().as_str();
        let right = c.get(3).unwrap().as_str();
        let inv = match op {
            "==" => "!=",
            "!=" => "==",
            "<" => ">=",
            ">" => "<=",
            "<=" => ">",
            ">=" => "<",
            _ => op,
        };
        return format!("{left} {inv} {right}");
    }
    format!("!({cond})")
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

// ===================== renderer =====================

const NORETURN_FUNCS: &[&str] = &[
    "dos_exit",
    "retf",
    "retf_pop",
    "long_jump",
    "iret",
    "lcall_table",
    "call_table",
    "jump_table",
];

pub struct Renderer {
    pub prefix: String,
    pub extern_labels: BTreeSet<i64>,
    pub func_names: BTreeMap<i64, String>,
    // Address -> comment (JSON string or {text, multiline}); base renderer only.
    pub comments: BTreeMap<i64, Value>,
    pub cs_base: i64,
    pub load_base: i64,
    pub program_addrs: BTreeSet<i64>,
    pub pending_dos: Vec<String>,
    pub dispatch_cases: Vec<String>,
    pub seen_cases: BTreeSet<i64>,
    // Literal-register trackers for DOS/BIOS interrupt-arg reconstruction.
    pub last_ah: Option<i64>,
    pub last_al: Option<i64>,
    pub last_dx: Option<i64>,
    pub last_bx: Option<i64>,
    pub last_dl: Option<i64>,
    pub last_cx: Option<i64>,
    pub last_cl: Option<i64>,
    pub last_ch: Option<i64>,
    pub current_func_name: String,
    pub stack_var_sizes: HashMap<(String, String), i64>,
    pub reloc_offsets: BTreeSet<i64>,
    pub load_segment: i64,
    // Structured-renderer state (empty/unused on the JIT PCSwitch path).
    pub code_bytes: Vec<u8>,
    pub current_func_all_addrs: BTreeSet<i64>,
    pub current_func_addrs: BTreeSet<i64>,
    pub current_blocks: BTreeMap<i64, BasicBlock>,
    pub loop_headers: BTreeSet<i64>,
    // Entry points lifted into separate helper functions (base renderer only).
    pub extra_func_blocks: BTreeMap<i64, Vec<String>>,
}

impl Renderer {
    pub fn dispatch_name(&self) -> String {
        format!("{}dispatch", self.prefix)
    }
    pub fn func_name(&self, addr: i64) -> String {
        if let Some(n) = self.func_names.get(&addr) {
            return format!("{}{}", self.prefix, n);
        }
        if self.extern_labels.contains(&addr) {
            return format!("{}label_{:04X}", self.prefix, addr);
        }
        format!("{}func_{:04X}", self.prefix, addr)
    }
    /// CCodeRenderer.comment_lines. JIT path has an empty
    /// `comments` map (no metadata) so this returns []; the base renderer's
    /// metadata tests populate it.
    pub(crate) fn comment_lines(&self, addr: i64) -> Vec<String> {
        let comment = match self.comments.get(&addr) {
            Some(c) => c,
            None => return Vec::new(),
        };
        // the reference str.splitlines(): split on '\n', drop a single trailing empty.
        fn splitlines(t: &str) -> Vec<&str> {
            if t.is_empty() {
                return Vec::new();
            }
            let mut v: Vec<&str> = t.split('\n').collect();
            if t.ends_with('\n') {
                v.pop();
            }
            v
        }
        match comment {
            Value::String(text) => splitlines(text)
                .into_iter()
                .map(|l| format!("// {l}"))
                .collect(),
            Value::Object(o) => {
                let text = o.get("text").and_then(Value::as_str).unwrap_or("");
                let lines = splitlines(text);
                if o.get("multiline").map_or(false, |m| !is_falsy(m)) {
                    let mut out = vec!["/*".to_string()];
                    out.extend(lines.iter().map(|l| l.to_string()));
                    out.push("*/".to_string());
                    out
                } else {
                    lines.into_iter().map(|l| format!("// {l}")).collect()
                }
            }
            _ => Vec::new(),
        }
    }

    /// Test constructor mirroring CCodeRenderer.__init__ kwargs (all-default);
    /// callers set public fields (code_bytes, extern_labels, program_addrs,
    /// comments, reloc_offsets, cs_base, load_segment, load_base) as needed.
    pub fn for_test(prefix: &str) -> Renderer {
        let load_segment: i64 = 0x1010;
        Renderer {
            prefix: prefix.to_string(),
            extern_labels: BTreeSet::new(),
            func_names: BTreeMap::new(),
            comments: BTreeMap::new(),
            cs_base: 0,
            load_base: load_segment << 4,
            program_addrs: BTreeSet::new(),
            pending_dos: Vec::new(),
            dispatch_cases: Vec::new(),
            seen_cases: BTreeSet::new(),
            last_ah: None,
            last_al: None,
            last_dx: None,
            last_bx: None,
            last_dl: None,
            last_cx: None,
            last_cl: None,
            last_ch: None,
            current_func_name: String::new(),
            code_bytes: Vec::new(),
            current_func_all_addrs: BTreeSet::new(),
            current_func_addrs: BTreeSet::new(),
            current_blocks: BTreeMap::new(),
            loop_headers: BTreeSet::new(),
            extra_func_blocks: BTreeMap::new(),
            stack_var_sizes: HashMap::new(),
            reloc_offsets: BTreeSet::new(),
            load_segment,
        }
    }
    /// CCodeRenderer.parse_imm (== ir_to_c._parse_imm).
    pub(crate) fn parse_imm(&self, value: &str) -> Option<i64> {
        parse_imm(value)
    }

    /// CCodeRenderer.structure — iterative region structuring.
    pub fn structure(
        &mut self,
        blocks: &BTreeMap<i64, BasicBlock>,
        graph: &DiGraph,
        known: &mut BTreeSet<i64>,
        entry: i64,
        loop_exits: &IndexSet<i64>,
    ) -> Vec<AstNode> {
        let graph = crate::cfg::merge_shared_tails(blocks, graph);
        let mut pending_blocks: BTreeMap<i64, BasicBlock> = (*blocks).clone();
        let mut pending_graph = graph.clone();
        let mut nodes_with_addr: Vec<(i64, AstNode)> = Vec::new();
        let order = crate::cfg::traversal_order(&graph, entry);
        let ipdom_all = crate::cfg::compute_immediate_postdominators(&graph);
        let mut protected: BTreeSet<i64> = BTreeSet::new();
        for addr in pending_graph.nodes() {
            if pending_graph.in_degree(addr) > 1 {
                protected.insert(addr);
            }
        }
        for &v in ipdom_all.values() {
            protected.insert(v);
        }
        let mut processed: BTreeSet<i64> = BTreeSet::new();

        while !pending_blocks.is_empty() {
            let start_size = pending_blocks.len();
            let loop_map = crate::cfg::find_loops(&pending_blocks, &pending_graph);
            let ipost = crate::cfg::compute_immediate_postdominators(&pending_graph);
            let mut progress = false;

            for &start in &order {
                if !pending_blocks.contains_key(&start) || processed.contains(&start) {
                    continue;
                }
                let mut hit: Option<(
                    AstNode,
                    IndexSet<i64>,
                    bool, /*is_loop_hdr*/
                    bool, /*comment*/
                )> = None;
                if let Some((node, consumed)) = crate::patterns::detect_for_loop(
                    self,
                    start,
                    &pending_blocks,
                    &pending_graph,
                    &loop_map,
                    &ipost,
                    loop_exits,
                    known,
                ) {
                    hit = Some((node, consumed, false, false));
                } else if let Some((node, consumed)) = crate::patterns::detect_loop(
                    self,
                    start,
                    &pending_blocks,
                    &pending_graph,
                    &loop_map,
                    &ipost,
                    loop_exits,
                    known,
                ) {
                    hit = Some((node, consumed, true, true));
                } else if let Some((node, consumed)) = crate::patterns::detect_switch(
                    self,
                    start,
                    &pending_blocks,
                    &pending_graph,
                    &loop_map,
                    &ipost,
                    loop_exits,
                    known,
                ) {
                    hit = Some((node, consumed, false, true));
                } else if let Some((node, consumed)) = crate::patterns::detect_if_else(
                    self,
                    start,
                    &pending_blocks,
                    &pending_graph,
                    &loop_map,
                    &ipost,
                    loop_exits,
                    known,
                ) {
                    hit = Some((node, consumed, false, true));
                }
                if let Some((node, consumed, is_hdr, want_comment)) = hit {
                    if want_comment && !self.comment_lines(start).is_empty() {
                        nodes_with_addr.push((start, AstNode::Comment { start }));
                    }
                    if is_hdr {
                        if let AstNode::Loop { header, .. } | AstNode::DoWhile { header, .. } =
                            &node
                        {
                            self.loop_headers.insert(*header);
                        }
                    }
                    nodes_with_addr.push((start, node));
                    for b in &consumed {
                        processed.insert(*b);
                        if !protected.contains(b) {
                            pending_blocks.remove(b);
                            pending_graph.remove_node(*b);
                        }
                    }
                    progress = true;
                    break;
                }
            }

            if !progress {
                let mut emitted = false;
                for &start in &order {
                    if pending_blocks.contains_key(&start) && !processed.contains(&start) {
                        let insts = pending_blocks[&start].instructions.clone();
                        let node = if insts.len() == 1 {
                            let mnem = s(&insts[0], "mnemonic");
                            match mnem {
                                "ret" | "retn" | "retf" => AstNode::Return {
                                    start: Some(start),
                                    mnemonic: mnem.into(),
                                    pop_bytes: parse_imm(s(&insts[0], "op_str")),
                                },
                                "jmp" => AstNode::Goto {
                                    start,
                                    insn: insts[0].clone(),
                                },
                                "call" => AstNode::Call {
                                    start,
                                    insn: insts[0].clone(),
                                },
                                _ => AstNode::BasicBlock {
                                    start,
                                    instructions: insts.clone(),
                                },
                            }
                        } else {
                            AstNode::BasicBlock {
                                start,
                                instructions: insts.clone(),
                            }
                        };
                        if !self.comment_lines(start).is_empty() {
                            nodes_with_addr.push((start, AstNode::Comment { start }));
                        }
                        nodes_with_addr.push((start, node));
                        processed.insert(start);
                        if !protected.contains(&start) {
                            pending_blocks.remove(&start);
                            pending_graph.remove_node(start);
                        }
                        emitted = true;
                        break;
                    }
                }
                if !emitted {
                    break;
                }
            }

            if pending_blocks.len() == start_size
                && !progress
                && pending_blocks.keys().all(|b| processed.contains(b))
            {
                break;
            }
        }

        let order_index: HashMap<i64, usize> =
            order.iter().enumerate().map(|(i, &a)| (a, i)).collect();
        let fallback = order_index.len();
        nodes_with_addr.sort_by_key(|(a, _)| *order_index.get(a).unwrap_or(&fallback));
        let nodes = self.extract_shared_postdom_blocks(nodes_with_addr, &pending_graph);
        nodes.into_iter().map(|(_a, n)| n).collect()
    }

    /// _extract_shared_postdom_blocks.
    pub fn extract_shared_postdom_blocks(
        &self,
        nodes_with_addr: Vec<(i64, AstNode)>,
        graph: &DiGraph,
    ) -> Vec<(i64, AstNode)> {
        let ipdom = crate::cfg::compute_immediate_postdominators(graph);
        let mut counts: HashMap<i64, usize> = HashMap::new();
        for &v in ipdom.values() {
            *counts.entry(v).or_insert(0) += 1;
        }
        let mut shared: BTreeSet<i64> = counts
            .iter()
            .filter(|(_, &c)| c > 1)
            .map(|(&a, _)| a)
            .collect();
        for n in graph.nodes() {
            if graph.in_degree(n) > 1 {
                shared.insert(n);
            }
        }
        if shared.is_empty() {
            return nodes_with_addr;
        }
        let mut seen: BTreeSet<i64> = BTreeSet::new();
        let mut out = Vec::new();
        for (addr, node) in nodes_with_addr {
            if matches!(node, AstNode::Comment { .. }) {
                if seen.contains(&addr) {
                    continue;
                }
                out.push((addr, node));
                continue;
            }
            if matches!(
                node,
                AstNode::BasicBlock { .. }
                    | AstNode::Goto { .. }
                    | AstNode::Call { .. }
                    | AstNode::Return { .. }
            ) && shared.contains(&addr)
            {
                if seen.contains(&addr) {
                    continue;
                }
                seen.insert(addr);
            }
            out.push((addr, node));
        }
        out
    }

    pub(crate) fn flush_pending_dos(&mut self, lines: &mut Vec<String>, indent: &str) {
        if !self.pending_dos.is_empty() {
            for e in &self.pending_dos {
                lines.push(format!("{indent}{e}"));
            }
            self.pending_dos.clear();
            self.last_ah = None;
            self.last_al = None;
            self.last_dx = None;
            self.last_bx = None;
            self.last_dl = None;
            self.last_cx = None;
            self.last_cl = None;
            self.last_ch = None;
        }
    }

    /// _invalidate_register.
    fn invalidate_register(&mut self, dest: &str) {
        match dest.to_lowercase().as_str() {
            "cx" | "cl" | "ch" => {
                self.last_cx = None;
                self.last_cl = None;
                self.last_ch = None;
            }
            "dx" => {
                self.last_dx = None;
                self.last_dl = None;
            }
            "dh" => self.last_dx = None,
            "dl" => {
                self.last_dx = None;
                self.last_dl = None;
            }
            "bx" | "bl" | "bh" => self.last_bx = None,
            "ax" => {
                self.last_ah = None;
                self.last_al = None;
            }
            "ah" => self.last_ah = None,
            "al" => self.last_al = None,
            _ => {}
        }
    }

    fn emit_with_comment(&self, lines: &mut Vec<String>, insn: &Insn, code: &str, indent: &str) {
        let asm = format!("{} {}", s(insn, "mnemonic"), s(insn, "op_str"))
            .trim()
            .to_string();
        lines.push(format!("{indent}// ASM: {asm}"));
        lines.push(format!("{indent}{code}"));
    }

    fn emit_unsupported_abort(&self, insn: &Insn, asm: &str) -> ! {
        let addr = i64f(insn, "address").unwrap_or(0);
        let module = self.prefix.trim_end_matches('_');
        let module = if module.is_empty() { "?" } else { module };
        eprintln!(
            "\n[ir_to_c FATAL] unsupported instruction blocked translation\n  module:       {module}\n  file_off:     0x{addr:X}\n  mnemonic:     {asm}\n"
        );
        std::process::exit(2)
    }

    fn terminates(&self, line: &str) -> bool {
        let line = line.trim();
        if line.starts_with("return") || line.starts_with("goto") {
            return true;
        }
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"^([a-zA-Z_][a-zA-Z0-9_]*)\(").unwrap());
        if let Some(c) = re.captures(line) {
            if NORETURN_FUNCS.contains(&c.get(1).unwrap().as_str()) {
                return true;
            }
        }
        false
    }

    fn fallthrough_fileoff(&self, insn: &Insn) -> Option<i64> {
        if let Some(n) = i64f(insn, "_next_addr") {
            return Some(n);
        }
        let size = isize_bytes(insn);
        if size <= 0 {
            return None;
        }
        Some(i64f(insn, "address")? + size)
    }
    fn fallthrough_ip(&self, insn: &Insn) -> Option<i64> {
        self.fallthrough_fileoff(insn).map(|a| a & 0xFFFF)
    }

    fn call_return_arg(&self, insn: &Insn) -> String {
        if self.cs_base != 0 {
            match self.fallthrough_ip(insn) {
                None => "ip".into(),
                Some(ip) => format!("0x{:04X}", (ip + self.cs_base) & 0xFFFF),
            }
        } else {
            match self.fallthrough_fileoff(insn) {
                None => "ip".into(),
                Some(off) => format!(
                    "(uint16_t)(0x{off:05X}U + 0x{:05X}U - ((uint32_t)cs << 4))",
                    self.load_base
                ),
            }
        }
    }

    /// _indirect_jump_target — returns (c_type, expr). Applies
    /// the same rewrite_mem_op + RCB-field rewrite the reference does.
    fn indirect_jump_target(&self, insn: &Insn) -> (String, String) {
        if let Some(reg) = insn.get("reg").and_then(Value::as_str) {
            return ("uint16_t".to_string(), reg.to_string());
        }
        let seg = insn.get("seg").and_then(Value::as_str).unwrap_or("ds");
        let ea = insn.get("ea").and_then(Value::as_str).unwrap_or("0");
        let width = i64f(insn, "width").unwrap_or(16);
        let size_tok = if width == 8 { "byte" } else { "word" };
        let mem_expr = rewrite_mem_op(&format!("{size_tok} ptr [{ea}]"), Some(seg));
        let c_type = if width == 8 { "uint8_t" } else { "uint16_t" };
        let expr = match self.match_rcb_access(&mem_expr) {
            Some((size, field)) => {
                let func = if size == "w" {
                    "rcb_read16"
                } else {
                    "rcb_read8"
                };
                format!("{func}({field})")
            }
            None => mem_expr,
        };
        (c_type.to_string(), expr)
    }

    /// format_instruction — dispatch to per-mnemonic handlers.
    pub(crate) fn format_instruction(
        &mut self,
        insn: &Insn,
        known_funcs: &BTreeSet<i64>,
    ) -> Vec<String> {
        let mnem = s(insn, "mnemonic").to_string();
        if let Some(r) = self.dispatch_handler(&mnem, insn, known_funcs) {
            return r;
        }
        if mnem.contains(' ') {
            let (prefix, base) = mnem.split_once(' ').unwrap();
            let mut prefixed = insn.clone();
            set_str(&mut prefixed, "mnemonic", prefix);
            set_str(
                &mut prefixed,
                "op_str",
                format!("{base} {}", s(insn, "op_str")).trim(),
            );
            let mut unknown_prefix = false;
            if let Some(r) = self.dispatch_handler(prefix, &prefixed, known_funcs) {
                return r;
            } else if !self.is_known_handler(prefix) {
                unknown_prefix = true;
            }
            let mut base_insn = insn.clone();
            set_str(&mut base_insn, "mnemonic", base);
            if let Some(r) = self.dispatch_handler(base, &base_insn, known_funcs) {
                if unknown_prefix {
                    let op_str = decode_variables(s(insn, "op_str"));
                    let asm = format!("{mnem} {op_str}").trim().to_string();
                    self.emit_unsupported_abort(insn, &asm);
                }
                return r;
            }
        }
        if mnem.starts_with('j') && mnem != "jmp" {
            if let Some(r) = self.handle_jcc(insn, known_funcs) {
                return r;
            }
        }
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let asm = format!("{mnem} {op_str}").trim().to_string();
        self.emit_unsupported_abort(insn, &asm);
    }

    // ---- handler dispatch (mnemonic -> ported handler; TODO: fill in) ----
    fn is_known_handler(&self, mnem: &str) -> bool {
        HANDLED.contains(&mnem)
    }

    /// Returns Some(lines) if a handler exists and produced output, None to fall
    /// through (mirrors the reference handler returning None / no handler).
    fn dispatch_handler(
        &mut self,
        mnem: &str,
        insn: &Insn,
        known_funcs: &BTreeSet<i64>,
    ) -> Option<Vec<String>> {
        let _ = known_funcs;
        match mnem {
            "nop" | "db" => Some(self.handle_noop()),
            "cwde" => Some(self.simple_flush(&["ax = ((int8_t)al) & 0xFFFF;"])),
            "cdq" => {
                self.invalidate_register("dx");
                Some(self.simple_flush(&["dx = (ax & 0x8000) ? 0xFFFF : 0;"]))
            }
            "cld" => Some(self.simple_flush(&["DF = 0;"])),
            "std" => Some(self.simple_flush(&["DF = 1;"])),
            "clc" => Some(self.simple_flush(&["CF = 0;"])),
            "cmc" => Some(self.simple_flush(&["CF ^= 1;"])),
            "stc" => Some(self.simple_flush(&["CF = 1;"])),
            "cli" => Some(self.simple_flush(&["IF = 0;"])),
            "sti" => Some(self.simple_flush(&["IF = 1;", "interrupt_shadow = 1;"])),
            "lodsb" => Some(self.h_lods(insn, 1, "al", "memb")),
            "lodsw" => Some(self.h_lods(insn, 2, "ax", "memw")),
            "xlatb" => Some(self.h_xlatb(insn)),
            "movsb" => Some(self.h_movs(insn, 1, "memb")),
            "movsw" => Some(self.h_movs(insn, 2, "memw")),
            "cmpsb" => Some(self.h_cmpsb(insn)),
            "scasb" => Some(self.h_scasb()),
            "scasw" => Some(self.h_scasw()),
            "stosb" => Some(self.h_stos(1, "al", "memb")),
            "stosw" => Some(self.h_stos(2, "ax", "memw")),
            "rep" => Some(self.h_rep(insn)),
            "repe" => Some(self.h_repe_cmpsb(insn)),
            "repne" => Some(self.h_repne_scasb(insn)),
            "push" => Some(self.handle_push(insn)),
            "pop" => Some(self.handle_pop(insn)),
            "ret" | "retn" | "retf" => Some(self.handle_ret(insn)),
            "cmp" => Some(self.handle_cmp(insn)),
            "test" => Some(self.handle_test(insn)),
            "call" => Some(self.handle_call(insn, known_funcs)),
            "mov" => Some(self.handle_mov(insn)),
            "add" | "adc" | "sub" | "sbb" | "and" | "or" | "xor" | "inc" | "dec" => {
                self.handle_arithmetic(insn)
            }
            "in" | "out" => Some(self.handle_io(insn)),
            "pushf" => Some(self.handle_pushf()),
            "popf" => Some(self.handle_popf()),
            "pushaw" => Some(self.handle_pushaw()),
            "popaw" => Some(self.handle_popaw()),
            "leave" => Some(self.handle_leave()),
            "enter" => Some(self.handle_enter(insn)),
            "xchg" => Some(self.handle_xchg(insn)),
            "lds" => Some(self.handle_load_far(insn, "ds")),
            "les" => Some(self.handle_load_far(insn, "es")),
            "mul" => Some(self.handle_mul(insn)),
            "imul" => Some(self.handle_imul(insn)),
            "div" => Some(self.handle_div(insn)),
            "idiv" => Some(self.handle_idiv(insn)),
            "not" => Some(self.handle_not(insn)),
            "neg" => Some(self.handle_neg(insn)),
            "shl" | "shr" | "sar" | "sal" => Some(self.handle_shift(insn)),
            "rol" => Some(self.handle_rotate(insn)),
            "rcl" => Some(self.handle_rcl(insn)),
            "lcall" => Some(self.handle_lcall(insn)),
            "ljmp" => Some(self.handle_ljmp(insn)),
            "iret" => Some(self.handle_iret()),
            "loop" | "loopne" | "loopnz" | "loope" | "loopz" => Some(self.handle_loop(insn)),
            "int" => Some(self.handle_interrupt(insn)),
            "lea" => Some(self.handle_lea(insn)),
            "ror" => Some(self.handle_ror(insn)),
            "rcr" => Some(self.handle_rcr(insn)),
            "aaa" => Some(self.handle_aaa_aas(true)),
            "aas" => Some(self.handle_aaa_aas(false)),
            "daa" => Some(self.handle_daa_das(true)),
            "das" => Some(self.handle_daa_das(false)),
            "lahf" => Some(self.handle_lahf()),
            "sahf" => Some(self.handle_sahf()),
            "salc" => Some(self.handle_salc()),
            "aam" => Some(self.handle_aam(insn)),
            "aad" => Some(self.handle_aad(insn)),
            "bound" => Some(self.handle_bound(insn)),
            // The JIT PCSwitch path handles jmp inline in the state machine, so it
            // never reaches here; the BASE renderer routes AstNode::Goto through
            // format_instruction -> here. Any other mnemonic returns None ->
            // format_instruction aborts faithfully.
            "jmp" => Some(self.handle_jmp(insn, known_funcs)),
            _ => None,
        }
    }

    fn handle_noop(&mut self) -> Vec<String> {
        Vec::new()
    }
    /// flush pending_dos then emit fixed line(s) — the common trivial-handler shape.
    fn simple_flush(&mut self, code: &[&str]) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        for c in code {
            lines.push((*c).to_string());
        }
        lines
    }
    /// handle_jcc — base-renderer leading Jcc lowering.
    fn handle_jcc(&mut self, insn: &Insn, known_funcs: &BTreeSet<i64>) -> Option<Vec<String>> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let target = match parse_hex(&op_str) {
            Some(t) => t,
            None => {
                let asm = format!("{} {op_str}", s(insn, "mnemonic"))
                    .trim()
                    .to_string();
                self.emit_unsupported_abort(insn, &asm);
            }
        };
        let cond = jcc_condition(
            s(insn, "mnemonic"),
            insn.get("cond_prev").and_then(|v| v.as_object()),
            i64f(insn, "address"),
        );
        self.emit_with_comment(&mut lines, insn, &format!("if ({cond}) {{"), "");
        if known_funcs.contains(&target) || self.current_func_addrs.contains(&target) {
            lines.push(format!("    pc = 0x{target:04X};"));
            lines.push("    continue;".into());
        } else {
            let name = self.func_name(target);
            lines.push(format!("    {name}();"));
            lines.push("    return;".into());
        }
        lines.push("}".into());
        Some(lines)
    }

    /// handle_jmp — base-renderer near/indirect jmp lowering.
    /// (The JIT PCSwitch path handles jmp inline in render_block_state_machine;
    /// this method drives the AstNode::Goto / block-terminal-jmp base path.)
    fn handle_jmp(&mut self, insn: &Insn, known_funcs: &BTreeSet<i64>) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let op_low = op_str.to_lowercase();
        let op = op_low.trim();
        const JMP_REGS: &[&str] = &["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"];
        if insn.get("op").and_then(Value::as_str) == Some("INDIRECT_NEAR_JMP") {
            let (_, expr) = self.indirect_jump_target(insn);
            self.emit_with_comment(
                &mut lines,
                insn,
                &format!("jump_table((((uint32_t)cs << 4) + (uint16_t)({expr})) & 0xFFFFF, expected_retip);"),
                "",
            );
            lines.push("return;".into());
            return lines;
        }
        if JMP_REGS.contains(&op) {
            self.emit_with_comment(
                &mut lines,
                insn,
                &format!("jump_table((((uint32_t)cs << 4) + (uint16_t)({op})) & 0xFFFFF, expected_retip);"),
                "",
            );
            lines.push("return;".into());
            return lines;
        }
        if mem_ptr_re().is_match(op) {
            let seg = insn
                .get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|m| m.get("segment"))
                .and_then(Value::as_str)
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "ds".into());
            let mem_expr = rewrite_mem_op(op, Some(&seg));
            let expr = match self.match_rcb_access(&mem_expr) {
                Some((size, field)) => {
                    let func = if size == "w" {
                        "rcb_read16"
                    } else {
                        "rcb_read8"
                    };
                    format!("{func}({field})")
                }
                None => mem_expr,
            };
            self.emit_with_comment(
                &mut lines,
                insn,
                &format!("jump_table((((uint32_t)cs << 4) + (uint16_t)({expr})) & 0xFFFFF, expected_retip);"),
                "",
            );
            lines.push("return;".into());
            return lines;
        }
        let target = match parse_hex(&op_str) {
            Some(t) => t,
            None => {
                let asm = format!("jmp {op_str}").trim().to_string();
                self.emit_unsupported_abort(insn, &asm);
            }
        };
        if self.loop_headers.contains(&target) && self.current_blocks.contains_key(&target) {
            lines.push("continue;".into());
            return lines;
        }
        let single_ret = self.current_blocks.get(&target).map_or(false, |b| {
            b.instructions.len() == 1
                && matches!(s(&b.instructions[0], "mnemonic"), "ret" | "retn" | "retf")
        }) && !self.func_names.contains_key(&target);
        let block_pc = self
            .current_blocks
            .get(&target)
            .map_or(false, |b| !b.instructions.is_empty())
            && self.current_func_addrs.contains(&target)
            && self.program_addrs.contains(&target);
        if known_funcs.contains(&target) {
            self.emit_with_comment(&mut lines, insn, &format!("pc = 0x{target:04X};"), "");
            lines.push("continue;".into());
            return lines;
        }
        if single_ret {
            lines.push("return;".into());
            return lines;
        }
        if block_pc {
            self.emit_with_comment(&mut lines, insn, &format!("pc = 0x{target:04X};"), "");
            lines.push("continue;".into());
            return lines;
        }
        if insn.get("force_pc").map_or(false, |v| !is_falsy(v)) {
            self.emit_with_comment(&mut lines, insn, &format!("pc = 0x{target:04X};"), "");
            lines.push("continue;".into());
            return lines;
        }
        self.emit_with_comment(&mut lines, insn, &format!("pc = 0x{target:04X};"), "");
        lines.push("continue;".into());
        lines
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
    fn rewrite_operands(&mut self, insn: &Insn, operands: &[String]) -> Vec<String> {
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
    fn match_rcb_access(&self, op: &str) -> Option<(String, String)> {
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

    fn handle_ret(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let mnem = s(insn, "mnemonic");
        let imm = parse_imm(s(insn, "op_str"));
        if mnem == "retf" {
            match imm {
                Some(v) => lines.push(format!("retf_pop({v});")),
                None => lines.push("retf();".into()),
            }
            lines.push("return;".into());
            return lines;
        }
        lines.push("{".into());
        lines.push("    uint16_t popped_ip = memw(ss, sp);".into());
        lines.push("    sp = (sp + 2) & 0xFFFF;".into());
        if let Some(v) = imm {
            lines.push(format!("    sp = (sp + {v}) & 0xFFFF;"));
        }
        if !self.prefix.is_empty() {
            if self.cs_base != 0 {
                lines.push(format!(
                    "    pc = (int)((popped_ip - 0x{:04X}) & 0xFFFF);",
                    self.cs_base
                ));
            } else {
                lines.push(format!(
                    "    pc = (int)(((uint32_t)cs << 4) + popped_ip - 0x{:05X}U);",
                    self.load_base
                ));
            }
            lines.push("    continue;".into());
        } else {
            lines.push("    near_ret_tail(popped_ip, expected_retip);".into());
            lines.push("    return;".into());
        }
        lines.push("}".into());
        lines
    }

    fn handle_push(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let ops = self.rewrite_operands(insn, &[op_str]);
        let op = &ops[0];
        if op.to_lowercase() == "sp" {
            lines.push("uint16_t push_value = sp;".into());
            lines.push("sp = (sp - 2) & 0xFFFF;".into());
            lines.push("memw_write(ss, sp, push_value);".into());
            return lines;
        }
        lines.push("sp = (sp - 2) & 0xFFFF;".into());
        lines.push(format!("memw_write(ss, sp, {op});"));
        lines
    }

    fn handle_pop(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let ops = self.rewrite_operands(insn, &[op_str]);
        let dest = ops[0].clone();
        let dest_l = dest.to_lowercase();
        match dest_l.as_str() {
            "dx" => {
                self.last_dx = None;
                self.last_dl = None;
            }
            "dl" => self.last_dl = None,
            "bx" => self.last_bx = None,
            "ax" | "ah" | "al" => {
                self.last_ah = None;
                self.last_al = None;
            }
            _ => {}
        }
        if dest_l == "sp" {
            lines.push("{".into());
            lines.push("    uint16_t tmp = memw(ss, sp);".into());
            lines.push("    sp = (sp + 2) & 0xFFFF;".into());
            lines.push("    sp = tmp;".into());
            lines.push("}".into());
            return lines;
        }
        if dest.starts_with("memw(") || dest.starts_with("memb(") {
            let inner = &dest[5..dest.len() - 1];
            let writer = if dest.starts_with("memw(") {
                "memw_write"
            } else {
                "memb_write"
            };
            lines.push(format!("{writer}({inner}, memw(ss, sp));"));
        } else {
            lines.push(format!("{dest} = memw(ss, sp);"));
        }
        if dest_l == "ss" {
            lines.push("interrupt_shadow = 1;".into());
        }
        lines.push("sp = (sp + 2) & 0xFFFF;".into());
        lines
    }

    fn handle_cmp(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            self.emit_unsupported_abort(insn, format!("{} {op_str}", s(insn, "mnemonic")).trim());
        }
        let rw = self.rewrite_operands(insn, &parts);
        let (mut left, mut right) = (rw[0].clone(), rw[1].clone());
        left = fix_negative_imm(&left, &right);
        right = fix_negative_imm(&right, &left);
        let is_byte = match self.match_rcb_access(&left) {
            Some((size, _)) => size == "b",
            None => left.starts_with("memb(") || BYTE_REGS.contains(&left.to_lowercase().as_str()),
        };
        let (ctype, sign_bit, shift) = if is_byte {
            ("uint8_t", "0x80", 7)
        } else {
            ("uint16_t", "0x8000", 15)
        };
        lines.push("{".into());
        lines.push(format!("    uint32_t left_val = {left};"));
        lines.push(format!("    uint32_t right_val = {right};"));
        lines.push("    CF = left_val < right_val;".into());
        lines.push("    uint32_t tmp = left_val - right_val;".into());
        lines.push(format!("    {ctype} result = ({ctype})tmp;"));
        lines.push("    ZF = result == 0;".into());
        lines.push(format!("    SF = (result >> {shift}) & 1;"));
        lines.push("    PF = parity8((uint8_t)result);".into());
        lines.push(format!(
            "    OF = ((left_val ^ right_val) & (left_val ^ result) & {sign_bit}) != 0;"
        ));
        lines.push("}".into());
        lines
    }

    fn handle_test(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            self.emit_unsupported_abort(insn, format!("{} {op_str}", s(insn, "mnemonic")).trim());
        }
        let rw = self.rewrite_operands(insn, &parts);
        let (mut left, mut right) = (rw[0].clone(), rw[1].clone());
        left = fix_negative_imm(&left, &right);
        right = fix_negative_imm(&right, &left);
        let is_byte =
            left.starts_with("memb(") || BYTE_REGS.contains(&left.to_lowercase().as_str());
        let (ctype, shift) = if is_byte {
            ("uint8_t", 7)
        } else {
            ("uint16_t", 15)
        };
        lines.push("{".into());
        lines.push(format!("    uint32_t left_val = {left};"));
        lines.push(format!("    uint32_t right_val = {right};"));
        lines.push(format!(
            "    {ctype} tmp = ({ctype})(left_val & right_val);"
        ));
        lines.push("    CF = 0;".into());
        lines.push("    OF = 0;".into());
        lines.push("    ZF = tmp == 0;".into());
        lines.push(format!("    SF = (tmp >> {shift}) & 1;"));
        lines.push("    PF = parity8((uint8_t)tmp);".into());
        lines.push("}".into());
        lines
    }

    /// handle_loop. current_blocks is empty on the PCSwitch
    /// path, so the three target branches collapse to the pc=target form.
    fn handle_loop(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        self.invalidate_register("cx");
        let target = parse_imm(s(insn, "op_str"));
        let mnem = s(insn, "mnemonic");
        lines.push("cx--;".into());
        let mut cond = "cx != 0".to_string();
        if matches!(mnem, "loopne" | "loopnz") {
            cond += " && ZF == 0";
        } else if matches!(mnem, "loope" | "loopz") {
            cond += " && ZF == 1";
        }
        match target {
            Some(t) => {
                lines.push(format!("if ({cond}) {{"));
                lines.push(format!("    pc = 0x{t:04X};"));
                lines.push("    continue;".into());
                lines.push("}".into());
            }
            None => {
                self.emit_unsupported_abort(insn, format!("{mnem} {}", s(insn, "op_str")).trim());
            }
        }
        lines
    }

    fn handle_lea(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            self.emit_unsupported_abort(insn, format!("lea {op_str}").trim());
        }
        let (dest, src) = (parts[0].clone(), parts[1].clone());
        self.invalidate_register(&dest);
        let src_l = src.to_lowercase();
        let mem_refs = insn
            .get("detail")
            .and_then(|d| d.get("mem_refs"))
            .and_then(Value::as_array);
        if let Some(m) = var_re().captures(&src_l) {
            let disp = match mem_refs.and_then(|a| a.first()) {
                Some(mr) => mr.get("disp").and_then(Value::as_i64).unwrap_or(0),
                None => -i64::from_str_radix(m.get(1).unwrap().as_str(), 16).unwrap_or(0),
            };
            lines.push(format!(
                "{dest} = ((bp + 0x{:04X}) & 0xFFFF);",
                disp & 0xFFFF
            ));
            return lines;
        }
        if src.starts_with('[') && src.ends_with(']') {
            let inner = src[1..src.len() - 1].trim();
            lines.push(format!("{dest} = ({inner}) & 0xFFFF;"));
            return lines;
        }
        let value = if src_l.starts_with("0x") || src_l.starts_with("-0x") {
            parse_hex16(&src)
        } else {
            src.parse::<i64>().ok()
        };
        match value {
            Some(v) => {
                lines.push(format!("{dest} = 0x{:04X};", v & 0xFFFF));
                lines
            }
            None => self.emit_unsupported_abort(insn, format!("lea {}", s(insn, "op_str")).trim()),
        }
    }

    fn handle_ror(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            self.emit_unsupported_abort(insn, format!("{} {op_str}", s(insn, "mnemonic")).trim());
        }
        let rw = self.rewrite_operands(insn, &parts);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        let reg = dest.to_lowercase();
        self.invalidate_register(&dest);
        let width: i64 = if dest.starts_with("memb(") || BYTE_REGS.contains(&reg.as_str()) {
            8
        } else {
            16
        };
        let is_mem = dest.starts_with("memw(") || dest.starts_with("memb(");
        lines.push("{".into());
        lines.push(format!("    unsigned count = {src} & {};", width - 1));
        lines.push("    if (count) {".into());
        if is_mem {
            let inner = &dest[5..dest.len() - 1];
            let acc = if dest.starts_with("memw(") {
                "memw"
            } else {
                "memb"
            };
            lines.push(format!(
                "        {acc}_write({inner}, ({acc}({inner}) >> count) | ({acc}({inner}) << ({width} - count)));"
            ));
            lines.push(format!("        CF = {acc}({inner}) >> {};", width - 1));
            lines.push("        if (count == 1) {".into());
            lines.push(format!(
                "            OF = (({acc}({inner}) >> {}) & 1) ^ (({acc}({inner}) >> {}) & 1);",
                width - 1,
                width - 2
            ));
            lines.push("        } else {".into());
            lines.push("            OF = 0;".into());
            lines.push("        }".into());
        } else {
            lines.push(format!(
                "        {dest} = ({dest} >> count) | ({dest} << ({width} - count));"
            ));
            lines.push(format!("        CF = {dest} >> {};", width - 1));
            lines.push("        if (count == 1) {".into());
            lines.push(format!(
                "            OF = (({dest} >> {}) & 1) ^ (({dest} >> {}) & 1);",
                width - 1,
                width - 2
            ));
            lines.push("        } else {".into());
            lines.push("            OF = 0;".into());
            lines.push("        }".into());
        }
        lines.push("    }".into());
        lines.push("}".into());
        lines
    }

    fn handle_rcr(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        let rw = self.rewrite_operands(insn, &[parts[0].clone()]);
        let dest = rw[0].clone();
        let count = if parts.len() == 2 {
            parts[1].clone()
        } else {
            "1".into()
        };
        self.invalidate_register(&dest);
        let is_mem = dest.starts_with("memw(") || dest.starts_with("memb(");
        let is_byte =
            dest.starts_with("memb(") || BYTE_REGS.contains(&dest.to_lowercase().as_str());
        let width = if is_byte { 8 } else { 16 };
        let mask = if is_byte { "0xFF" } else { "0xFFFF" };
        if is_mem {
            let inner = &dest[5..dest.len() - 1];
            let acc = if dest.starts_with("memb(") {
                "memb"
            } else {
                "memw"
            };
            let ctype = if acc == "memb" { "uint8_t" } else { "uint16_t" };
            lines.push("{".into());
            lines.push(format!(
                "    unsigned count = ({count} & 0x1F) % {};",
                width + 1
            ));
            lines.push("    unsigned orig_count = count;".into());
            lines.push(format!("    {ctype} tmp = {acc}({inner});"));
            lines.push("    while (count--) {".into());
            lines.push("        unsigned new_cf = tmp & 1;".into());
            lines.push(format!(
                "        tmp = ((tmp >> 1) | (CF << {})) & {mask};",
                width - 1
            ));
            lines.push("        CF = new_cf;".into());
            lines.push("    }".into());
            lines.push(format!("    {acc}_write({inner}, tmp & {mask});"));
            lines.push("    if (orig_count == 1) {".into());
            lines.push(format!(
                "        OF = ((tmp >> {}) & 1) ^ ((tmp >> {}) & 1);",
                width - 1,
                width - 2
            ));
            lines.push("    }".into());
            lines.push("}".into());
        } else {
            lines.push("{".into());
            lines.push(format!(
                "    unsigned count = ({count} & 0x1F) % {};",
                width + 1
            ));
            lines.push("    unsigned orig_count = count;".into());
            lines.push("    while (count--) {".into());
            lines.push(format!("        unsigned new_cf = {dest} & 1;"));
            lines.push(format!(
                "        {dest} = (({dest} >> 1) | (CF << {})) & {mask};",
                width - 1
            ));
            lines.push("        CF = new_cf;".into());
            lines.push("    }".into());
            lines.push("    if (orig_count == 1) {".into());
            lines.push(format!(
                "        OF = (({dest} >> {}) & 1) ^ (({dest} >> {}) & 1);",
                width - 1,
                width - 2
            ));
            lines.push("    }".into());
            lines.push("}".into());
        }
        lines
    }

    fn handle_aaa_aas(&mut self, is_add: bool) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        self.invalidate_register("ax");
        let op = if is_add { "+" } else { "-" };
        lines.push("{".into());
        lines.push("    uint8_t tmp = al;".into());
        lines.push("    if ((tmp & 0x0F) > 9) {".into());
        lines.push(format!("        al = (uint8_t)((tmp {op} 6) & 0x0F);"));
        lines.push(format!("        ah = (uint8_t)((ah {op} 1) & 0xFF);"));
        lines.push("        CF = 1;".into());
        lines.push("    } else {".into());
        lines.push("        al = tmp & 0x0F;".into());
        lines.push("        CF = 0;".into());
        lines.push("    }".into());
        lines.push("}".into());
        lines
    }
    fn handle_daa_das(&mut self, is_add: bool) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        self.invalidate_register("ax");
        let op = if is_add { "+" } else { "-" };
        lines.push("{".into());
        lines.push("    uint8_t old_al = al;".into());
        lines.push("    uint8_t old_cf = (uint8_t)CF;".into());
        lines.push("    uint8_t new_cf = 0;".into());
        lines.push("    if (((old_al & 0x0F) > 9)) {".into());
        lines.push(format!("        al = (uint8_t)(al {op} 6);"));
        lines.push("    }".into());
        lines.push("    if (old_cf || old_al > 0x99) {".into());
        lines.push(format!("        al = (uint8_t)(al {op} 0x60);"));
        lines.push("        new_cf = 1;".into());
        lines.push("    }".into());
        lines.push("    CF = new_cf;".into());
        lines.push("    ZF = (al == 0);".into());
        lines.push("    SF = (al >> 7) & 1;".into());
        lines.push("    PF = parity8(al);".into());
        lines.push("}".into());
        lines
    }
    fn handle_lahf(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        self.last_ah = None;
        lines.push("ah = (uint8_t)((SF << 7) | (ZF << 6) | (PF << 2) | 0x02 | CF);".into());
        lines
    }
    fn handle_sahf(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        for l in [
            "SF = (ah >> 7) & 1;",
            "ZF = (ah >> 6) & 1;",
            "PF = (ah >> 2) & 1;",
            "CF = ah & 1;",
        ] {
            lines.push(l.into());
        }
        lines
    }
    fn handle_salc(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        self.last_al = None;
        lines.push("al = CF ? 0xFF : 0x00;".into());
        lines
    }
    fn aad_aam_base(&self, insn: &Insn) -> i64 {
        let op = decode_variables(s(insn, "op_str"));
        let op = op.trim();
        if op.is_empty() {
            10
        } else {
            parse_imm(op).unwrap_or(10)
        }
    }
    fn handle_aam(&mut self, insn: &Insn) -> Vec<String> {
        let base = self.aad_aam_base(insn);
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        self.last_al = None;
        self.last_ah = None;
        if base != 0 {
            lines.push(format!(
                "ah = (uint8_t)(al / {base}); al = (uint8_t)(al % {base});"
            ));
        }
        lines.push("ZF = al == 0; SF = (al >> 7) & 1; PF = parity8(al);".into());
        lines
    }
    fn handle_aad(&mut self, insn: &Insn) -> Vec<String> {
        let base = self.aad_aam_base(insn);
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        self.last_al = None;
        self.last_ah = None;
        lines.push(format!(
            "al = (uint8_t)((al + ah * {base}) & 0xFF); ah = 0;"
        ));
        lines.push("ZF = al == 0; SF = (al >> 7) & 1; PF = parity8(al);".into());
        lines
    }
    fn handle_bound(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            self.emit_unsupported_abort(insn, format!("bound {op_str}").trim());
        }
        let rw = self.rewrite_operands(insn, &[parts[0].clone()]);
        let dest = rw[0].clone();
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"(?i)^dword ptr \[(.+)\]$").unwrap());
        let inner = match re.captures(&parts[1]) {
            Some(c) => c.get(1).unwrap().as_str().trim().to_string(),
            None => self.emit_unsupported_abort(insn, format!("bound {op_str}").trim()),
        };
        let lower_access = rewrite_mem_op(&format!("word ptr [{inner}]"), None);
        let (helper, inner_args) = match split_mem_accessor(&lower_access) {
            Some(x) => x,
            None => self.emit_unsupported_abort(insn, format!("bound {op_str}").trim()),
        };
        let (seg_expr, addr_expr) = match inner_args.split_once(',') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => self.emit_unsupported_abort(insn, format!("bound {op_str}").trim()),
        };
        let upper_inner = format!("{seg_expr}, ((({addr_expr}) + 2) & 0xFFFF)");
        lines.push("{".into());
        lines.push(format!("    uint16_t lower = {helper}({inner_args});"));
        lines.push(format!("    uint16_t upper = {helper}({upper_inner});"));
        lines.push(format!("    uint16_t value = {dest};"));
        lines.push("    if (value < lower || value > upper) {".into());
        lines.push(
            "        shim_log_crash(\"[BOUND] %04X not in [%04X, %04X]\\n\", value, lower, upper);"
                .into(),
        );
        lines.push("        abort();".into());
        lines.push("    }".into());
        lines.push("}".into());
        lines
    }

    fn handle_iret(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        lines.push("iret();".into());
        lines.push("return;".into());
        lines
    }

    fn handle_rcl(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        let rw = self.rewrite_operands(insn, &[parts[0].clone()]);
        let dest = rw[0].clone();
        let count = if parts.len() == 2 {
            parts[1].clone()
        } else {
            "1".into()
        };
        self.invalidate_register(&dest);
        let is_mem = dest.starts_with("memw(") || dest.starts_with("memb(");
        let is_byte =
            dest.starts_with("memb(") || BYTE_REGS.contains(&dest.to_lowercase().as_str());
        let width = if is_byte { 8 } else { 16 };
        let mask = if is_byte { "0xFF" } else { "0xFFFF" };
        if is_mem {
            let inner = &dest[5..dest.len() - 1];
            let acc = if dest.starts_with("memb(") {
                "memb"
            } else {
                "memw"
            };
            let ctype = if acc == "memb" { "uint8_t" } else { "uint16_t" };
            lines.push("{".into());
            lines.push(format!(
                "    unsigned count = ({count} & 0x1F) % {};",
                width + 1
            ));
            lines.push("    unsigned orig_count = count;".into());
            lines.push(format!("    {ctype} tmp = {acc}({inner});"));
            lines.push("    while (count--) {".into());
            lines.push(format!(
                "        unsigned new_cf = (tmp >> {}) & 1;",
                width - 1
            ));
            lines.push(format!("        tmp = ((tmp << 1) | CF) & {mask};"));
            lines.push("        CF = new_cf;".into());
            lines.push("    }".into());
            lines.push(format!("    {acc}_write({inner}, tmp & {mask});"));
            lines.push("    if (orig_count == 1) {".into());
            lines.push(format!("        OF = ((tmp >> {}) & 1) ^ CF;", width - 1));
            lines.push("    }".into());
            lines.push("}".into());
        } else {
            lines.push("{".into());
            lines.push(format!(
                "    unsigned count = ({count} & 0x1F) % {};",
                width + 1
            ));
            lines.push("    unsigned orig_count = count;".into());
            lines.push("    while (count--) {".into());
            lines.push(format!(
                "        unsigned new_cf = ({dest} >> {}) & 1;",
                width - 1
            ));
            lines.push(format!("        {dest} = (({dest} << 1) | CF) & {mask};"));
            lines.push("        CF = new_cf;".into());
            lines.push("    }".into());
            lines.push("    if (orig_count == 1) {".into());
            lines.push(format!(
                "        OF = (({dest} >> {}) & 1) ^ CF;",
                width - 1
            ));
            lines.push("    }".into());
            lines.push("}".into());
        }
        lines
    }

    fn handle_lcall(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let mut op = op_str.to_lowercase().trim().to_string();
        let ret_arg = self.call_return_arg(insn);
        if op.contains(':') {
            op = op.splitn(2, ':').nth(1).unwrap().trim().to_string();
        }
        let first_seg = || {
            insn.get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|m| m.get("segment"))
                .and_then(Value::as_str)
                .map(|s| s.to_lowercase())
        };
        if op.starts_with('[') && op.ends_with(']') {
            let inner = op[1..op.len() - 1].trim().to_string();
            let seg_reg = first_seg().unwrap_or_else(|| "ds".into());
            let il = inner.to_lowercase();
            let parsed = if il.starts_with("0x") || il.starts_with("-0x") {
                parse_hex16(&inner)
            } else {
                inner.parse::<i64>().ok()
            };
            match parsed {
                Some(off) => {
                    let off = off & 0xFFFF;
                    let off2 = (off + 2) & 0xFFFF;
                    let expr = format!(
                        "lcall_table({ret_arg}, memw({seg_reg}, 0x{off2:04x}), memw({seg_reg}, 0x{off:04x}));"
                    );
                    self.emit_with_comment(&mut lines, insn, &expr, "");
                    return lines;
                }
                None => {
                    static RE: OnceLock<Regex> = OnceLock::new();
                    let re = RE.get_or_init(|| {
                        Regex::new(r"(?i)^(bx|bp|si|di)(?:\s*([+-])\s*(0x[0-9a-f]+|\d+))?$")
                            .unwrap()
                    });
                    if let Some(m) = re.captures(&inner) {
                        let reg = m.get(1).unwrap().as_str().to_lowercase();
                        let off = match m.get(3) {
                            Some(os) => {
                                let osz = os.as_str();
                                let mut imm = if osz.to_lowercase().starts_with("0x") {
                                    i64::from_str_radix(&osz[2..], 16).unwrap_or(0)
                                } else {
                                    osz.parse::<i64>().unwrap_or(0)
                                };
                                if m.get(2).map(|s| s.as_str()) == Some("-") {
                                    imm = (-imm) & 0xFFFF;
                                }
                                imm & 0xFFFF
                            }
                            None => 0,
                        };
                        let reg_expr = if off == 0 {
                            reg.clone()
                        } else {
                            format!("{reg} + 0x{off:04X}")
                        };
                        let off2 = (off + 2) & 0xFFFF;
                        let seg_expr = if off2 == 0 {
                            reg.clone()
                        } else {
                            format!("{reg} + 0x{off2:04X}")
                        };
                        let expr = format!(
                            "lcall_table({ret_arg}, memw({seg_reg}, {seg_expr}), memw({seg_reg}, {reg_expr}));"
                        );
                        self.emit_with_comment(&mut lines, insn, &expr, "");
                        return lines;
                    }
                }
            }
        }
        // imm seg, off
        static IMM: OnceLock<Regex> = OnceLock::new();
        let imm = IMM.get_or_init(|| {
            Regex::new(r"(?i)^(0x[0-9a-f]+|\d+)\s*,\s*(0x[0-9a-f]+|\d+)$").unwrap()
        });
        if let Some(m) = imm.captures(&op) {
            let pv = |x: &str| -> i64 {
                if x.to_lowercase().starts_with("0x") {
                    i64::from_str_radix(&x[2..], 16).unwrap_or(0)
                } else {
                    x.parse::<i64>().unwrap_or(0)
                }
            };
            let mut seg_val = pv(m.get(1).unwrap().as_str()) & 0xFFFF;
            let off_val = pv(m.get(2).unwrap().as_str()) & 0xFFFF;
            let size_bytes = isize_bytes(insn);
            if self
                .reloc_offsets
                .contains(&(i64f(insn, "address").unwrap_or(0) + size_bytes - 2))
            {
                seg_val = (seg_val + self.load_segment) & 0xFFFF;
            }
            let expr = format!("lcall_table({ret_arg}, 0x{seg_val:04X}, 0x{off_val:04X});");
            self.emit_with_comment(&mut lines, insn, &expr, "");
            return lines;
        }
        if let Some(m) = var_re().captures(&op) {
            let mem_ref = insn
                .get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first());
            let (seg_reg, disp) = match mem_ref {
                Some(mr) => (
                    mr.get("segment")
                        .and_then(Value::as_str)
                        .unwrap_or("ss")
                        .to_lowercase(),
                    mr.get("disp").and_then(Value::as_i64).unwrap_or(0),
                ),
                None => (
                    "ss".into(),
                    -i64::from_str_radix(m.get(1).unwrap().as_str(), 16).unwrap_or(0),
                ),
            };
            let off_expr = format!("((bp + 0x{:04X}) & 0xFFFF)", disp & 0xFFFF);
            let seg_expr = format!("((bp + 0x{:04X}) & 0xFFFF)", (disp + 2) & 0xFFFF);
            let expr = format!(
                "lcall_table({ret_arg}, memw({seg_reg}, {seg_expr}), memw({seg_reg}, {off_expr}));"
            );
            self.emit_with_comment(&mut lines, insn, &expr, "");
            return lines;
        }
        let asm = format!("{} {}", s(insn, "mnemonic"), s(insn, "op_str"))
            .trim()
            .to_string();
        self.emit_unsupported_abort(insn, &asm);
    }

    fn handle_ljmp(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ':')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() == 2 {
            let seg_p = parse_hex16(&parts[0]);
            let off_p = parse_hex16(&parts[1]);
            if let (Some(mut seg), Some(off)) = (seg_p, off_p) {
                let size_bytes = isize_bytes(insn);
                if self
                    .reloc_offsets
                    .contains(&(i64f(insn, "address").unwrap_or(0) + size_bytes - 2))
                {
                    seg = (seg + self.load_segment) & 0xFFFF;
                }
                let expr = format!("long_jump(0x{seg:04X}, 0x{off:04X});");
                self.emit_with_comment(&mut lines, insn, &expr, "");
                lines.push("return;".into());
                return lines;
            }
            // seg_reg:[mem] form
            let seg_reg = parts[0].to_lowercase();
            let mem = parts[1].trim();
            if mem.starts_with('[')
                && mem.ends_with(']')
                && matches!(seg_reg.as_str(), "cs" | "ds" | "es" | "ss")
            {
                let inner = &mem[1..mem.len() - 1];
                let il = inner.to_lowercase();
                let parsed = if il.starts_with("0x") || il.starts_with("-0x") {
                    parse_hex16(inner)
                } else {
                    inner.parse::<i64>().ok()
                };
                match parsed {
                    None => {
                        let off_mem =
                            rewrite_mem_op(&format!("word ptr [{inner}]"), Some(&seg_reg));
                        let seg_mem =
                            rewrite_mem_op(&format!("word ptr [{inner}+2]"), Some(&seg_reg));
                        let expr = format!("long_jump({seg_mem}, {off_mem});");
                        self.emit_with_comment(&mut lines, insn, &expr, "");
                        lines.push("return;".into());
                        return lines;
                    }
                    Some(off) => {
                        let off = off & 0xFFFF;
                        let off2 = (off + 2) & 0xFFFF;
                        let seg_mem = self.rcb_read(&format!("memw({seg_reg}, 0x{off2:04X})"));
                        let off_mem = self.rcb_read(&format!("memw({seg_reg}, 0x{off:04X})"));
                        let expr = format!("long_jump({seg_mem}, {off_mem});");
                        self.emit_with_comment(&mut lines, insn, &expr, "");
                        lines.push("return;".into());
                        return lines;
                    }
                }
            }
        }
        // [mem] no segment prefix
        let mem = op_str.trim();
        if mem.starts_with('[') && mem.ends_with(']') {
            let inner = mem[1..mem.len() - 1].trim();
            let seg_reg = insn
                .get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|m| m.get("segment"))
                .and_then(Value::as_str)
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "ds".into());
            let il = inner.to_lowercase();
            let parsed = if il.starts_with("0x") || il.starts_with("-0x") {
                parse_hex16(inner)
            } else {
                inner.parse::<i64>().ok()
            };
            let (off_mem, seg_mem) = match parsed {
                None => (
                    rewrite_mem_op(&format!("word ptr [{inner}]"), Some(&seg_reg)),
                    rewrite_mem_op(&format!("word ptr [{inner}+2]"), Some(&seg_reg)),
                ),
                Some(off) => {
                    let off = off & 0xFFFF;
                    let off2 = (off + 2) & 0xFFFF;
                    (
                        format!("memw({seg_reg}, 0x{off:04X})"),
                        format!("memw({seg_reg}, 0x{off2:04X})"),
                    )
                }
            };
            let expr = format!(
                "long_jump({}, {});",
                self.rcb_read(&seg_mem),
                self.rcb_read(&off_mem)
            );
            self.emit_with_comment(&mut lines, insn, &expr, "");
            lines.push("return;".into());
            return lines;
        }
        self.emit_unsupported_abort(insn, format!("ljmp {op_str}").trim());
    }

    /// _rcb_read (the source handle_ljmp/handle_jmp): rewrite a memw/memb RCB
    /// field access to rcb_read16/rcb_read8, else return the expr unchanged.
    fn rcb_read(&self, expr: &str) -> String {
        match self.match_rcb_access(expr) {
            Some((size, field)) => {
                format!(
                    "{}({field})",
                    if size == "w" {
                        "rcb_read16"
                    } else {
                        "rcb_read8"
                    }
                )
            }
            None => expr.to_string(),
        }
    }

    fn handle_shift(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            self.emit_unsupported_abort(insn, format!("{} {op_str}", s(insn, "mnemonic")).trim());
        }
        let rw = self.rewrite_operands(insn, &parts);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        let reg = dest.to_lowercase();
        let width: i64 = match split_mem_accessor(&dest) {
            Some((h, _)) => {
                if h == "memb" {
                    8
                } else {
                    16
                }
            }
            None => {
                if BYTE_REGS.contains(&reg.as_str()) {
                    8
                } else {
                    16
                }
            }
        };
        let mask = if width == 8 { "0xFF" } else { "0xFFFF" };
        let shift = width - 1;
        let ctype = if width == 8 { "uint8_t" } else { "uint16_t" };
        let dest_value = value_expr(&dest);
        self.invalidate_register(&dest);
        let mnem = s(insn, "mnemonic").to_string();
        lines.push("{".into());
        lines.push(format!("    unsigned count = {src} & 0x1F;"));
        lines.push("    if (count) {".into());
        lines.push("        unsigned orig_count = count;".into());
        if mnem == "sar" {
            let ctype_s = if width == 8 { "int8_t" } else { "int16_t" };
            lines.push(format!("        {ctype_s} tmp = ({ctype_s}){dest_value};"));
            lines.push("        while (count--) {".into());
            lines.push("            CF = tmp & 1;".into());
            lines.push("            tmp >>= 1;".into());
            lines.push("        }".into());
            emit_store(&mut lines, &dest, &format!("({ctype})tmp"), "        ");
            lines.push("        OF = 0;".into());
        } else if mnem == "shr" {
            lines.push(format!("        {ctype} tmp = {dest_value};"));
            lines.push(format!(
                "        unsigned orig_sign = (tmp >> {shift}) & 1;"
            ));
            lines.push("        while (count--) {".into());
            lines.push("            CF = tmp & 1;".into());
            lines.push("            tmp >>= 1;".into());
            lines.push("        }".into());
            emit_store(&mut lines, &dest, "tmp", "        ");
            lines.push("        OF = (orig_count == 1) ? orig_sign : 0;".into());
        } else {
            lines.push(format!("        {ctype} tmp = {dest_value};"));
            lines.push("        while (count--) {".into());
            lines.push(format!("            CF = (tmp >> {shift}) & 1;"));
            lines.push(format!("            tmp = ({ctype})((tmp << 1) & {mask});"));
            lines.push("        }".into());
            emit_store(&mut lines, &dest, "tmp", "        ");
            lines.push(format!(
                "        OF = (orig_count == 1) ? (CF ^ ((tmp >> {shift}) & 1)) : 0;"
            ));
        }
        let result_expr = value_expr(&dest);
        lines.push(format!("        ZF = {result_expr} == 0;"));
        lines.push(format!("        PF = parity8((uint8_t){result_expr});"));
        lines.push(format!("        SF = ({result_expr} >> {shift}) & 1;"));
        lines.push("    }".into());
        lines.push("}".into());
        lines
    }

    fn handle_rotate(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            self.emit_unsupported_abort(insn, format!("{} {op_str}", s(insn, "mnemonic")).trim());
        }
        let rw = self.rewrite_operands(insn, &parts);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        let reg = dest.to_lowercase();
        self.invalidate_register(&dest);
        let width: i64 = if dest.starts_with("memb(") || BYTE_REGS.contains(&reg.as_str()) {
            8
        } else {
            16
        };
        let is_mem = dest.starts_with("memw(") || dest.starts_with("memb(");
        lines.push("{".into());
        lines.push(format!("    unsigned count = {src} & {};", width - 1));
        lines.push("    if (count) {".into());
        if is_mem {
            let inner = &dest[5..dest.len() - 1];
            let acc = if dest.starts_with("memw(") {
                "memw"
            } else {
                "memb"
            };
            lines.push(format!(
                "        {acc}_write({inner}, ({acc}({inner}) << count) | ({acc}({inner}) >> ({width} - count)));"
            ));
            lines.push(format!("        CF = {acc}({inner}) & 1;"));
            lines.push("        if (count == 1) {".into());
            lines.push(format!(
                "            OF = (({acc}({inner}) >> {}) & 1) ^ CF;",
                width - 1
            ));
            lines.push("        } else {".into());
            lines.push("            OF = 0;".into());
            lines.push("        }".into());
        } else {
            lines.push(format!(
                "        {dest} = ({dest} << count) | ({dest} >> ({width} - count));"
            ));
            lines.push(format!("        CF = {dest} & 1;"));
            lines.push("        if (count == 1) {".into());
            lines.push(format!(
                "            OF = (({dest} >> {}) & 1) ^ CF;",
                width - 1
            ));
            lines.push("        } else {".into());
            lines.push("            OF = 0;".into());
            lines.push("        }".into());
        }
        lines.push("    }".into());
        lines.push("}".into());
        lines
    }

    fn handle_mul(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str")).trim().to_string();
        let ops = self.rewrite_operands(insn, &[op_str]);
        let operand = &ops[0];
        self.invalidate_register("dx");
        self.invalidate_register("ax");
        let regs_write = detail_regs(insn, "regs_write");
        if regs_write.contains("dx") {
            lines.push("{".into());
            lines.push(format!("    uint32_t tmp = (uint32_t)ax * {operand};"));
            lines.push("    dx = (tmp >> 16) & 0xFFFF;".into());
            lines.push("    ax = tmp & 0xFFFF;".into());
            lines.push("}".into());
            lines.push("CF = OF = dx != 0;".into());
        } else {
            lines.push(format!("ax = (al * {operand}) & 0xFFFF;"));
            lines.push("CF = OF = ah != 0;".into());
        }
        lines
    }
    fn handle_imul(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str")).trim().to_string();
        let regs_write = detail_regs(insn, "regs_write");
        let parts: Vec<String> = if op_str.is_empty() {
            vec![]
        } else {
            op_str.split(',').map(|p| p.trim().to_string()).collect()
        };
        if parts.len() >= 2 {
            let dest = self.rewrite_operands(insn, &[parts[0].clone()])[0].clone();
            let (fa, fb) = if parts.len() >= 3 {
                (
                    self.rewrite_operands(insn, &[parts[1].clone()])[0].clone(),
                    self.rewrite_operands(insn, &[parts[2].clone()])[0].clone(),
                )
            } else {
                (
                    dest.clone(),
                    self.rewrite_operands(insn, &[parts[1].clone()])[0].clone(),
                )
            };
            self.invalidate_register(&parts[0].trim().to_lowercase());
            lines.push("{".into());
            lines.push(format!(
                "    int32_t tmp = (int16_t)({fa}) * (int16_t)({fb});"
            ));
            lines.push(format!("    {dest} = tmp & 0xFFFF;"));
            lines.push("    CF = OF = (tmp != (int32_t)(int16_t)tmp);".into());
            lines.push("}".into());
            return lines;
        }
        let ops = self.rewrite_operands(insn, &[op_str]);
        let operand = ops[0].clone();
        if operand_width_from_str(&operand) == Some(8) && !regs_write.contains("dx") {
            self.invalidate_register("ax");
            lines.push("{".into());
            lines.push(format!("    int16_t tmp = (int8_t)al * (int8_t){operand};"));
            lines.push("    ax = (uint16_t)tmp & 0xFFFF;".into());
            lines.push("    CF = OF = (tmp < -128) || (tmp > 127);".into());
            lines.push("}".into());
        } else {
            self.invalidate_register("dx");
            self.invalidate_register("ax");
            lines.push("{".into());
            lines.push(format!(
                "    int32_t tmp = (int16_t)ax * (int16_t){operand};"
            ));
            lines.push("    dx = (tmp >> 16) & 0xFFFF;".into());
            lines.push("    ax = tmp & 0xFFFF;".into());
            lines.push("    CF = OF = tmp != (int32_t)(int16_t)ax;".into());
            lines.push("}".into());
        }
        lines
    }
    fn handle_div(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str")).trim().to_string();
        let ops = self.rewrite_operands(insn, &[op_str]);
        let operand = &ops[0];
        self.invalidate_register("dx");
        self.invalidate_register("ax");
        let regs_write = detail_regs(insn, "regs_write");
        if regs_write.contains("dx") {
            lines.push("{".into());
            lines.push("    uint32_t tmp = ((uint32_t)dx << 16) | ax;".into());
            lines.push(format!("    uint16_t divisor = {operand};"));
            lines.push("    ax = (tmp / divisor) & 0xFFFF;".into());
            lines.push("    dx = (tmp % divisor) & 0xFFFF;".into());
            lines.push("}".into());
        } else {
            lines.push("{".into());
            lines.push("    uint16_t tmp = ax;".into());
            lines.push(format!("    uint8_t divisor = {operand};"));
            lines.push("    al = (tmp / divisor) & 0xFF;".into());
            lines.push("    ah = (tmp % divisor) & 0xFF;".into());
            lines.push("}".into());
        }
        lines
    }
    fn handle_idiv(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str")).trim().to_string();
        let ops = self.rewrite_operands(insn, &[op_str]);
        let operand = &ops[0];
        let regs_write = detail_regs(insn, "regs_write");
        if regs_write.contains("dx") {
            self.invalidate_register("dx");
        }
        if regs_write.contains("ax") || regs_write.contains("dx") {
            self.invalidate_register("ax");
        }
        if regs_write.contains("dx") {
            lines.push("{".into());
            lines.push("    int32_t dividend = ((int32_t)(int16_t)dx << 16) | ax;".into());
            lines.push(format!("    int16_t divisor = (int16_t){operand};"));
            lines.push("    ax = (uint16_t)(dividend / divisor);".into());
            lines.push("    dx = (uint16_t)(dividend % divisor);".into());
            lines.push("}".into());
        } else {
            lines.push("{".into());
            lines.push("    int16_t dividend = (int16_t)ax;".into());
            lines.push(format!("    int8_t divisor = (int8_t){operand};"));
            lines.push("    al = (uint8_t)((dividend / divisor) & 0xFF);".into());
            lines.push("    ah = (uint8_t)((dividend % divisor) & 0xFF);".into());
            lines.push("}".into());
        }
        lines
    }
    fn handle_not(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str")).trim().to_string();
        let ops = self.rewrite_operands(insn, &[op_str]);
        let operand = ops[0].clone();
        self.invalidate_register(&operand);
        if operand.starts_with("memw(") || operand.starts_with("memb(") {
            let (inner, acc, mask, _s, _sh) = Self::mem_parts(&operand);
            lines.push(format!("{acc}_write({inner}, (~{acc}({inner})) & {mask});"));
            lines.push(format!("ZF = {acc}({inner}) == 0;"));
        } else {
            let mask = if BYTE_REGS.contains(&operand.to_lowercase().as_str()) {
                "0xFF"
            } else {
                "0xFFFF"
            };
            lines.push(format!("{operand} = (~{operand}) & {mask};"));
            lines.push(format!("ZF = {operand} == 0;"));
        }
        lines
    }
    fn handle_neg(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str")).trim().to_string();
        let ops = self.rewrite_operands(insn, &[op_str]);
        let operand = ops[0].clone();
        self.invalidate_register(&operand);
        let is_mem = operand.starts_with("memw(") || operand.starts_with("memb(");
        let is_byte =
            operand.starts_with("memb(") || BYTE_REGS.contains(&operand.to_lowercase().as_str());
        let (ctype, shift, sign_bit, mask) = if is_byte {
            ("uint8_t", 7, "0x80", "0xFF")
        } else {
            ("uint16_t", 15, "0x8000", "0xFFFF")
        };
        if is_mem {
            let inner = &operand[5..operand.len() - 1];
            let acc = if operand.starts_with("memb(") {
                "memb"
            } else {
                "memw"
            };
            lines.push("{".into());
            lines.push(format!("    {ctype} tmp = {acc}({inner});"));
            lines.push(format!("    {acc}_write({inner}, (0 - tmp) & {mask});"));
            lines.push("    CF = tmp != 0;".into());
            lines.push(format!("    ZF = {acc}({inner}) == 0;"));
            lines.push(format!("    PF = parity8((uint8_t){acc}({inner}));"));
            lines.push(format!("    SF = ({acc}({inner}) >> {shift}) & 1;"));
            lines.push(format!("    OF = tmp == {sign_bit};"));
            lines.push("}".into());
        } else {
            lines.push("{".into());
            lines.push(format!("    {ctype} tmp = {operand};"));
            lines.push(format!("    {operand} = (0 - tmp) & {mask};"));
            lines.push("    CF = tmp != 0;".into());
            lines.push(format!("    ZF = {operand} == 0;"));
            lines.push(format!("    PF = parity8((uint8_t){operand});"));
            lines.push(format!("    SF = ({operand} >> {shift}) & 1;"));
            lines.push(format!("    OF = tmp == {sign_bit};"));
            lines.push("}".into());
        }
        lines
    }

    fn clear_last(&mut self) {
        self.last_ah = None;
        self.last_al = None;
        self.last_dx = None;
        self.last_bx = None;
        self.last_dl = None;
        self.last_cx = None;
        self.last_cl = None;
        self.last_ch = None;
    }

    /// handle_interrupt.
    fn handle_interrupt(&mut self, insn: &Insn) -> Vec<String> {
        let op = s(insn, "op_str").to_lowercase();
        let op = op.trim().trim_end_matches('h');
        let int_num = i64::from_str_radix(op.trim_start_matches("0x"), 16).ok();
        if int_num == Some(0x21) {
            if let Some(r) = self.handle_dos_interrupt_call() {
                return r;
            }
        }
        if let Some(r) = self.handle_bios_interrupt_call(int_num) {
            return r;
        }
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        if int_num == Some(0x21) {
            lines.push("dos_api();".into());
            return lines;
        }
        if let Some(n) = int_num {
            if n == 0x20 {
                lines.push("dos_exit();".into());
                return lines;
            }
            if n == 0x16 {
                lines.push("bios_keyboard();".into());
                return lines;
            }
            lines.push(format!("run_interrupt(0x{n:02X});"));
            return lines;
        }
        self.emit_unsupported_abort(insn, &format!("int {}", s(insn, "op_str")));
    }

    /// handle_dos_interrupt_call.
    fn handle_dos_interrupt_call(&mut self) -> Option<Vec<String>> {
        let ah = self.last_ah?;
        let func = dos_api_func(ah);
        let mut args: Vec<String> = Vec::new();
        let bx4 = opt_hex(self.last_bx, 4, "bx");
        let dx4 = opt_hex(self.last_dx, 4, "dx");
        let cx4 = opt_hex(self.last_cx, 4, "cx");
        let al2 = opt_hex(self.last_al, 2, "al");
        let dx_seg = |v: Option<i64>, cast: &str| -> String {
            match v {
                Some(x) => format!("{cast}seg_off(cs, 0x{x:04X})"),
                None => format!("{cast}seg_off(ds, dx)"),
            }
        };
        match ah {
            0x48 => args.push(bx4),
            0x49 => args.push("(void *)seg_off(es, 0)".into()),
            0x4B => {
                args.push(match self.last_bx {
                    Some(x) => format!("(void *)0x{x:04X}"),
                    None => "(void *)bx".into(),
                });
                args.push(dx_seg(self.last_dx, "(const char *)"));
            }
            0x25 => {
                args.push(al2);
                args.push("ds".into());
                args.push(dx4);
            }
            0x42 => {
                args.push(bx4);
                args.push(cx4);
                args.push(dx4);
                args.push(al2);
            }
            0x3E => args.push(bx4),
            0x3F => {
                args.push(bx4);
                args.push(dx_seg(self.last_dx, "(void *)"));
                args.push(cx4);
            }
            0x40 => {
                args.push(bx4);
                args.push(dx_seg(self.last_dx, "(const void *)"));
                args.push(cx4);
            }
            0x39 | 0x3B | 0x41 => args.push(dx_seg(self.last_dx, "(const char *)")),
            0x56 => {
                args.push("(const char *)seg_off(ds, dx)".into());
                args.push("(const char *)seg_off(es, di)".into());
            }
            0x44 => {
                args.push(al2);
                args.push(bx4);
            }
            0x47 => args.push("(char *)seg_off(ds, si)".into()),
            0x4A => {
                args.push("es".into());
                args.push(bx4);
            }
            0x43 => {
                args.push(dx_seg(self.last_dx, "(const char *)"));
                args.push("&cx".into());
            }
            0x3C => {
                args.push(dx_seg(self.last_dx, "(const char *)"));
                args.push(cx4);
            }
            0x0C => args.push(al2),
            0x2F | 0x4F => {}
            0x4E => {
                args.push(dx_seg(self.last_dx, "(const char *)"));
                args.push(cx4);
            }
            0x1A => args.push(dx_seg(self.last_dx, "(void *)")),
            0x06 => args.push("dl".into()),
            0x02 | 0x0E | 0x36 => args.push(opt_hex(self.last_dl, 2, "dl")),
            0x09 => args.push(dx_seg(self.last_dx, "(const char *)")),
            0x3D => args.push(dx_seg(self.last_dx, "(const char *)")),
            _ => {
                if self.last_dx.is_some() {
                    args.push("dx".into());
                }
            }
        }
        let mut lines = std::mem::take(&mut self.pending_dos);
        self.clear_last();
        let call = if args.is_empty() {
            format!("{func}()")
        } else {
            format!("{func}({})", args.join(", "))
        };
        if ah == 0x20 || ah == 0x4C || ah == 0x06 {
            lines.push(format!("{call};"));
        } else {
            lines.push(format!("CF = {call};"));
        }
        Some(lines)
    }

    /// handle_bios_interrupt_call.
    fn handle_bios_interrupt_call(&mut self, int_num: Option<i64>) -> Option<Vec<String>> {
        let int_num = int_num?;
        if int_num != 0x10 {
            return None; // only INT 10h has a _BIOS_API_FUNCS entry
        }
        let ah = self.last_ah?;
        let func = bios_api_func(ah);

        // Special reconstructions (return their own reg-restore blocks).
        let take = |r: &mut Renderer| -> Vec<String> {
            let l = std::mem::take(&mut r.pending_dos);
            l
        };
        if ah == 0x0F || ah == 0x1A {
            let mut lines = filter_pending(take(self), &["ax", "ah", "al", "bx", "bh"]);
            self.clear_last();
            lines.push("CF = 0;".into());
            if ah == 0x0F {
                lines.push("al = bios_current_video_mode();".into());
                lines.push("ah = bios_current_video_columns();".into());
                lines.push("bh = bios_current_active_page();".into());
            } else {
                lines.push("al = 0x1A;".into());
                lines.push("bl = bios_display_combination_code();".into());
                lines.push("bh = bios_display_combination_alt_code();".into());
            }
            return Some(lines);
        }
        if ah == 0x03 {
            let mut lines = filter_pending(take(self), &["dx", "cx"]);
            let page = match self.last_bx {
                Some(x) => format!("0x{:02X}", (x >> 8) & 0xFF),
                None => "bh".into(),
            };
            self.clear_last();
            lines.push(format!("bios_get_cursor({page});"));
            return Some(lines);
        }
        if ah == 0x08 {
            let mut lines = filter_pending(take(self), &["ax", "ah", "al"]);
            self.clear_last();
            lines.push("ax = bios_read_char_attr();".into());
            lines.push("CF = 0;".into());
            return Some(lines);
        }
        if ah == 0x30 {
            let mut lines = filter_pending(take(self), &["cx", "dx"]);
            self.clear_last();
            for l in [
                "{",
                "    uint16_t seg;",
                "    uint16_t off;",
                "    bios_get_video_parameter_block(al, &seg, &off);",
                "    cx = seg;",
                "    dx = off;",
                "}",
                "CF = 0;",
            ] {
                lines.push(l.into());
            }
            return Some(lines);
        }
        if ah == 0x06 || ah == 0x07 {
            let mut lines = take(self);
            self.clear_last();
            let dir = if ah == 0x06 { 0 } else { 1 };
            lines.push(format!(
                "bios_scroll_window(al, bh, ch, cl, dh, dl, {dir});"
            ));
            lines.push("CF = 0;".into());
            return Some(lines);
        }
        let func = match func {
            Some(f) => f,
            None => {
                self.pending_dos.clear();
                self.clear_last();
                self.emit_unsupported_abort(
                    &Insn::new(),
                    &format!("int 0x{int_num:02X} AH=0x{ah:02X} (unsupported BIOS)"),
                );
            }
        };
        let mut lines = take(self);
        self.last_ah = None;
        let mut call_args: Vec<String> = Vec::new();
        let mut regs_to_clear: Vec<&str> = Vec::new();
        match ah {
            0x00 => {
                let mode = opt_hex(self.last_al, 2, "al");
                let mut l = filter_pending(lines, &["ax", "ah", "al"]);
                self.last_dx = None;
                self.last_bx = None;
                self.last_dl = None;
                self.last_cx = None;
                self.last_cl = None;
                self.last_ch = None;
                self.last_al = None;
                l.push(format!("{func}({mode});"));
                return Some(l);
            }
            0x02 => {
                call_args.push(match self.last_bx {
                    Some(x) => format!("0x{:02X}", (x >> 8) & 0xFF),
                    None => "bh".into(),
                });
                match self.last_dx {
                    Some(x) => {
                        call_args.push(format!("0x{:02X}", (x >> 8) & 0xFF));
                        call_args.push(format!("0x{:02X}", x & 0xFF));
                    }
                    None => {
                        call_args.push("dh".into());
                        call_args.push("dl".into());
                    }
                }
                regs_to_clear = vec!["ax", "ah", "al", "bx", "dx"];
            }
            0x0B => {
                match self.last_bx {
                    Some(x) => {
                        call_args.push(format!("0x{:02X}", (x >> 8) & 0xFF));
                        call_args.push(format!("0x{:02X}", x & 0xFF));
                    }
                    None => {
                        call_args.push("bh".into());
                        call_args.push("bl".into());
                    }
                }
                regs_to_clear = vec!["ax", "ah", "al", "bx"];
            }
            0x0E => {
                call_args.push(opt_hex(self.last_al, 2, "al"));
                match self.last_bx {
                    Some(x) => {
                        call_args.push(format!("0x{:02X}", (x >> 8) & 0xFF));
                        call_args.push(format!("0x{:02X}", x & 0xFF));
                    }
                    None => {
                        call_args.push("bh".into());
                        call_args.push("bl".into());
                    }
                }
                regs_to_clear = vec!["ax", "ah", "al", "bx"];
            }
            0x09 => {
                call_args.push(opt_hex(self.last_al, 2, "al"));
                match self.last_bx {
                    Some(x) => {
                        call_args.push(format!("0x{:02X}", (x >> 8) & 0xFF));
                        call_args.push(format!("0x{:02X}", x & 0xFF));
                    }
                    None => {
                        call_args.push("bh".into());
                        call_args.push("bl".into());
                    }
                }
                call_args.push(opt_hex(self.last_cx, 4, "cx"));
                regs_to_clear = vec!["ax", "ah", "al"];
            }
            0x0A => {
                call_args.push(opt_hex(self.last_al, 2, "al"));
                call_args.push(match self.last_bx {
                    Some(x) => format!("0x{:02X}", (x >> 8) & 0xFF),
                    None => "bh".into(),
                });
                call_args.push(opt_hex(self.last_cx, 4, "cx"));
                regs_to_clear = vec!["ax", "ah", "al"];
            }
            _ => {}
        }
        self.last_dx = None;
        self.last_bx = None;
        self.last_dl = None;
        self.last_cx = None;
        self.last_cl = None;
        self.last_ch = None;
        self.last_al = None;
        if !regs_to_clear.is_empty() {
            lines = filter_pending(lines, &regs_to_clear);
        }
        let call = if call_args.is_empty() {
            format!("{func}()")
        } else {
            format!("{func}({})", call_args.join(", "))
        };
        lines.push(format!("{call};"));
        Some(lines)
    }

    fn handle_io(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        let mnem = s(insn, "mnemonic");
        if mnem == "in" && parts.len() == 2 {
            let (dest, port) = (&parts[0], &parts[1]);
            let dl = dest.to_lowercase();
            if dl == "al" || dl == "ax" {
                let func = if dl == "al" { "inb" } else { "inw" };
                lines.push(format!("{dest} = {func}({port});"));
                return lines;
            }
        }
        if mnem == "out" && parts.len() == 2 {
            let (port, src) = (&parts[0], &parts[1]);
            let sl = src.to_lowercase();
            if sl == "al" || sl == "ax" {
                let func = if sl == "al" { "outb" } else { "outw" };
                lines.push(format!("{func}({port}, {src});"));
                return lines;
            }
        }
        self.emit_unsupported_abort(insn, format!("{mnem} {op_str}").trim());
    }

    fn handle_pushf(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        lines.push("sp = (sp - 2) & 0xFFFF;".into());
        lines.push("memw_write(ss, sp, (uint16_t)(0x0002u | CF | (PF << 2) | (ZF << 6) | (SF << 7) | (IF << 9) | (DF << 10) | (OF << 11)));".into());
        lines
    }
    fn handle_popf(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        for l in [
            "uint16_t oldIF = IF;",
            "uint16_t flags = memw(ss, sp);",
            "CF = flags & 0x0001;",
            "PF = (flags >> 2) & 1;",
            "ZF = (flags >> 6) & 1;",
            "SF = (flags >> 7) & 1;",
            "IF = (flags >> 9) & 1;",
            "DF = (flags >> 10) & 1;",
            "OF = (flags >> 11) & 1;",
            "sp = (sp + 2) & 0xFFFF;",
            "if (!oldIF && IF) interrupt_shadow = 1;",
        ] {
            lines.push(l.into());
        }
        lines
    }
    fn handle_pushaw(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        lines.push("{".into());
        lines.push("    uint16_t orig_sp = sp;".into());
        for reg in ["ax", "cx", "dx", "bx"] {
            lines.push("    sp = (sp - 2) & 0xFFFF;".into());
            lines.push(format!("    memw_write(ss, sp, {reg});"));
        }
        lines.push("    sp = (sp - 2) & 0xFFFF;".into());
        lines.push("    memw_write(ss, sp, orig_sp);".into());
        for reg in ["bp", "si", "di"] {
            lines.push("    sp = (sp - 2) & 0xFFFF;".into());
            lines.push(format!("    memw_write(ss, sp, {reg});"));
        }
        lines.push("}".into());
        lines
    }
    fn handle_popaw(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        lines.push("{".into());
        for reg in ["di", "si", "bp"] {
            lines.push(format!("    {reg} = memw(ss, sp);"));
            lines.push("    sp = (sp + 2) & 0xFFFF;".into());
        }
        lines.push("    sp = (sp + 2) & 0xFFFF;  /* saved SP slot: popped, discarded */".into());
        for reg in ["bx", "dx", "cx", "ax"] {
            lines.push(format!("    {reg} = memw(ss, sp);"));
            lines.push("    sp = (sp + 2) & 0xFFFF;".into());
        }
        lines.push("}".into());
        for reg in ["ax", "bx", "cx", "dx"] {
            self.invalidate_register(reg);
        }
        lines
    }
    fn handle_leave(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        lines.push("sp = bp;".into());
        lines.push("bp = memw(ss, sp);".into());
        lines.push("sp = (sp + 2) & 0xFFFF;".into());
        lines
    }
    fn handle_enter(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let raw = s(insn, "op_str");
        let parts: Vec<&str> = raw.split(',').map(|p| p.trim()).collect();
        let alloc = (parts
            .first()
            .filter(|p| !p.is_empty())
            .and_then(|p| int0(p))
            .unwrap_or(0))
            & 0xFFFF;
        let level = ((parts
            .get(1)
            .filter(|p| !p.is_empty())
            .and_then(|p| int0(p))
            .unwrap_or(0))
            & 0xFF)
            % 32;
        lines.push(format!("/* enter 0x{alloc:X}, {level} */"));
        lines.push("sp = (sp - 2) & 0xFFFF;".into());
        lines.push("memw_write(ss, sp, bp);".into());
        lines.push("{".into());
        lines.push("    uint16_t frame_temp = sp;".into());
        for _ in 1..level {
            lines.push("    bp = (bp - 2) & 0xFFFF;".into());
            lines.push("    sp = (sp - 2) & 0xFFFF;".into());
            lines.push("    memw_write(ss, sp, memw(ss, bp));".into());
        }
        if level > 0 {
            lines.push("    sp = (sp - 2) & 0xFFFF;".into());
            lines.push("    memw_write(ss, sp, frame_temp);".into());
        }
        lines.push("    bp = frame_temp;".into());
        lines.push("}".into());
        if alloc != 0 {
            lines.push(format!("sp = (sp - 0x{alloc:X}) & 0xFFFF;"));
        }
        lines
    }
    fn handle_xchg(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() == 2 {
            let rw = self.rewrite_operands(insn, &parts);
            let (dest, src) = (rw[0].clone(), rw[1].clone());
            if dest != src {
                self.invalidate_register(&dest);
                self.invalidate_register(&src);
                let width = operand_width_from_str(&dest)
                    .or_else(|| operand_width_from_str(&src))
                    .unwrap_or(16);
                let ctype = if width == 8 { "uint8_t" } else { "uint16_t" };
                let dest_val = value_expr(&dest);
                let src_val = value_expr(&src);
                lines.push("{".into());
                lines.push(format!("    {ctype} tmp = {dest_val};"));
                emit_store(&mut lines, &dest, &src_val, "    ");
                emit_store(&mut lines, &src, "tmp", "    ");
                lines.push("}".into());
                return lines;
            }
        }
        self.emit_unsupported_abort(insn, format!("xchg {op_str}").trim());
    }

    /// _emit_load_far_ptr.
    fn emit_load_far_ptr(
        dest: &str,
        seg_reg: &str,
        mem_seg: &str,
        off: &str,
        seg_expr: &str,
    ) -> Vec<String> {
        vec![
            "{".into(),
            format!("    uint16_t _far_seg = memw({mem_seg}, {seg_expr});"),
            format!("    {dest} = memw({mem_seg}, {off});"),
            format!("    {seg_reg} = _far_seg;"),
            "}".into(),
        ]
    }
    /// handle_lds/handle_les (ir_to_c/2896). seg_reg = "ds" or "es".
    fn handle_load_far(&mut self, insn: &Insn, seg_reg: &str) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() == 2 {
            let dest = parts[0].clone();
            let dest_l = dest.to_lowercase();
            let mut src = parts[1].clone();
            if src.to_lowercase().starts_with("ptr ") {
                src = src[4..].trim().to_string();
            }
            if let Some(c) = mem_ptr_re().captures(&format!("word ptr {src}")) {
                let seg_grp = c.get(2).map(|m| m.as_str());
                let expr = c.get(3).unwrap().as_str();
                let seg = match seg_grp {
                    Some(sp) => sp.to_lowercase(),
                    None => {
                        if Regex::new(r"\bbp\b")
                            .unwrap()
                            .is_match(&expr.to_lowercase())
                        {
                            "ss".into()
                        } else {
                            "ds".into()
                        }
                    }
                };
                let el = expr.to_lowercase();
                let parsed = if el.starts_with("0x") || el.starts_with("-0x") {
                    parse_hex16(expr)
                } else {
                    expr.parse::<i64>().ok()
                };
                let (expr1, expr2) = match parsed {
                    Some(off) => {
                        let off = off & 0xFFFF;
                        (
                            format!("0x{off:04X}"),
                            format!("0x{:04X}", (off + 2) & 0xFFFF),
                        )
                    }
                    None => (expr.to_string(), format!("{expr} + 2")),
                };
                self.pending_dos
                    .retain(|e| !e.to_lowercase().starts_with(&format!("{dest_l} =")));
                if dest_l == "dx" {
                    self.last_dx = None;
                    self.last_dl = None;
                }
                lines.extend(Self::emit_load_far_ptr(
                    &dest, seg_reg, &seg, &expr1, &expr2,
                ));
                return lines;
            }
            let src_l = src.to_lowercase();
            if let Some(m) = var_re().captures(&src_l) {
                let mem_ref = insn
                    .get("detail")
                    .and_then(|d| d.get("mem_refs"))
                    .and_then(Value::as_array)
                    .and_then(|a| a.first());
                let (seg, disp) = match mem_ref {
                    Some(mr) => (
                        mr.get("segment")
                            .and_then(Value::as_str)
                            .unwrap_or("ss")
                            .to_lowercase(),
                        mr.get("disp").and_then(Value::as_i64).unwrap_or(0),
                    ),
                    None => (
                        "ss".into(),
                        -i64::from_str_radix(m.get(1).unwrap().as_str(), 16).unwrap_or(0),
                    ),
                };
                let expr1 = format!("((bp + 0x{:04X}) & 0xFFFF)", disp & 0xFFFF);
                let expr2 = format!("((bp + 0x{:04X}) & 0xFFFF)", (disp + 2) & 0xFFFF);
                self.pending_dos.retain(|e| {
                    let el = e.to_lowercase();
                    !el.starts_with(&format!("{dest_l}="))
                        && !el.starts_with(&format!("{dest_l} ="))
                });
                if dest_l == "dx" {
                    self.last_dx = None;
                    self.last_dl = None;
                }
                lines.extend(Self::emit_load_far_ptr(
                    &dest, seg_reg, &seg, &expr1, &expr2,
                ));
                return lines;
            }
        }
        self.flush_pending_dos(&mut lines, "");
        let mnem = if seg_reg == "ds" { "lds" } else { "les" };
        self.emit_unsupported_abort(insn, format!("{mnem} {op_str}").trim());
    }

    /// handle_arithmetic — add/adc/sub/sbb/and/or/xor/inc/dec.
    /// (RCB dest/src paths omitted: match_rcb_access is stubbed None.)
    fn handle_arithmetic(&mut self, insn: &Insn) -> Option<Vec<String>> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        let mnem = s(insn, "mnemonic").to_string();

        if matches!(mnem.as_str(), "inc" | "dec") {
            let ops = self.rewrite_operands(insn, &[op_str.trim().to_string()]);
            let operand = ops[0].clone();
            self.invalidate_register(&operand);
            self.arith_incdec(&mut lines, mnem == "inc", &operand);
            return Some(lines);
        }

        if parts.len() != 2 {
            return None;
        }
        let rw = self.rewrite_operands(insn, &parts);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        self.invalidate_register(&dest);
        match mnem.as_str() {
            "add" | "adc" | "sub" | "sbb" => {
                self.arith_addsub(&mut lines, &mnem, &dest, &src);
                Some(lines)
            }
            "and" | "or" => {
                self.arith_logic(
                    &mut lines,
                    if mnem == "and" { '&' } else { '|' },
                    &dest,
                    &src,
                );
                Some(lines)
            }
            "xor" => {
                self.arith_xor(&mut lines, &dest, &src);
                Some(lines)
            }
            _ => None,
        }
    }

    fn mem_parts(dest: &str) -> (String, &'static str, &'static str, &'static str, i64) {
        // (inner, acc, mask, sign_bit, shift) for a memw(...)/memb(...) dest.
        let acc = if dest.starts_with("memw(") {
            "memw"
        } else {
            "memb"
        };
        let inner = dest[5..dest.len() - 1].to_string();
        let (mask, sign, shift) = if acc == "memw" {
            ("0xFFFF", "0x8000", 15)
        } else {
            ("0xFF", "0x80", 7)
        };
        (inner, acc, mask, sign, shift)
    }
    fn reg_parts(dest: &str) -> (&'static str, &'static str, i64) {
        let is_byte = BYTE_REGS.contains(&dest.to_lowercase().as_str());
        if is_byte {
            ("0xFF", "0x80", 7)
        } else {
            ("0xFFFF", "0x8000", 15)
        }
    }

    fn arith_addsub(&self, lines: &mut Vec<String>, mnem: &str, dest: &str, src: &str) {
        let is_mem = dest.starts_with("memw(") || dest.starts_with("memb(");
        let (dr, wp, ws, mask, sign, shift): (String, String, String, &str, &str, i64) = if is_mem {
            let (inner, acc, mask, sign, shift) = Self::mem_parts(dest);
            (
                format!("{acc}({inner})"),
                format!("{acc}_write({inner}, "),
                ")".into(),
                mask,
                sign,
                shift,
            )
        } else {
            let (mask, sign, shift) = Self::reg_parts(dest);
            (
                dest.into(),
                format!("{dest} = "),
                String::new(),
                mask,
                sign,
                shift,
            )
        };
        lines.push("{".into());
        lines.push(format!("    uint32_t old = {dr};"));
        match mnem {
            "add" => {
                lines.push(format!("    uint32_t src = {src};"));
                lines.push("    uint32_t tmp = old + src;".into());
                lines.push(format!("    CF = tmp > {mask};"));
                lines.push(format!("    {wp}tmp & {mask}{ws};"));
                lines.push(format!(
                    "    OF = (~(old ^ src) & (old ^ tmp) & {sign}) != 0;"
                ));
            }
            "adc" => {
                lines.push(format!("    uint32_t src = {src};"));
                lines.push("    uint32_t tmp = old + src + CF;".into());
                lines.push(format!("    CF = tmp > {mask};"));
                lines.push(format!("    {wp}tmp & {mask}{ws};"));
                lines.push(format!(
                    "    OF = (~(old ^ src) & (old ^ tmp) & {sign}) != 0;"
                ));
            }
            "sub" => {
                lines.push(format!("    uint32_t src = {src};"));
                lines.push("    CF = old < src;".into());
                lines.push("    uint32_t tmp = old - src;".into());
                lines.push(format!("    {wp}tmp & {mask}{ws};"));
                lines.push(format!(
                    "    OF = ((old ^ src) & (old ^ tmp) & {sign}) != 0;"
                ));
            }
            "sbb" => {
                lines.push(format!("    uint32_t src = {src} + CF;"));
                lines.push("    CF = old < src;".into());
                lines.push("    uint32_t tmp = old - src;".into());
                lines.push(format!("    {wp}tmp & {mask}{ws};"));
                lines.push(format!(
                    "    OF = ((old ^ src) & (old ^ tmp) & {sign}) != 0;"
                ));
            }
            _ => {}
        }
        lines.push("}".into());
        lines.push(format!("ZF = {dr} == 0;"));
        lines.push(format!("PF = parity8((uint8_t){dr});"));
        lines.push(format!("SF = ({dr} >> {shift}) & 1;"));
    }

    fn arith_logic(&self, lines: &mut Vec<String>, op: char, dest: &str, src: &str) {
        let is_mem = dest.starts_with("memw(") || dest.starts_with("memb(");
        if is_mem {
            let (inner, acc, mask, _sign, shift) = Self::mem_parts(dest);
            lines.push("{".into());
            lines.push(format!("    uint32_t old = {acc}({inner});"));
            lines.push(format!("    uint32_t src = {src};"));
            lines.push(format!("    uint32_t tmp = (old {op} src) & {mask};"));
            lines.push(format!("    {acc}_write({inner}, tmp);"));
            lines.push("    CF = 0;".into());
            lines.push("    OF = 0;".into());
            lines.push("    ZF = tmp == 0;".into());
            lines.push("    PF = parity8((uint8_t)tmp);".into());
            lines.push(format!("    SF = (tmp >> {shift}) & 1;"));
            lines.push("}".into());
        } else {
            let (mask, _sign, shift) = Self::reg_parts(dest);
            lines.push(format!("{dest} = ({dest} {op} {src}) & {mask};"));
            lines.push("CF = 0;".into());
            lines.push("OF = 0;".into());
            lines.push(format!("ZF = {dest} == 0;"));
            lines.push(format!("PF = parity8((uint8_t){dest});"));
            lines.push(format!("SF = ({dest} >> {shift}) & 1;"));
        }
    }

    fn arith_xor(&self, lines: &mut Vec<String>, dest: &str, src: &str) {
        let is_mem = dest.starts_with("memw(") || dest.starts_with("memb(");
        if dest == src {
            if is_mem {
                let inner = &dest[5..dest.len() - 1];
                let writer = if dest.starts_with("memw(") {
                    "memw_write"
                } else {
                    "memb_write"
                };
                lines.push(format!("{writer}({inner}, 0);"));
                for l in ["CF = 0;", "OF = 0;", "ZF = 1;", "PF = 1;", "SF = 0;"] {
                    lines.push(l.into());
                }
            } else {
                let helper = if BYTE_REGS.contains(&dest.to_lowercase().as_str()) {
                    "xor8"
                } else {
                    "xor16"
                };
                lines.push(format!("{helper}(&{dest}, {src});"));
            }
            return;
        }
        if is_mem {
            let (inner, acc, mask, _sign, shift) = Self::mem_parts(dest);
            let writer = format!("{acc}_write");
            lines.push(format!(
                "{writer}({inner}, ({acc}({inner}) ^ {src}) & {mask});"
            ));
            lines.push("CF = 0;".into());
            lines.push("OF = 0;".into());
            lines.push(format!("ZF = {dest} == 0;"));
            lines.push(format!("PF = parity8((uint8_t){dest});"));
            lines.push(format!("SF = ({dest} >> {shift}) & 1;"));
        } else {
            let (mask, _sign, shift) = Self::reg_parts(dest);
            lines.push(format!("{dest} = ({dest} ^ {src}) & {mask};"));
            lines.push("CF = 0;".into());
            lines.push("OF = 0;".into());
            lines.push(format!("ZF = {dest} == 0;"));
            lines.push(format!("PF = parity8((uint8_t){dest});"));
            lines.push(format!("SF = ({dest} >> {shift}) & 1;"));
        }
    }

    fn arith_incdec(&self, lines: &mut Vec<String>, is_inc: bool, operand: &str) {
        let is_mem = operand.starts_with("memw(") || operand.starts_with("memb(");
        let opc = if is_inc { "+" } else { "-" };
        if is_mem {
            let (inner, acc, mask, sign, shift) = Self::mem_parts(operand);
            let limit = if is_inc {
                if acc == "memw" {
                    "0x7FFF"
                } else {
                    "0x7F"
                }
            } else {
                sign
            };
            lines.push("{".into());
            lines.push(format!("    uint32_t old = {acc}({inner});"));
            lines.push(format!("    {acc}_write({inner}, (old {opc} 1) & {mask});"));
            lines.push(format!("    ZF = {acc}({inner}) == 0;"));
            lines.push(format!("    PF = parity8((uint8_t){acc}({inner}));"));
            lines.push(format!("    SF = ({acc}({inner}) >> {shift}) & 1;"));
            lines.push(format!("    OF = old == {limit};"));
            lines.push("}".into());
        } else {
            let (mask, sign, shift) = Self::reg_parts(operand);
            let limit = if is_inc {
                if mask == "0xFF" {
                    "0x7F"
                } else {
                    "0x7FFF"
                }
            } else {
                sign
            };
            lines.push("{".into());
            lines.push(format!("    uint32_t old = {operand};"));
            lines.push(format!("    {operand} = ({operand} {opc} 1) & {mask};"));
            lines.push(format!("    ZF = {operand} == 0;"));
            lines.push(format!("    PF = parity8((uint8_t){operand});"));
            lines.push(format!("    SF = ({operand} >> {shift}) & 1;"));
            lines.push(format!("    OF = old == {limit};"));
            lines.push("}".into());
        }
    }

    /// handle_dos_interrupt_prep. Returns true if it deferred.
    fn handle_dos_interrupt_prep(
        &mut self,
        dest: &str,
        src: &str,
        lines: &mut Vec<String>,
    ) -> bool {
        const REGS: &[&str] = &[
            "ah", "al", "ax", "bh", "bl", "bx", "ch", "cl", "cx", "dh", "dl", "dx", "si", "di",
            "bp", "sp", "cs", "ds", "es", "ss",
        ];
        if REGS.contains(&src.to_lowercase().as_str()) {
            self.flush_pending_dos(lines, "");
            return false;
        }
        let val = match parse_hex16(src.trim_end_matches(['h', 'H'])) {
            Some(v) => v,
            None => {
                self.flush_pending_dos(lines, "");
                return false;
            }
        };
        let dest_l = dest.to_lowercase();
        let flush_prefixes: &[&str] = match dest_l.as_str() {
            "ax" => &["ax"],
            "ah" => &["ah", "ax"],
            "al" => &["al", "ax"],
            _ => &[],
        };
        if !flush_prefixes.is_empty()
            && self.pending_dos.iter().any(|line| {
                let ll = line.to_lowercase();
                flush_prefixes.iter().any(|p| ll.starts_with(p))
            })
        {
            self.flush_pending_dos(lines, "");
        }
        match dest_l.as_str() {
            "ax" => {
                self.last_ah = Some((val >> 8) & 0xFF);
                self.last_al = Some(val & 0xFF);
                self.pending_dos.push(format!("{dest} = {src};"));
                true
            }
            "ah" => {
                self.last_ah = Some(val);
                self.last_al = None;
                self.pending_dos.push(format!("{dest} = {src};"));
                true
            }
            "al" => {
                self.last_al = Some(val);
                self.pending_dos
                    .retain(|e| !e.to_lowercase().starts_with("al ="));
                self.pending_dos.push(format!("{dest} = {src};"));
                true
            }
            _ => false,
        }
    }

    /// handle_mov.
    fn handle_mov(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            self.flush_pending_dos(&mut lines, "");
            self.emit_unsupported_abort(insn, format!("mov {op_str}").trim());
        }
        let rw = self.rewrite_operands(insn, &parts);
        let dest = rw[0].clone();
        let mut src = rw[1].clone();

        if let Some((size, field)) = self.match_rcb_access(&dest) {
            let func = if size == "w" {
                "rcb_write16"
            } else {
                "rcb_write8"
            };
            lines.push(format!("{func}({field}, {src});"));
            return lines;
        }
        if let Some((size, field)) = self.match_rcb_access(&src) {
            let func = if size == "w" {
                "rcb_read16"
            } else {
                "rcb_read8"
            };
            lines.push(format!("{dest} = {func}({field});"));
            return lines;
        }

        if let Some(mut imm_value) = parse_imm(&src) {
            let size_bytes = isize_bytes(insn);
            if size_bytes >= 3 {
                let imm_offset = i64f(insn, "address").unwrap_or(0) + size_bytes - 2;
                if self.reloc_offsets.contains(&imm_offset) {
                    imm_value = (imm_value + self.load_segment) & 0xFFFF;
                    src = format!("0x{imm_value:04X}");
                }
            }
        }

        let dest_l = dest.to_lowercase();
        if dest_l == "ss" {
            self.flush_pending_dos(&mut lines, "");
            lines.push(format!("{dest} = {src};"));
            lines.push("interrupt_shadow = 1;".into());
            return lines;
        }
        if matches!(dest_l.as_str(), "ah" | "ax" | "al") {
            self.invalidate_register(&dest);
            if self.handle_dos_interrupt_prep(&dest, &src, &mut lines) {
                return lines;
            }
            self.flush_pending_dos(&mut lines, "");
            lines.push(format!("{dest} = {src};"));
            return lines;
        }
        if matches!(dest_l.as_str(), "dx" | "dl" | "bx") && parse_imm(&src).is_none() {
            self.flush_pending_dos(&mut lines, "");
            if dest_l == "dx" || dest_l == "dl" {
                self.last_dx = None;
                self.last_dl = None;
            } else {
                self.last_bx = None;
            }
            lines.push(format!("{dest} = {src};"));
            return lines;
        }
        if dest_l == "dx" {
            let val = parse_imm(&src);
            self.last_dx = val;
            self.last_dl = val.map(|v| v & 0xFF);
            self.pending_dos
                .retain(|e| !e.to_lowercase().starts_with("dx ="));
            self.pending_dos.push(format!("{dest} = {src};"));
            return lines;
        }
        if dest_l == "dl" {
            self.last_dx = None;
            self.last_dl = parse_imm(&src);
            self.pending_dos
                .retain(|e| !e.to_lowercase().starts_with("dl ="));
            self.pending_dos.push(format!("{dest} = {src};"));
            return lines;
        }
        if dest_l == "bx" {
            self.last_bx = parse_imm(&src);
            self.pending_dos
                .retain(|e| !e.to_lowercase().starts_with("bx ="));
            self.pending_dos.push(format!("{dest} = {src};"));
            return lines;
        }
        if dest_l == "cx" {
            self.flush_pending_dos(&mut lines, "");
            let val = parse_imm(&src);
            self.last_cx = val;
            self.last_cl = val.map(|v| v & 0xFF);
            self.last_ch = val.map(|v| (v >> 8) & 0xFF);
            lines.push(format!("{dest} = {src};"));
            return lines;
        }
        if dest_l == "cl" {
            self.flush_pending_dos(&mut lines, "");
            self.last_cx = None;
            self.last_ch = None;
            self.last_cl = parse_imm(&src);
            lines.push(format!("{dest} = {src};"));
            return lines;
        }
        if dest_l == "ch" {
            self.flush_pending_dos(&mut lines, "");
            self.last_cx = None;
            self.last_cl = None;
            self.last_ch = parse_imm(&src);
            lines.push(format!("{dest} = {src};"));
            return lines;
        }
        self.flush_pending_dos(&mut lines, "");
        if dest.starts_with("memw(") || dest.starts_with("memb(") {
            let inner = &dest[5..dest.len() - 1];
            let writer = if dest.starts_with("memw(") {
                "memw_write"
            } else {
                "memb_write"
            };
            lines.push(format!("{writer}({inner}, {src});"));
        } else {
            lines.push(format!("{dest} = {src};"));
        }
        lines
    }

    /// PCSwitchRenderer.handle_call.
    fn handle_call(&mut self, insn: &Insn, known_funcs: &BTreeSet<i64>) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let mut target = i64f(insn, "target");
        if let Some(t) = target {
            if s(insn, "bytes").len() == 6 && (t & 0xFFFF0000) == 0xFFFF0000 {
                target = Some(t & 0xFFFF);
            }
        }
        let ret_arg = self.call_return_arg(insn);
        if target.is_none() {
            target = parse_imm(s(insn, "op_str"));
        }
        if let Some(t) = target {
            if known_funcs.contains(&t)
                || self.func_names.contains_key(&t)
                || self.extern_labels.contains(&t)
            {
                self.emit_with_comment(&mut lines, insn, "{", "");
                lines.push("    sp = (sp - 2) & 0xFFFF;".into());
                lines.push(format!("    memw_write(ss, sp, {ret_arg});"));
                lines.push(format!("    pc = 0x{t:04X};"));
                lines.push("    continue;".into());
                lines.push("}".into());
            } else {
                let asm = format!("{} {}", s(insn, "mnemonic"), s(insn, "op_str"))
                    .trim()
                    .to_string();
                self.emit_unsupported_abort(insn, &asm);
            }
            return lines;
        }
        let op_str = decode_variables(s(insn, "op_str"));
        let op = op_str.to_lowercase();
        let op = op.trim();
        if WORD_REGS.contains(&op) {
            let expr = format!(
                "call_table({ret_arg}, (((uint32_t)cs << 4) + (uint16_t)({op})) & 0xFFFFF);"
            );
            self.emit_with_comment(&mut lines, insn, &expr, "");
            return lines;
        }
        if mem_ptr_re().is_match(op) {
            let seg = insn
                .get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|m| m.get("segment"))
                .and_then(Value::as_str)
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "ds".into());
            let mem_expr = rewrite_mem_op(op, Some(&seg));
            let expr = format!(
                "call_table({ret_arg}, (((uint32_t)cs << 4) + (uint16_t)({mem_expr})) & 0xFFFFF);"
            );
            self.emit_with_comment(&mut lines, insn, &expr, "");
            return lines;
        }
        let asm = format!("{} {op_str}", s(insn, "mnemonic"))
            .trim()
            .to_string();
        self.emit_unsupported_abort(insn, &asm);
    }

    /// _string_source_segment.
    fn string_source_segment(&self, insn: &Insn) -> String {
        if let Some(refs) = insn
            .get("detail")
            .and_then(|d| d.get("mem_refs"))
            .and_then(Value::as_array)
        {
            for r in refs {
                let access = r
                    .get("access")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                if access != "read" && access != "readwrite" {
                    continue;
                }
                if let Some(seg) = r.get("segment").and_then(Value::as_str) {
                    if !seg.is_empty() {
                        return seg.to_lowercase();
                    }
                }
            }
        }
        "ds".into()
    }

    fn h_lods(&mut self, insn: &Insn, w: i64, reg: &str, m: &str) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let seg = self.string_source_segment(insn);
        let d = if w == 1 { "-1 : 1" } else { "-2 : 2" };
        lines.push("{".into());
        lines.push(format!("int delta = DF ? {d};"));
        lines.push(format!("{reg} = {m}({seg}, si);"));
        lines.push("si = (si + delta) & 0xFFFF;".into());
        lines.push("}".into());
        lines
    }
    fn h_xlatb(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let mut seg = "ds".to_string();
        if let Some(so) = insn
            .get("detail")
            .and_then(|d| d.get("seg_override"))
            .and_then(Value::as_str)
        {
            if !so.is_empty() {
                seg = so.to_lowercase();
            }
        }
        lines.push(format!("al = memb({seg}, (bx + al) & 0xFFFF);"));
        lines
    }
    fn h_movs(&mut self, insn: &Insn, w: i64, m: &str) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let seg = self.string_source_segment(insn);
        let d = if w == 1 { "-1 : 1" } else { "-2 : 2" };
        lines.push("{".into());
        lines.push(format!("int delta = DF ? {d};"));
        lines.push(format!("{m}_write(es, di, {m}({seg}, si));"));
        lines.push("si = (si + delta) & 0xFFFF;".into());
        lines.push("di = (di + delta) & 0xFFFF;".into());
        lines.push("}".into());
        lines
    }
    fn h_cmpsb(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let seg = self.string_source_segment(insn);
        lines.push("int delta = DF ? -1 : 1;".into());
        lines.push("{".into());
        lines.push(format!("    uint32_t left_val = memb({seg}, si);"));
        for l in [
            "    uint32_t right_val = memb(es, di);",
            "    CF = left_val < right_val;",
            "    uint32_t tmp = left_val - right_val;",
            "    uint8_t result = (uint8_t)tmp;",
            "    ZF = result == 0;",
            "    SF = (result >> 7) & 1;",
            "    OF = ((left_val ^ right_val) & (left_val ^ result) & 0x80) != 0;",
            "}",
            "si = (si + delta) & 0xFFFF;",
            "di = (di + delta) & 0xFFFF;",
        ] {
            lines.push(l.into());
        }
        lines
    }
    fn h_scasb(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        for l in [
            "int delta = DF ? -1 : 1;",
            "{",
            "    uint32_t left_val = al;",
            "    uint32_t right_val = memb(es, di);",
            "    CF = left_val < right_val;",
            "    uint32_t tmp = left_val - right_val;",
            "    uint8_t result = (uint8_t)tmp;",
            "    ZF = result == 0;",
            "    SF = (result >> 7) & 1;",
            "    OF = ((left_val ^ right_val) & (left_val ^ result) & 0x80) != 0;",
            "}",
            "di = (di + delta) & 0xFFFF;",
        ] {
            lines.push(l.into());
        }
        lines
    }
    fn h_scasw(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        for l in [
            "int delta = DF ? -2 : 2;",
            "{",
            "    uint32_t left_val = ax;",
            "    uint32_t right_val = memw(es, di);",
            "    CF = left_val < right_val;",
            "    uint32_t tmp = left_val - right_val;",
            "    uint16_t result = (uint16_t)tmp;",
            "    ZF = result == 0;",
            "    SF = (result >> 15) & 1;",
            "    PF = parity8((uint8_t)result);",
            "    OF = ((left_val ^ right_val) & (left_val ^ result) & 0x8000) != 0;",
            "}",
            "di = (di + delta) & 0xFFFF;",
        ] {
            lines.push(l.into());
        }
        lines
    }
    fn h_stos(&mut self, w: i64, reg: &str, m: &str) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let d = if w == 1 { "-1 : 1" } else { "-2 : 2" };
        lines.push("{".into());
        lines.push(format!("int delta = DF ? {d};"));
        lines.push(format!("{m}_write(es, di, {reg});"));
        lines.push("di = (di + delta) & 0xFFFF;".into());
        lines.push("}".into());
        lines
    }
    fn h_rep(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let seg = self.string_source_segment(insn);
        let op_str = decode_variables(s(insn, "op_str"));
        if op_str.starts_with("movsb") {
            lines.push(format!("rep_movsb_block(es, {seg});"));
        } else if op_str.starts_with("movsw") {
            lines.push(format!("rep_movsw_block(es, {seg});"));
        } else if op_str.starts_with("stosb") {
            lines.push("rep_stosb_block(es);".into());
        } else if op_str.starts_with("stosw") {
            for l in [
                "{",
                "    int delta = DF ? -2 : 2;",
                "    for (; cx; cx--) {",
                "        memw_write(es, di, ax);",
                "        di = (di + delta) & 0xFFFF;",
                "    }",
                "    cx = 0;",
                "}",
            ] {
                lines.push(l.into());
            }
        } else if op_str.starts_with("lodsb") {
            lines.push("if (cx) {".into());
            lines.push("    int delta = DF ? -1 : 1;".into());
            lines.push(format!("    al = memb({seg}, si + (cx - 1) * delta);"));
            lines.push("    si = (si + cx * delta) & 0xFFFF;".into());
            lines.push("    cx = 0;".into());
            lines.push("}".into());
        } else {
            self.emit_unsupported_abort(insn, format!("rep {op_str}").trim());
        }
        lines
    }
    fn h_repe_cmpsb(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let seg = self.string_source_segment(insn);
        let op_str = decode_variables(s(insn, "op_str"));
        if op_str.starts_with("cmpsb") {
            lines.push("{".into());
            lines.push("    int delta = DF ? -1 : 1;".into());
            lines.push("    uint16_t count = cx, i = 0;".into());
            lines.push("    uint8_t lb = 0, rb = 0;".into());
            lines.push("    while (i < count) {".into());
            lines.push(format!("        lb = memb({seg}, si);"));
            for l in [
                "        rb = memb(es, di);",
                "        si = (si + delta) & 0xFFFF;",
                "        di = (di + delta) & 0xFFFF;",
                "        ++i;",
                "        if (lb != rb) break;  /* ZF->0 ends repe */",
                "    }",
                "    cx = (count - i) & 0xFFFF;",
                "    if (i > 0) {",
                "        uint32_t t = (uint32_t)lb - (uint32_t)rb;",
                "        uint8_t res = (uint8_t)t;",
                "        CF = lb < rb;",
                "        ZF = res == 0;",
                "        PF = parity8(res);",
                "        SF = (res >> 7) & 1;",
                "        OF = ((lb ^ rb) & (lb ^ res) & 0x80) != 0;",
                "    }",
                "}",
            ] {
                lines.push(l.into());
            }
            return lines;
        }
        if op_str.starts_with("cmpsw") {
            lines.push("{".into());
            lines.push("    int delta = DF ? -2 : 2;".into());
            lines.push("    uint16_t count = cx, i = 0, lw = 0, rw = 0;".into());
            lines.push("    while (i < count) {".into());
            lines.push(format!("        lw = memw({seg}, si);"));
            for l in [
                "        rw = memw(es, di);",
                "        si = (si + delta) & 0xFFFF;",
                "        di = (di + delta) & 0xFFFF;",
                "        ++i;",
                "        if (lw != rw) break;  /* ZF->0 ends repe */",
                "    }",
                "    cx = (count - i) & 0xFFFF;",
                "    if (i > 0) {",
                "        uint32_t t = (uint32_t)lw - (uint32_t)rw;",
                "        uint16_t res = (uint16_t)t;",
                "        CF = lw < rw;",
                "        ZF = res == 0;",
                "        PF = parity8((uint8_t)res);",
                "        SF = (res >> 15) & 1;",
                "        OF = ((lw ^ rw) & (lw ^ res) & 0x8000) != 0;",
                "    }",
                "}",
            ] {
                lines.push(l.into());
            }
            return lines;
        }
        if op_str.starts_with("scasb") {
            lines.push("{".into());
            lines.push("    int delta = DF ? -1 : 1;".into());
            lines.push("    uint16_t count = cx, i = 0;".into());
            lines.push("    uint8_t last_byte = 0;".into());
            lines.push("    while (i < count) {".into());
            for l in [
                "        last_byte = memb(es, di);",
                "        di = (di + delta) & 0xFFFF;",
                "        ++i;",
                "        if (al != last_byte) break;  /* ZF->0 ends repe */",
                "    }",
                "    cx = (count - i) & 0xFFFF;",
                "    if (i > 0) {",
                "        uint32_t l = al, r = last_byte, t = l - r;",
                "        uint8_t res = (uint8_t)t;",
                "        CF = l < r;",
                "        ZF = res == 0;",
                "        PF = parity8(res);",
                "        SF = (res >> 7) & 1;",
                "        OF = ((l ^ r) & (l ^ res) & 0x80) != 0;",
                "    }",
                "}",
            ] {
                lines.push(l.into());
            }
            return lines;
        }
        self.emit_unsupported_abort(insn, format!("{} {op_str}", s(insn, "mnemonic")).trim());
    }
    fn h_repne_scasb(&mut self, insn: &Insn) -> Vec<String> {
        let mut lines = Vec::new();
        self.flush_pending_dos(&mut lines, "");
        let op_str = decode_variables(s(insn, "op_str"));
        if op_str.starts_with("scasb") {
            for l in [
                "{",
                "int delta = DF ? -1 : 1;",
                "uint16_t count = cx;",
                "uint8_t last_byte = 0;",
                "uint16_t index = scanMemoryForAl((const uint8_t *)seg_off(es, di), al, count, delta, &last_byte);",
                "uint16_t advance = (index < count) ? index + 1 : index;",
                "if (advance > 0) {",
                "    uint32_t left_val = al;",
                "    uint32_t right_val = last_byte;",
                "    uint32_t tmp = left_val - right_val;",
                "    uint8_t result = (uint8_t)tmp;",
                "    CF = left_val < right_val;",
                "    ZF = result == 0;",
                "    PF = parity8(result);",
                "    SF = (result >> 7) & 1;",
                "    OF = ((left_val ^ right_val) & (left_val ^ result) & 0x80) != 0;",
                "}",
                "cx = (count - advance) & 0xFFFF;",
                "di = (di + advance * delta) & 0xFFFF;",
                "}",
            ] {
                lines.push(l.into());
            }
            return lines;
        }
        self.emit_unsupported_abort(insn, format!("{} {op_str}", s(insn, "mnemonic")).trim());
    }

    // ---- PCSwitchRenderer state machine ----

    /// PCSwitchRenderer._try_match_swap_loop_w.
    fn try_match_swap_loop_w(&self, block: &BasicBlock) -> Option<Vec<String>> {
        let insns = &block.instructions;
        let norm = |i: usize| s(&insns[i], "op_str").to_lowercase().replace(' ', "");
        let (form_name, loop_insn, start_insn): (&str, &Insn, &Insn) = if insns.len() == 5 {
            let op1 = norm(1);
            let op3 = norm(3);
            let ok = s(&insns[0], "mnemonic") == "lodsw"
                && s(&insns[1], "mnemonic") == "mov"
                && op1.contains("es:[di]")
                && op1.starts_with("dx,")
                && s(&insns[2], "mnemonic") == "stosw"
                && s(&insns[3], "mnemonic") == "mov"
                && op3.contains("[si-2]")
                && op3.ends_with(",dx")
                && s(&insns[4], "mnemonic") == "loop";
            if !ok {
                return None;
            }
            (
                "lodsw/mov-dx,es:[di]/stosw/mov-[si-2],dx/loop",
                &insns[4],
                &insns[0],
            )
        } else if insns.len() == 4 {
            let op0 = norm(0);
            let op2 = norm(2);
            let ok = s(&insns[0], "mnemonic") == "mov"
                && op0.starts_with("ax,")
                && op0.contains("es:[di]")
                && s(&insns[1], "mnemonic") == "movsw"
                && s(&insns[2], "mnemonic") == "mov"
                && op2.contains("[si-2]")
                && op2.ends_with(",ax")
                && s(&insns[3], "mnemonic") == "loop";
            if !ok {
                return None;
            }
            (
                "mov-ax,es:[di]/movsw/mov-[si-2],ax/loop",
                &insns[3],
                &insns[0],
            )
        } else {
            return None;
        };
        let loop_target =
            i64::from_str_radix(s(loop_insn, "op_str").trim().trim_start_matches("0x"), 16).ok()?;
        if loop_target != block.start {
            return None;
        }
        let loop_len = (s(loop_insn, "bytes").len() / 2) as i64;
        let succ_addr = (i64f(loop_insn, "address").unwrap_or(0) + loop_len) & 0xFFFF;
        let indent = "            ";
        let mut lines = Vec::new();
        for c in self.comment_lines(i64f(start_insn, "address").unwrap_or(0)) {
            lines.push(format!("{indent}{c}"));
        }
        lines.push(format!(
            "{indent}// Swap-loop pattern ({form_name}) -> shim_swap_regions_w"
        ));
        lines.push(format!(
            "{indent}ip = 0x{:04X};",
            (i64f(start_insn, "address").unwrap_or(0) + self.cs_base) & 0xFFFF
        ));
        lines.push(format!("{indent}SAFEPOINT();"));
        lines.push(format!(
            "{indent}shim_swap_regions_w(es, di, ds, si, cx, DF);"
        ));
        lines.push(format!("{indent}{{"));
        lines.push(format!("{indent}    int _swap_delta = DF ? -2 : 2;"));
        lines.push(format!(
            "{indent}    si = (uint16_t)((int)si + _swap_delta * (int)cx);"
        ));
        lines.push(format!(
            "{indent}    di = (uint16_t)((int)di + _swap_delta * (int)cx);"
        ));
        lines.push(format!("{indent}    cx = 0;"));
        lines.push(format!("{indent}    ZF = 1;"));
        lines.push(format!("{indent}}}"));
        lines.push(format!("{indent}pc = 0x{succ_addr:04X};"));
        lines.push(format!("{indent}continue;"));
        Some(lines)
    }

    fn render_block_state_machine(
        &mut self,
        block: &BasicBlock,
        succ: &HashMap<i64, Vec<i64>>,
        known_funcs: &BTreeSet<i64>,
    ) -> Vec<String> {
        if let Some(swap) = self.try_match_swap_loop_w(block) {
            return swap;
        }
        let indent = "            ";
        let mut lines: Vec<String> = Vec::new();
        let insns = &block.instructions;
        let n = insns.len();
        let empty = Vec::new();
        let succs = |a: i64| succ.get(&a).unwrap_or(&empty);
        for (idx, insn) in insns.iter().enumerate() {
            for c in self.comment_lines(i64f(insn, "address").unwrap_or(0)) {
                lines.push(format!("{indent}{c}"));
            }
            let ip = (i64f(insn, "address").unwrap_or(0) + self.cs_base) & 0xFFFF;
            lines.push(format!("{indent}ip = 0x{ip:04X};"));
            lines.push(format!("{indent}SAFEPOINT();"));

            let is_last = idx == n - 1;
            let mnem = s(insn, "mnemonic").to_string();
            let op_str = decode_variables(s(insn, "op_str"));
            let op = op_str.to_lowercase();
            let op = op.trim();

            if is_last && insn.get("op").and_then(Value::as_str) == Some("INDIRECT_NEAR_JMP") {
                self.flush_pending_dos(&mut lines, indent);
                let (_, expr) = self.indirect_jump_target(insn);
                self.emit_with_comment(
                    &mut lines,
                    insn,
                    &format!("jump_table((((uint32_t)cs << 4) + (uint16_t)({expr})) & 0xFFFFF, expected_retip);"),
                    indent,
                );
                lines.push(format!("{indent}return;"));
                continue;
            }
            if is_last
                && mnem == "jmp"
                && ["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"].contains(&op)
            {
                self.flush_pending_dos(&mut lines, indent);
                self.emit_with_comment(
                    &mut lines,
                    insn,
                    &format!("jump_table((((uint32_t)cs << 4) + (uint16_t)({op})) & 0xFFFFF, expected_retip);"),
                    indent,
                );
                lines.push(format!("{indent}return;"));
                continue;
            }
            if is_last && mnem == "jmp" {
                self.flush_pending_dos(&mut lines, indent);
                // the reference state machine uses int(op_str, 16) (hex) here, NOT
                // _parse_imm — a bare "0109" is 0x109, not decimal 109.
                match parse_hex16(&op_str) {
                    Some(target) => {
                        lines.push(format!("{indent}pc = 0x{target:04X};"));
                        lines.push(format!("{indent}continue;"));
                    }
                    None => {
                        self.emit_unsupported_abort(insn, &format!("jmp {op_str}"));
                    }
                }
                continue;
            }
            if is_last && mnem.starts_with('j') && mnem != "jmp" {
                self.flush_pending_dos(&mut lines, indent);
                let target = match parse_hex16(&op_str) {
                    Some(t) => t,
                    None => self.emit_unsupported_abort(insn, &format!("{mnem} {op_str}")),
                };
                let cond = jcc_condition(
                    &mnem,
                    insn.get("cond_prev").and_then(|v| v.as_object()),
                    i64f(insn, "address"),
                );
                let ss = succs(block.start);
                let fall = ss.iter().find(|&&x| x != target).cloned();
                match fall {
                    Some(f) => {
                        lines.push(format!("{indent}if ({cond}) {{"));
                        lines.push(format!("{indent}    pc = 0x{target:04X};"));
                        lines.push(format!("{indent}}} else {{"));
                        lines.push(format!("{indent}    pc = 0x{f:04X};"));
                        lines.push(format!("{indent}}}"));
                        lines.push(format!("{indent}continue;"));
                    }
                    None => {
                        lines.push(format!("{indent}if ({cond}) {{"));
                        lines.push(format!("{indent}    pc = 0x{target:04X};"));
                        lines.push(format!("{indent}    continue;"));
                        lines.push(format!("{indent}}}"));
                        lines.push(format!("{indent}return;"));
                    }
                }
                continue;
            }
            if is_last && matches!(mnem.as_str(), "ret" | "retn" | "retf") {
                self.flush_pending_dos(&mut lines, indent);
                let formatted = self.format_instruction(insn, known_funcs);
                let mut last_code = String::new();
                for code in &formatted {
                    lines.push(format!("{indent}{code}"));
                    last_code = code.trim().to_string();
                }
                if last_code.is_empty() || !last_code.starts_with("return") {
                    lines.push(format!("{indent}return;"));
                }
                continue;
            }

            let formatted = self.format_instruction(insn, known_funcs);
            let mut last_code = String::new();
            for code in &formatted {
                lines.push(format!("{indent}{code}"));
                last_code = code.trim().to_string();
            }
            if !last_code.is_empty() && self.terminates(&last_code) {
                if !last_code.starts_with("return") {
                    lines.push(format!("{indent}return;"));
                }
                break;
            }

            if is_last {
                self.flush_pending_dos(&mut lines, indent);
                if last_code == "return;" || last_code == "continue;" {
                    // pass
                } else {
                    let ss = succs(block.start);
                    if let Some(&first) = ss.first() {
                        lines.push(format!("{indent}pc = 0x{first:04X};"));
                        lines.push(format!("{indent}continue;"));
                    } else {
                        lines.push(format!("{indent}return;"));
                    }
                }
            }
        }
        self.flush_pending_dos(&mut lines, indent);
        lines
    }

    /// PCSwitchRenderer.render_function (the JIT dispatch-switch path). `pub` so
    /// the ported test suite can drive it (the source etc.).
    pub fn render_function(&mut self, func: &Value, known_funcs: &BTreeSet<i64>) -> Vec<String> {
        let start = func.get("start").and_then(Value::as_i64).unwrap_or(0);
        let raw: Vec<Insn> = func
            .get("instructions")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_object().cloned()).collect())
            .unwrap_or_default();
        let instrs = normalize_flags(&raw);
        let instrs = normalize_indirect_jumps(&instrs);
        let blocks = build_basic_blocks(
            &instrs,
            &{
                let mut s = BTreeSet::new();
                s.insert(start);
                s
            },
            Some(start),
        );
        let succ = cfg_successors(&blocks);

        let name = self.func_name(start);
        self.current_func_name = name.clone();
        let impl_ = format!("{name}_impl");

        let block_addrs: Vec<i64> = blocks.keys().cloned().collect();
        let mut cases: BTreeSet<i64> = block_addrs.iter().cloned().collect();
        cases.insert(start);
        let first_real = block_addrs.first().cloned().unwrap_or(start);

        let indent = "            ";
        for addr in cases {
            if self.seen_cases.contains(&addr) {
                continue;
            }
            self.seen_cases.insert(addr);
            self.dispatch_cases.push(format!(
                "        case 0x{addr:04X}: {{ /* {impl_}@{addr:04X} */"
            ));
            match blocks.get(&addr) {
                None => {
                    if !block_addrs.is_empty() {
                        self.dispatch_cases
                            .push(format!("{indent}pc = 0x{first_real:04X};"));
                        self.dispatch_cases.push(format!("{indent}continue;"));
                    } else {
                        self.dispatch_cases.push(format!("{indent}return;"));
                    }
                }
                Some(block) => {
                    let body = self.render_block_state_machine(block, &succ, known_funcs);
                    self.dispatch_cases.extend(body);
                }
            }
            self.dispatch_cases.push("        }".into());
            self.dispatch_cases.push("".into());
        }

        let binary_name = self.prefix.trim_end_matches('_').to_string();
        let params = "uint16_t expected_retip, const char *file, const char *func, int line";
        let mut lines = vec![format!("void {impl_}({params}) {{")];
        lines.push(format!("    shim_enter_binary(\"{binary_name}\");"));
        lines.push("    ShimTailDispatchState saved_tail;".into());
        lines.push("    shim_tail_dispatch_save(&saved_tail);".into());
        lines.push(format!(
            "    {}(0x{start:04X}, expected_retip, file, func, line);",
            self.dispatch_name()
        ));
        lines.push("    shim_drain_pending_tail_dispatch(file, func, line);".into());
        lines.push("    shim_tail_dispatch_restore(&saved_tail);".into());
        lines.push("    shim_leave_binary();".into());
        lines.push("}".into());
        lines
    }

    /// CCodeRenderer.render_function — the BASE structured
    /// (readable-C) renderer. Distinct from the JIT `render_function` above
    /// (PCSwitchRenderer override). Exercised by the ~100 structured-renderer
    /// tests. Returns the `void <name>_impl(...)` body lines; extern-label
    /// regions are lifted into `self.extra_func_blocks`.
    pub fn render_function_c(&mut self, func: &Value, known_funcs: &BTreeSet<i64>) -> Vec<String> {
        self.last_ah = None;
        self.last_al = None;
        self.last_dx = None;
        self.last_bx = None;
        self.last_dl = None;
        self.pending_dos.clear();
        let start = func.get("start").and_then(Value::as_i64).unwrap_or(0);
        self.current_func_name = self.func_name(start);
        // Threaded known set (the reference passes one mutable object through structure).
        let mut known = known_funcs.clone();

        let raw: Vec<Insn> = func
            .get("instructions")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_object().cloned()).collect())
            .unwrap_or_default();
        let instrs = normalize_flags(&raw);
        let instrs = normalize_indirect_jumps(&instrs);
        self.current_func_all_addrs = instrs.iter().filter_map(|i| i64f(i, "address")).collect();
        self.current_func_addrs = self.current_func_all_addrs.clone();
        let label_addrs: BTreeSet<i64> = instrs
            .iter()
            .filter_map(|i| i64f(i, "address"))
            .filter(|a| {
                self.extern_labels.contains(a)
                    || (self.func_names.contains_key(a) && !known.contains(a))
            })
            .collect();

        let mut blocks = build_basic_blocks(&instrs, &label_addrs, Some(start));
        let graph0 = crate::cfg::build_cfg(&blocks);
        propagate_flag_conditions(&mut blocks, &graph0);
        for block in blocks.values_mut() {
            block
                .instructions
                .retain(|i| i.get("skip").map_or(true, is_falsy));
        }
        self.current_func_addrs = blocks
            .values()
            .flat_map(|b| b.instructions.iter())
            .filter_map(|i| i64f(i, "address"))
            .collect();
        let to_pop: Vec<i64> = self
            .func_names
            .iter()
            .filter(|(a, n)| !self.current_func_addrs.contains(a) && n.starts_with("label_"))
            .map(|(&a, _)| a)
            .collect();
        for a in to_pop {
            self.func_names.remove(&a);
        }
        let mut graph = crate::cfg::build_cfg(&blocks);
        let indent = "    ";

        let mut label_sorted: Vec<i64> = label_addrs.iter().copied().collect();
        label_sorted.sort_unstable();
        for &addr in &label_sorted {
            let mut others: IndexSet<i64> = label_addrs.iter().copied().collect();
            others.shift_remove(&addr);
            let mut region = crate::cfg::collect_branch(&graph, addr, None, &others);
            region.retain(|&a| a >= addr);
            let mut region_blocks: BTreeMap<i64, BasicBlock> = BTreeMap::new();
            for &a in &region {
                if let Some(b) = blocks.remove(&a) {
                    region_blocks.insert(a, b);
                }
            }
            if region_blocks.is_empty() {
                continue;
            }
            let region_graph = crate::cfg::build_cfg(&region_blocks);
            propagate_flag_conditions(&mut region_blocks, &region_graph);
            let region_graph = crate::cfg::build_cfg(&region_blocks);
            let prev_blocks = std::mem::replace(&mut self.current_blocks, region_blocks.clone());
            let nodes = self.structure(
                &region_blocks,
                &region_graph,
                &mut known,
                addr,
                &IndexSet::new(),
            );
            let mut lines: Vec<String> = Vec::new();
            for node in &nodes {
                lines.extend(node.render(self, indent, &known));
            }
            if !self.pending_dos.is_empty() {
                let dos: Vec<String> = std::mem::take(&mut self.pending_dos);
                lines.extend(dos);
            }
            // Drop duplicate label lines (region entry + repeats).
            let name = self.func_name(addr);
            let label_line = format!("{name}:");
            let mut seen_labels: BTreeSet<String> = BTreeSet::new();
            let mut filtered: Vec<String> = Vec::new();
            for line in lines {
                let stripped = line.trim().to_string();
                if stripped.ends_with(':') {
                    if stripped == label_line {
                        continue;
                    }
                    if seen_labels.contains(&stripped) {
                        continue;
                    }
                    seen_labels.insert(stripped);
                }
                filtered.push(line);
            }
            let mut lines = self.rewrite_gotos(filtered, &seen_labels);
            if addr == 0x0B01 {
                if let Some(i) = lines.iter().position(|c| c.contains("pc =")) {
                    lines.insert(
                        i,
                        "    file_mapping_swap(cs + 0x2000, 0x9000, es, 0x3000, 0x7000);".into(),
                    );
                }
            }
            self.extra_func_blocks.insert(addr, lines);
            graph.remove_nodes_from(&region.iter().copied().collect::<Vec<_>>());
            self.last_ah = None;
            self.last_al = None;
            self.last_dx = None;
            self.last_bx = None;
            self.last_dl = None;
            self.pending_dos.clear();
            self.current_blocks = prev_blocks;
        }

        self.current_func_addrs = blocks
            .values()
            .flat_map(|b| b.instructions.iter())
            .filter_map(|i| i64f(i, "address"))
            .collect();
        if !label_addrs.is_empty() {
            graph = crate::cfg::build_cfg(&blocks);
            propagate_flag_conditions(&mut blocks, &graph);
        }

        let params = "const char *file, const char *func, int line";
        let reachable: BTreeSet<i64> = if graph.contains(start) {
            let mut r = crate::cfg::collect_branch(&graph, start, None, &IndexSet::new());
            r.insert(start);
            r.into_iter().collect()
        } else {
            BTreeSet::new()
        };
        self.current_blocks = reachable
            .iter()
            .filter_map(|a| blocks.get(a).map(|b| (*a, b.clone())))
            .collect();
        let mut nodes = self.structure(&blocks, &graph, &mut known, start, &IndexSet::new());
        self.rewrite_returning_do_while(&mut nodes);
        self.merge_adjacent_empty_if(&mut nodes);

        let orig_name = format!("func_{start:04X}");
        let name = self.func_name(start);
        let impl_ = format!("{name}_impl");
        let mut lines: Vec<String> = if name != orig_name {
            vec![
                format!("// {orig_name}"),
                format!("void {impl_}({params}) {{"),
            ]
        } else {
            vec![format!("void {impl_}({params}) {{")]
        };
        lines.push(format!(
            "{indent}shim_log(__func__, file, func, line, NULL);"
        ));
        if nodes
            .first()
            .map_or(true, |n| !matches!(n, AstNode::BasicBlock { .. }))
        {
            lines.push(format!("{indent}SAFEPOINT();"));
        }
        for node in &nodes {
            lines.extend(node.render(self, indent, &known));
        }
        if !self.pending_dos.is_empty() {
            let dos: Vec<String> = std::mem::take(&mut self.pending_dos);
            lines.extend(dos);
        }
        let seen_labels: BTreeSet<String> = lines
            .iter()
            .map(|l| l.trim().to_string())
            .filter(|l| l.ends_with(':'))
            .collect();
        let mut lines = self.rewrite_gotos(lines, &seen_labels);

        let terminal_return = nodes.last().map_or(false, node_returns);
        let last = if terminal_return {
            "return;".to_string()
        } else {
            lines
                .iter()
                .rev()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string()
        };
        if !self.terminates(&last) {
            if let Some(li) = instrs.last() {
                if matches!(s(li, "mnemonic"), "ret" | "retn" | "retf") {
                    if instrs.len() >= 2 {
                        let prev = &instrs[instrs.len() - 2];
                        if s(prev, "mnemonic") == "pop" && s(prev, "op_str").to_lowercase() == "bp"
                        {
                            lines.push(format!("{indent}bp = memw(ss, sp);"));
                            lines.push(format!("{indent}sp = (sp + 2) & 0xFFFF;"));
                        }
                    }
                    lines.push(format!("{indent}return;"));
                }
            }
        }
        lines.push("}".into());
        self.current_func_addrs = BTreeSet::new();
        self.current_blocks = BTreeMap::new();
        lines
    }

    /// Shared goto-rewrite pass (ir_to_c-5480 and 5539-5576): turn
    /// `goto label_XXXX;` / `if (c) goto label_XXXX;` targeting an undefined
    /// label into an in-switch `pc = …; continue;` or a `name();return;` call.
    fn rewrite_gotos(&self, lines: Vec<String>, seen_labels: &BTreeSet<String>) -> Vec<String> {
        static GOTO_RE: OnceLock<Regex> = OnceLock::new();
        static IF_GOTO_RE: OnceLock<Regex> = OnceLock::new();
        let goto_re = GOTO_RE.get_or_init(|| Regex::new(r"^(\s*)goto ([A-Za-z0-9_]+);").unwrap());
        let if_goto_re = IF_GOTO_RE
            .get_or_init(|| Regex::new(r"^(\s*)if \((.+)\) goto ([A-Za-z0-9_]+);").unwrap());
        let hex_after = |lbl: &str| -> Option<i64> {
            let part = lbl.rsplit_once('_').map(|(_, b)| b)?;
            i64::from_str_radix(part, 16).ok()
        };
        let mut out: Vec<String> = Vec::new();
        for line in lines {
            if let Some(m) = goto_re.captures(&line) {
                let lbl = m.get(2).unwrap().as_str();
                if !seen_labels.contains(&format!("{lbl}:")) {
                    match hex_after(lbl) {
                        Some(target) => {
                            let ind = m.get(1).unwrap().as_str().to_string();
                            if self.current_blocks.contains_key(&target) {
                                out.push(format!("{ind}pc = 0x{target:04X};"));
                                out.push(format!("{ind}continue;"));
                            } else {
                                let name = self.func_name(target);
                                out.push(format!("{ind}{name}();"));
                                out.push(format!("{ind}return;"));
                            }
                        }
                        None => out.push(line.clone()),
                    }
                    continue;
                }
            }
            if let Some(m) = if_goto_re.captures(&line) {
                let lbl = m.get(3).unwrap().as_str();
                if !seen_labels.contains(&format!("{lbl}:")) {
                    match hex_after(lbl) {
                        Some(target) => {
                            let ind = m.get(1).unwrap().as_str().to_string();
                            let cond = m.get(2).unwrap().as_str();
                            if self.current_blocks.contains_key(&target) {
                                out.push(format!("{ind}if ({cond}) {{"));
                                out.push(format!("{ind}    pc = 0x{target:04X};"));
                                out.push(format!("{ind}    continue;"));
                                out.push(format!("{ind}}}"));
                            } else {
                                let name = self.func_name(target);
                                out.push(format!("{ind}if ({cond}) {{"));
                                out.push(format!("{ind}    {name}();"));
                                out.push(format!("{ind}    return;"));
                                out.push(format!("{ind}}}"));
                            }
                        }
                        None => out.push(line.clone()),
                    }
                    continue;
                }
            }
            out.push(line);
        }
        out
    }

    /// _merge_adjacent_empty_if.
    fn merge_adjacent_empty_if(&self, nodes: &mut Vec<AstNode>) {
        let mut i = 0;
        while i < nodes.len() {
            match &mut nodes[i] {
                AstNode::IfElse {
                    then_body,
                    else_body,
                    ..
                } => {
                    self.merge_adjacent_empty_if(then_body);
                    if let Some(eb) = else_body {
                        self.merge_adjacent_empty_if(eb);
                    }
                }
                AstNode::ForLoop { body, .. }
                | AstNode::Loop { body, .. }
                | AstNode::DoWhile { body, .. } => self.merge_adjacent_empty_if(body),
                AstNode::Switch { cases, .. } => {
                    for (_v, b) in cases.iter_mut() {
                        if let crate::ast::CaseBody::Nodes(bn) = b {
                            self.merge_adjacent_empty_if(bn);
                        }
                    }
                }
                _ => {}
            }
            let is_empty_if = matches!(&nodes[i],
                AstNode::IfElse { then_body, else_body, .. } if then_body.is_empty() && else_body.is_none());
            if i < nodes.len() - 1 && is_empty_if {
                let first_opt = if let AstNode::BasicBlock { instructions, .. } = &nodes[i + 1] {
                    instructions.first().and_then(|f| {
                        let op = s(f, "op_str").replace(' ', "");
                        if s(f, "mnemonic") == "mov" && (op == "al,1" || op == "al,0x1") {
                            Some(f.clone())
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                if let Some(first) = first_opt {
                    let faddr = i64f(&first, "address").unwrap_or(0);
                    if let AstNode::IfElse { then_body, .. } = &mut nodes[i] {
                        *then_body = vec![AstNode::BasicBlock {
                            start: faddr,
                            instructions: vec![first],
                        }];
                    }
                    let emptied =
                        if let AstNode::BasicBlock { instructions, .. } = &mut nodes[i + 1] {
                            instructions.remove(0);
                            instructions.is_empty()
                        } else {
                            false
                        };
                    if emptied {
                        nodes.remove(i + 1);
                        continue;
                    }
                }
            }
            i += 1;
        }
    }

    /// _rewrite_returning_do_while.
    fn rewrite_returning_do_while(&self, nodes: &mut Vec<AstNode>) {
        let mut i = 0;
        while i < nodes.len() {
            match &mut nodes[i] {
                AstNode::IfElse {
                    then_body,
                    else_body,
                    ..
                } => {
                    self.rewrite_returning_do_while(then_body);
                    if let Some(eb) = else_body {
                        self.rewrite_returning_do_while(eb);
                    }
                }
                AstNode::DoWhile { body, .. } => {
                    self.rewrite_returning_do_while(body);
                    let is_ret = body.last().map_or(false, |tail| match tail {
                        AstNode::Return { .. } => true,
                        AstNode::BasicBlock { instructions, .. } => {
                            instructions.last().map_or(false, |li| {
                                matches!(s(li, "mnemonic"), "ret" | "retn" | "retf")
                            })
                        }
                        _ => false,
                    });
                    if is_ret {
                        let mut tail_nodes = nodes.split_off(i);
                        let dw = tail_nodes.remove(0);
                        let else_candidates = tail_nodes;
                        if let AstNode::DoWhile {
                            header,
                            cond_mnem,
                            body,
                            prev,
                            negate,
                            ..
                        } = dw
                        {
                            let else_body = if else_candidates.is_empty() {
                                None
                            } else {
                                Some(else_candidates)
                            };
                            nodes.push(AstNode::IfElse {
                                prefix_insts: vec![],
                                mnem: cond_mnem,
                                prev,
                                then_body: body,
                                else_body,
                                target: Some(header),
                                negate,
                                start: header,
                            });
                        }
                        return;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    pub fn render_dispatch(&self) -> Vec<String> {
        let mut lines = vec![
            format!(
                "void {}(int pc, uint16_t expected_retip, const char *file, const char *func, int line) {{",
                self.dispatch_name()
            ),
            "    (void)expected_retip;".into(),
            "    for (;;) {".into(),
            "        switch (pc) {".into(),
        ];
        lines.extend(self.dispatch_cases.iter().cloned());
        let default_popped = if self.cs_base != 0 {
            format!(
                "            uint16_t popped_ip = (uint16_t)((pc + 0x{:04X}) & 0xFFFF);",
                self.cs_base
            )
        } else {
            format!(
                "            uint16_t popped_ip = (uint16_t)((uint32_t)pc + 0x{:05X}U - ((uint32_t)cs << 4));",
                self.load_base
            )
        };
        lines.extend([
            "        default:".into(),
            "        {".into(),
            "            /* pc isn't a case in this binary's switch — most commonly a cross-binary RET / jmp. near_ret_tail composes cs:popped_ip into a linear address and resolves the right binary via file_mappings. Bogus IPs end up in its unhandled_pc abort path. */".into(),
            default_popped,
            "            near_ret_tail(popped_ip, expected_retip);".into(),
            "            return;".into(),
            "        }".into(),
            "        }".into(),
            "    }".into(),
            "}".into(),
        ]);
        lines
    }
}

// Mnemonics with a (real or placeholder) handler — mirrors CCodeRenderer.handlers
// keys plus the multiword string-rep forms. Used for format_instruction dispatch
// and the prefix-known check.
const HANDLED: &[&str] = &[
    "call", "lcall", "int", "mov", "add", "sub", "sbb", "adc", "and", "xor", "or", "inc", "dec",
    "aaa", "aas", "daa", "das", "mul", "imul", "div", "idiv", "not", "neg", "cmp", "test", "push",
    "pop", "pushf", "popf", "pushaw", "popaw", "leave", "enter", "shl", "shr", "sar", "sal", "rol",
    "ror", "rcl", "xchg", "xlatb", "lodsb", "lodsw", "movsb", "movsw", "cmpsb", "scasb", "scasw",
    "stosb", "stosw", "rep", "repe", "repne", "bound", "loop", "loopne", "loopnz", "loope",
    "loopz", "lahf", "sahf", "salc", "aam", "aad", "cld", "std", "cli", "sti", "in", "out", "lds",
    "les", "jmp", "ljmp", "cwde", "cdq", "clc", "cmc", "stc", "rcr", "nop", "db", "ret", "retn",
    "retf", "iret", "lea",
];

// ===================== entry =====================

fn struct_include() -> Vec<String> {
    [
        "#include <stddef.h>",
        "#include <string.h>",
        "#include <stdio.h>",
        "#include <stdlib.h>",
        "#include \"shims.h\"",
        "#include \"rcb_fields.h\"",
        "",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// emit_c — returns (c_text, h_text).
pub fn emit_c(
    ir: &Value,
    code_bytes: &[u8],
    prefix: &str,
    image_base: Option<i64>,
    out_path: &Path,
) -> (String, String) {
    let empty = Vec::new();
    let functions = ir
        .get("functions")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let known_funcs: BTreeSet<i64> = functions
        .iter()
        .filter_map(|f| f.get("start").and_then(Value::as_i64))
        .collect();
    let program_addrs: BTreeSet<i64> = functions
        .iter()
        .flat_map(|f| {
            f.get("instructions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|i| i.get("address").and_then(Value::as_i64))
        .collect();
    let mut addr_to_func: HashMap<i64, i64> = HashMap::new();
    for f in functions {
        let start = f.get("start").and_then(Value::as_i64).unwrap_or(0);
        if let Some(insns) = f.get("instructions").and_then(Value::as_array) {
            for insn in insns {
                if let Some(a) = insn.get("address").and_then(Value::as_i64) {
                    addr_to_func.entry(a).or_insert(start);
                }
            }
        }
    }
    let extern_labels: BTreeSet<i64> = ir
        .get("extern_labels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .filter(|addr| *addr_to_func.get(addr).unwrap_or(addr) == *addr)
                .collect()
        })
        .unwrap_or_default();

    let load_segment: i64 = 0x1010;
    let load_base = image_base.unwrap_or(load_segment << 4) & 0xFFFFF;

    let mut r = Renderer {
        prefix: prefix.to_string(),
        extern_labels: extern_labels.clone(),
        func_names: BTreeMap::new(),
        comments: BTreeMap::new(),
        cs_base: 0,
        load_base,
        program_addrs,
        pending_dos: Vec::new(),
        dispatch_cases: Vec::new(),
        seen_cases: BTreeSet::new(),
        last_ah: None,
        last_al: None,
        last_dx: None,
        last_bx: None,
        last_dl: None,
        last_cx: None,
        last_cl: None,
        last_ch: None,
        current_func_name: String::new(),
        code_bytes: code_bytes.to_vec(),
        current_func_all_addrs: BTreeSet::new(),
        current_func_addrs: BTreeSet::new(),
        current_blocks: BTreeMap::new(),
        loop_headers: BTreeSet::new(),
        extra_func_blocks: BTreeMap::new(),
        stack_var_sizes: HashMap::new(),
        reloc_offsets: ir
            .get("relocations")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|e| {
                        (e.get("segment").and_then(Value::as_i64).unwrap_or(0) << 4)
                            + e.get("offset").and_then(Value::as_i64).unwrap_or(0)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        load_segment,
    };

    // ---- .h ----
    let mut h: Vec<String> = vec![
        "#pragma once".into(),
        "".into(),
        "#include <stdint.h>".into(),
        "".into(),
    ];
    h.push(format!(
        "void {}(int pc, uint16_t expected_retip, const char *file, const char *func, int line);",
        r.dispatch_name()
    ));
    h.push("".into());
    for f in functions {
        let start = f.get("start").and_then(Value::as_i64).unwrap_or(0);
        if extern_labels.contains(&start) {
            continue;
        }
        let name = r.func_name(start);
        let impl_ = format!("{name}_impl");
        h.push(format!(
            "void {impl_}(uint16_t expected_retip, const char *file, const char *func, int line);"
        ));
        h.push("#ifndef SHIMS_DISABLE_MACROS".into());
        h.push(format!(
            "#define {name}(retip) {impl_}((retip), __FILE__, __func__, __LINE__)"
        ));
        h.push("#endif".into());
    }
    h.push("".into());

    // ---- .c: render functions (wrappers) then dispatch, matching main() ----
    let mut wrapper_lines: Vec<String> = Vec::new();
    for f in functions {
        let lines = r.render_function(f, &known_funcs);
        let start = f.get("start").and_then(Value::as_i64).unwrap_or(0);
        if extern_labels.contains(&start) {
            continue;
        }
        wrapper_lines.extend(lines);
        wrapper_lines.push("".into());
    }
    let dispatch_lines = r.render_dispatch();

    let header_name = out_path.with_extension("h");
    let header_name = header_name
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("out.h");
    let mut c: Vec<String> = struct_include();
    c.push(format!("#include \"{header_name}\""));
    c.push("".into());
    c.extend(dispatch_lines);
    c.push("".into());
    c.extend(wrapper_lines);

    (c.join("\n"), h.join("\n"))
}

/// Validation/test entry: build the same Renderer as `emit_c` but drive the
/// BASE `render_function_c` per function (the structured, readable-C path).
/// Returns `{"<hex start>": [lines], …, "__extra__": {"<hex>": [lines]}}`.
pub fn render_structured(
    ir: &Value,
    code_bytes: &[u8],
    prefix: &str,
    image_base: Option<i64>,
) -> Value {
    let empty = Vec::new();
    let functions = ir
        .get("functions")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let known_funcs: BTreeSet<i64> = functions
        .iter()
        .filter_map(|f| f.get("start").and_then(Value::as_i64))
        .collect();
    let program_addrs: BTreeSet<i64> = functions
        .iter()
        .flat_map(|f| {
            f.get("instructions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|i| i.get("address").and_then(Value::as_i64))
        .collect();
    let mut addr_to_func: HashMap<i64, i64> = HashMap::new();
    for f in functions {
        let start = f.get("start").and_then(Value::as_i64).unwrap_or(0);
        if let Some(insns) = f.get("instructions").and_then(Value::as_array) {
            for insn in insns {
                if let Some(a) = insn.get("address").and_then(Value::as_i64) {
                    addr_to_func.entry(a).or_insert(start);
                }
            }
        }
    }
    let extern_labels: BTreeSet<i64> = ir
        .get("extern_labels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .filter(|addr| *addr_to_func.get(addr).unwrap_or(addr) == *addr)
                .collect()
        })
        .unwrap_or_default();
    let mut r = Renderer::for_test(prefix);
    r.extern_labels = extern_labels;
    r.program_addrs = program_addrs;
    r.code_bytes = code_bytes.to_vec();
    r.load_base = image_base.unwrap_or(r.load_segment << 4) & 0xFFFFF;
    r.reloc_offsets = ir
        .get("relocations")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|e| {
                    (e.get("segment").and_then(Value::as_i64).unwrap_or(0) << 4)
                        + e.get("offset").and_then(Value::as_i64).unwrap_or(0)
                })
                .collect()
        })
        .unwrap_or_default();

    let mut out = serde_json::Map::new();
    for f in functions {
        let start = f.get("start").and_then(Value::as_i64).unwrap_or(0);
        let lines = r.render_function_c(f, &known_funcs);
        out.insert(
            format!("{start:04X}"),
            Value::Array(lines.into_iter().map(Value::String).collect()),
        );
    }
    let mut extra = serde_json::Map::new();
    for (a, lines) in &r.extra_func_blocks {
        extra.insert(
            format!("{a:04X}"),
            Value::Array(lines.iter().cloned().map(Value::String).collect()),
        );
    }
    out.insert("__extra__".into(), Value::Object(extra));
    Value::Object(out)
}
