//! byte image -> lossless `program.ir.json`.
//!
//! Faithfulness bar: the emitted `program.ir.json` must be BYTE-IDENTICAL to the
//! reference's, validated per-key by port/the source against a real game
//! corpus. Key order everywhere mirrors the reference dict insertion order exactly.
//!
//! Ported & validated: stage1, stage2 (CFG decode), compute_xrefs.
//! TODO(port): stage2 push-imm / jump-table discovery (only runs when cs_base is
//! set — never on JIT segment dumps — so it is validated separately).

use base64::Engine;
use capstone::arch::x86::{X86OpMem, X86OperandType};
use capstone::arch::ArchDetail;
use capstone::prelude::*;
use capstone::{Capstone, Insn, RegAccessType};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

// ===================== helpers =====================

fn u16le(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn u32le(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// Lowercase, unseparated hex — matches the reference `bytes.hex()`.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}
fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn prefix_seg(p: u8) -> Option<&'static str> {
    Some(match p {
        0x2E => "CS",
        0x36 => "SS",
        0x3E => "DS",
        0x26 => "ES",
        0x64 => "FS",
        0x65 => "GS",
        _ => return None,
    })
}

/// X86_EFLAGS_* (value, flag-suffix) from oracle capstone 5.0.7. decode_eflags
/// collects the suffix of every set bit into a sorted, de-duplicated list —
/// exactly the source `decode_eflags`.
const EFLAGS: &[(u64, &str)] = &[
    (0x0000000000000001, "AF"),
    (0x0000000000000002, "CF"),
    (0x0000000000000004, "SF"),
    (0x0000000000000008, "ZF"),
    (0x0000000000000010, "PF"),
    (0x0000000000000020, "OF"),
    (0x0000000000000040, "TF"),
    (0x0000000000000080, "IF"),
    (0x0000000000000100, "DF"),
    (0x0000000000000200, "NT"),
    (0x0000000000000400, "RF"),
    (0x0000000000000800, "OF"),
    (0x0000000000001000, "SF"),
    (0x0000000000002000, "ZF"),
    (0x0000000000004000, "AF"),
    (0x0000000000008000, "PF"),
    (0x0000000000010000, "CF"),
    (0x0000000000020000, "TF"),
    (0x0000000000040000, "IF"),
    (0x0000000000080000, "DF"),
    (0x0000000000100000, "NT"),
    (0x0000000000200000, "OF"),
    (0x0000000000400000, "CF"),
    (0x0000000000800000, "DF"),
    (0x0000000001000000, "IF"),
    (0x0000000002000000, "SF"),
    (0x0000000004000000, "AF"),
    (0x0000000008000000, "TF"),
    (0x0000000010000000, "NT"),
    (0x0000000020000000, "PF"),
    (0x0000000040000000, "CF"),
    (0x0000000080000000, "DF"),
    (0x0000000100000000, "IF"),
    (0x0000000200000000, "OF"),
    (0x0000000400000000, "SF"),
    (0x0000000800000000, "ZF"),
    (0x0000001000000000, "PF"),
    (0x0000002000000000, "CF"),
    (0x0000004000000000, "NT"),
    (0x0000008000000000, "DF"),
    (0x0000010000000000, "OF"),
    (0x0000020000000000, "SF"),
    (0x0000040000000000, "ZF"),
    (0x0000080000000000, "PF"),
    (0x0000100000000000, "AF"),
    (0x0000200000000000, "CF"),
    (0x0000400000000000, "RF"),
    (0x0000800000000000, "RF"),
    (0x0001000000000000, "IF"),
    (0x0002000000000000, "TF"),
    (0x0004000000000000, "AF"),
    (0x0008000000000000, "ZF"),
    (0x0010000000000000, "OF"),
    (0x0020000000000000, "SF"),
    (0x0040000000000000, "ZF"),
    (0x0080000000000000, "AF"),
    (0x0100000000000000, "PF"),
    (0x0200000000000000, "0F"),
    (0x0400000000000000, "AC"),
];

fn decode_eflags(mask: u64) -> Vec<String> {
    let mut set: BTreeSet<&str> = BTreeSet::new();
    for (v, name) in EFLAGS {
        if mask & v != 0 {
            set.insert(name);
        }
    }
    set.into_iter().map(str::to_string).collect()
}

/// Read the raw `cs_x86.eflags` union the high-level crate doesn't expose. Sound
/// because the crate guarantees `Insn` and `cs_insn` share layout (it relies on
/// the same cast internally).
fn insn_eflags(insn: &Insn) -> u64 {
    unsafe {
        let raw = &*(insn as *const Insn as *const capstone_sys::cs_insn);
        if raw.detail.is_null() {
            return 0;
        }
        (*raw.detail).__bindgen_anon_1.x86.__bindgen_anon_1.eflags
    }
}

fn imm_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:short|near)?\s*([+-]?(?:0x[0-9a-f]+|[0-9a-f]+h|[0-9a-f]+))$").unwrap()
    })
}

/// int(s, 0) — hex iff 0x-prefixed, else decimal; leading +/- allowed.
fn py_int_base0(s: &str) -> Option<i64> {
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let v = match rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        Some(h) => i64::from_str_radix(h, 16).ok()?,
        None => rest.parse::<i64>().ok()?,
    };
    Some(if neg { -v } else { v })
}

/// the source `_parse_imm` (also == the source's `_parse_imm`).
pub fn parse_imm(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.contains('[') || value.contains(']') || value.contains(':') {
        return None;
    }
    let caps = imm_re().captures(value)?;
    let mut token = caps.get(1)?.as_str().to_string();
    let mut sign = String::new();
    if token.starts_with(['+', '-']) {
        sign = token[..1].to_string();
        token = token[1..].to_string();
    }
    if token.ends_with(['h', 'H']) {
        token = format!("0x{}", &token[..token.len() - 1]);
    } else if token.to_lowercase().starts_with("0x") {
        // already hex
    } else if token.chars().all(|c| c.is_ascii_digit()) && !sign.is_empty() {
        // signed decimal — keep as-is
    } else {
        token = format!("0x{}", token);
    }
    py_int_base0(&format!("{sign}{token}"))
}

/// int(s, 16) — leading +/- and optional 0x allowed.
fn int_base16(s: &str) -> Option<i64> {
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

/// Parse a `seg:off` / `seg,off` operand into (seg, off) via `parser` (int16 or
/// _parse_imm), matching `op_str.replace(":", ",").split(",")` handling.
fn parse_far(op: &str, parser: fn(&str) -> Option<i64>) -> Option<(i64, i64)> {
    let parts: Vec<&str> = op.split([':', ',']).map(str::trim).collect();
    if parts.len() != 2 {
        return None;
    }
    Some((parser(parts[0])?, parser(parts[1])?))
}

// ===================== stage1 =====================

pub struct Metadata {
    pub load_module: Vec<u8>,
    pub header: Value,
    pub relocations: Value,
    pub segment_map: Value,
    /// entry_off = e_cs*16 + e_ip (0 for a raw BIN dump).
    pub entry_off: i64,
}

/// stage1 — binary intake and metadata capture.
pub fn stage1(data: &[u8]) -> Metadata {
    if data.get(0..2) != Some(b"MZ".as_slice()) {
        let header = json!({
            "e_magic": "BIN", "e_cblp": 0, "e_cp": 0, "e_crlc": 0, "e_cparhdr": 0,
            "e_minalloc": 0, "e_maxalloc": 0, "e_ss": 0, "e_sp": 0, "e_csum": 0,
            "e_ip": 0, "e_cs": 0, "e_lfarlc": 0, "e_ovno": 0,
            "e_res": [0, 0, 0, 0], "e_oemid": 0, "e_oeminfo": 0,
            "e_res2": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0], "e_lfanew": 0,
        });
        let segment_map = json!([{
            "name": "code", "base": 0, "size": data.len(),
            "bytes_b64": b64(data), "type": "code", "permissions": "r-x",
        }]);
        return Metadata {
            load_module: data.to_vec(),
            header,
            relocations: json!([]),
            segment_map,
            entry_off: 0,
        };
    }

    let e_cblp = u16le(data, 2);
    let e_cp = u16le(data, 4);
    let e_crlc = u16le(data, 6);
    let e_cparhdr = u16le(data, 8);
    let e_ss = u16le(data, 14);
    let e_sp = u16le(data, 16);
    let e_ip = u16le(data, 20);
    let e_cs = u16le(data, 22);
    let e_lfarlc = u16le(data, 24);
    let header = json!({
        "e_magic": String::from_utf8_lossy(&data[0..2]),
        "e_cblp": e_cblp, "e_cp": e_cp, "e_crlc": e_crlc, "e_cparhdr": e_cparhdr,
        "e_minalloc": u16le(data, 10), "e_maxalloc": u16le(data, 12),
        "e_ss": e_ss, "e_sp": e_sp, "e_csum": u16le(data, 18),
        "e_ip": e_ip, "e_cs": e_cs, "e_lfarlc": e_lfarlc, "e_ovno": u16le(data, 26),
        "e_res": [u16le(data, 28), u16le(data, 30), u16le(data, 32), u16le(data, 34)],
        "e_oemid": u16le(data, 36), "e_oeminfo": u16le(data, 38),
        "e_res2": [
            u16le(data, 40), u16le(data, 42), u16le(data, 44), u16le(data, 46),
            u16le(data, 48), u16le(data, 50), u16le(data, 52), u16le(data, 54),
            u16le(data, 56), u16le(data, 58),
        ],
        "e_lfanew": u32le(data, 60),
    });

    let mut relocs: Vec<Value> = Vec::new();
    for i in 0..e_crlc as usize {
        let eo = e_lfarlc as usize + i * 4;
        relocs.push(json!({ "offset": u16le(data, eo), "segment": u16le(data, eo + 2) }));
    }

    let header_size = e_cparhdr as usize * 16;
    let file_size = data.len();
    let expected_file_size = if e_cblp == 0 {
        e_cp as usize * 512
    } else {
        (e_cp as usize - 1) * 512 + e_cblp as usize
    };
    let module_size = file_size
        .min(expected_file_size)
        .saturating_sub(header_size);
    let load_module = data[header_size..header_size + module_size].to_vec();

    let mut segments = vec![json!({
        "name": "code", "base": 0x1000, "size": load_module.len(),
        "bytes_b64": b64(&load_module), "type": "code", "permissions": "r-x",
    })];
    if file_size > expected_file_size {
        let overlay = &data[expected_file_size..];
        segments.push(json!({
            "name": "overlay", "base": 0x1000 + load_module.len(), "size": overlay.len(),
            "bytes_b64": b64(overlay), "type": "overlay", "permissions": "r-x",
        }));
    }

    Metadata {
        load_module,
        header,
        relocations: Value::Array(relocs),
        segment_map: Value::Array(segments),
        entry_off: e_cs as i64 * 16 + e_ip as i64,
    }
}

// ===================== stage2 =====================

/// One decoded instruction (the shared `decoded` cache entry).
struct InsnRec {
    address: i64,
    bytes: String,
    mnemonic: String,
    op_str: String,
    detail: Value,
    target: Option<i64>,
    /// (segment, disp) per memory operand — for compute_xrefs.
    mem_refs: Vec<(String, i64)>,
}

impl InsnRec {
    fn size(&self) -> i64 {
        (self.bytes.len() / 2) as i64
    }
    fn to_value(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("address".into(), json!(self.address));
        m.insert("bytes".into(), json!(self.bytes));
        m.insert("mnemonic".into(), json!(self.mnemonic));
        m.insert("op_str".into(), json!(self.op_str));
        m.insert("detail".into(), self.detail.clone());
        if let Some(t) = self.target {
            m.insert("target".into(), json!(t));
        }
        Value::Object(m)
    }
}

fn default_segment(cs: &Capstone, m: &X86OpMem, prefix: &[u8; 4]) -> String {
    if let Some(name) = cs.reg_name(m.segment()) {
        if !name.is_empty() {
            return name.to_uppercase();
        }
    }
    for &p in prefix.iter() {
        if let Some(s) = prefix_seg(p) {
            return s.to_string();
        }
    }
    let base = cs.reg_name(m.base()).unwrap_or_default();
    let index = cs.reg_name(m.index()).unwrap_or_default();
    if base == "bp" || base == "sp" || index == "bp" || index == "sp" {
        return "SS".to_string();
    }
    "DS".to_string()
}

/// Decode ONE instruction from `code` at linear `addr`. None => capstone failed
/// (caller emits a `db` byte).
fn decode_one(cs: &Capstone, code: &[u8], addr: i64) -> Option<InsnRec> {
    let insns = cs.disasm_count(code, addr as u64, 1).ok()?;
    let insn = insns.iter().next()?;
    let detail = cs.insn_detail(&insn).ok()?;
    let x86 = match detail.arch_detail() {
        ArchDetail::X86Detail(x) => x,
        _ => return None,
    };
    let prefix = *x86.prefix();

    let mut det = serde_json::Map::new();
    let map_regs = |regs: &[capstone::RegId]| -> Vec<String> {
        regs.iter()
            .map(|r| cs.reg_name(*r).unwrap_or_default().to_uppercase())
            .collect()
    };
    det.insert("regs_read".into(), json!(map_regs(detail.regs_read())));
    det.insert("regs_write".into(), json!(map_regs(detail.regs_write())));
    let groups: Vec<String> = detail
        .groups()
        .iter()
        .map(|g| cs.group_name(*g).unwrap_or_default().to_uppercase())
        .collect();
    det.insert("groups".into(), json!(groups));
    det.insert("eflags".into(), json!(decode_eflags(insn_eflags(&insn))));

    if let Some(seg) = prefix.iter().find_map(|&p| prefix_seg(p)) {
        det.insert("seg_override".into(), json!(seg));
    }

    let mut mem_json: Vec<Value> = Vec::new();
    let mut mem_refs: Vec<(String, i64)> = Vec::new();
    for op in x86.operands() {
        if let X86OperandType::Mem(m) = op.op_type {
            let seg = default_segment(cs, &m, &prefix);
            let access = match op.access {
                Some(RegAccessType::ReadWrite) => "readwrite",
                Some(RegAccessType::WriteOnly) => "write",
                _ => "read",
            };
            let mut disp = m.disp();
            if m.base().0 == 0 && m.index().0 == 0 {
                disp &= 0xFFFF;
            }
            let mut o = serde_json::Map::new();
            o.insert("segment".into(), json!(seg));
            o.insert("disp".into(), json!(disp));
            o.insert("access".into(), json!(access));
            mem_json.push(Value::Object(o));
            mem_refs.push((seg, disp));
        }
    }
    if !mem_json.is_empty() {
        det.insert("mem_refs".into(), Value::Array(mem_json));
    }

    Some(InsnRec {
        address: addr,
        bytes: hex(insn.bytes()),
        mnemonic: insn.mnemonic().unwrap_or("").to_lowercase(),
        op_str: insn.op_str().unwrap_or("").to_string(),
        detail: Value::Object(det),
        target: None,
        mem_refs,
    })
}

fn enqueue(
    target: i64,
    new_func: bool,
    func_start: i64,
    len: i64,
    worklist: &mut Vec<(i64, i64)>,
    functions: &mut BTreeMap<i64, Vec<i64>>,
    visited: &HashMap<i64, HashSet<i64>>,
) {
    if target < 0 || target >= len {
        return;
    }
    let fs = if new_func { target } else { func_start };
    if !visited.get(&target).map_or(false, |s| s.contains(&fs)) {
        worklist.push((target, fs));
        if new_func {
            functions.entry(target).or_default();
        }
    }
}

const PUSH_IMM_REG_NAMES: &[&str] = &["ax", "bx", "cx", "dx", "si", "di", "bp"];

fn indirect_jmp_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\[\s*(?:[a-z]+\s*:\s*)?[a-z]+\s*\+\s*(0x[0-9a-fA-F]+|[0-9a-fA-F]+h|\d+)\s*\]",
        )
        .unwrap()
    })
}

/// the source `_discover_push_imm_targets` — tail-call return targets from
/// `push imm16` / `mov reg,imm16; push reg`.
fn discover_push_imm(decoded: &HashMap<i64, InsnRec>, cs_base: i64, code: &[u8]) -> BTreeSet<i64> {
    let mut targets = BTreeSet::new();
    let sorted: Vec<i64> = {
        let mut v: Vec<i64> = decoded.keys().cloned().collect();
        v.sort();
        v
    };
    let len = code.len() as i64;
    for (i, addr) in sorted.iter().enumerate() {
        let ins = &decoded[addr];
        if ins.mnemonic != "push" {
            continue;
        }
        let op = ins.op_str.trim().to_lowercase();
        if let Some(imm) = parse_imm(&op) {
            let fo = imm - cs_base;
            if 0 <= fo && fo < len {
                targets.insert(fo);
            }
            continue;
        }
        if !PUSH_IMM_REG_NAMES.contains(&op.as_str()) || i == 0 {
            continue;
        }
        let prev = &decoded[&sorted[i - 1]];
        if prev.mnemonic != "mov" {
            continue;
        }
        let pop = prev.op_str.trim().to_lowercase();
        let (dst, src) = match pop.split_once(',') {
            Some((d, s)) => (d.trim().to_string(), s.trim().to_string()),
            None => continue,
        };
        if dst != op {
            continue;
        }
        if let Some(imm) = parse_imm(&src) {
            let fo = imm - cs_base;
            if 0 <= fo && fo < len {
                targets.insert(fo);
            }
        }
    }
    targets
}

/// the source `_discover_jump_table_targets` — `jmp word ptr [reg+disp]`
/// table targets, bounded by a nearby `and reg, MASK`.
fn discover_jump_table(
    decoded: &HashMap<i64, InsnRec>,
    cs_base: i64,
    code: &[u8],
) -> BTreeSet<i64> {
    let mut targets = BTreeSet::new();
    let sorted: Vec<i64> = {
        let mut v: Vec<i64> = decoded.keys().cloned().collect();
        v.sort();
        v
    };
    let len = code.len() as i64;
    for (i, addr) in sorted.iter().enumerate() {
        let ins = &decoded[addr];
        if ins.mnemonic != "jmp" {
            continue;
        }
        let m = match indirect_jmp_re().captures(&ins.op_str) {
            Some(c) => c,
            None => continue,
        };
        let disp = match parse_imm(m.get(1).unwrap().as_str()) {
            Some(d) => d,
            None => continue,
        };
        let mut mask: Option<i64> = None;
        let lo = if i >= 12 { i - 12 } else { 0 };
        for j in (lo..i).rev() {
            let prev = &decoded[&sorted[j]];
            if prev.mnemonic != "and" {
                continue;
            }
            let pop = prev.op_str.to_lowercase();
            let parts: Vec<&str> = pop.splitn(2, ',').map(str::trim).collect();
            if parts.len() != 2 {
                continue;
            }
            if let Some(val) = parse_imm(parts[1]) {
                if 0 < val && val < 0x100 {
                    mask = Some(val);
                    break;
                }
            }
        }
        let mask = match mask {
            Some(m) => m,
            None => continue,
        };
        let table_off = disp - cs_base;
        let n = mask + 1;
        if table_off < 0 || table_off + n * 2 > len {
            continue;
        }
        for k in 0..n {
            let o = (table_off + k * 2) as usize;
            let entry = code[o] as i64 | ((code[o + 1] as i64) << 8);
            let fo = entry - cs_base;
            if 0 <= fo && fo < len {
                targets.insert(fo);
            }
        }
    }
    targets
}

pub struct Stage2Out {
    pub code: Value,
    pub functions: Value,
    pub data_regions: Value,
    pub extern_labels: Value,
    pub xrefs: Value,
}

/// stage2 — CFG-driven disassembly + compute_xrefs.
pub fn stage2(
    load_module: &[u8],
    entry_off: i64,
    extra_entries: &[i64],
    skip_entry_0000: bool,
    image_base: i64,
    max_insns: i64,
    cs_base: Option<i64>,
) -> Stage2Out {
    let len = load_module.len() as i64;
    let cs = Capstone::new()
        .x86()
        .mode(arch::x86::ArchMode::Mode16)
        .detail(true)
        .build()
        .expect("build capstone");

    let far_in_seg = |seg: i64| -> bool { image_base == 0 || seg == (image_base >> 4) };

    let mut visited: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut decoded: HashMap<i64, InsnRec> = HashMap::new();
    let mut decoded_owner: HashMap<i64, i64> = HashMap::new();
    let mut functions: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    let mut extern_labels: BTreeSet<i64> = BTreeSet::new();

    let mut entry_points: Vec<i64> = Vec::new();
    if !skip_entry_0000 {
        entry_points.push(entry_off);
    }
    let out_of_range: Vec<i64> = extra_entries
        .iter()
        .cloned()
        .filter(|&e| e >= len)
        .collect();
    if !out_of_range.is_empty() {
        let list = out_of_range
            .iter()
            .map(|e| format!("0x{e:X}"))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "Refusing to seed disassembly with entries past end-of-file (binary size 0x{len:X}): {list}"
        );
        std::process::exit(1);
    }
    entry_points.extend_from_slice(extra_entries);

    let mut worklist: Vec<(i64, i64)> = entry_points.iter().rev().map(|&off| (off, off)).collect();
    let mut total_insns: i64 = 0;

    loop {
        let (addr, func_start) = match worklist.pop() {
            Some(x) => x,
            None => {
                // Fixpoint discovery of push-imm / jump-table targets (gated on
                // cs_base). Re-runs as newly decoded code exposes more sites.
                let cb = match cs_base {
                    Some(c) => c,
                    None => break,
                };
                let mut seeds = discover_push_imm(&decoded, cb, load_module);
                seeds.append(&mut discover_jump_table(&decoded, cb, load_module));
                let fresh: Vec<i64> = seeds
                    .into_iter()
                    .filter(|s| !functions.contains_key(s))
                    .collect();
                if fresh.is_empty() {
                    break;
                }
                for s in fresh {
                    worklist.push((s, s));
                    functions.entry(s).or_default();
                }
                continue;
            }
        };
        functions.entry(func_start).or_default();
        let mut cur = addr;
        // `X or Y` quirk: owner 0 is falsy in the reference, so it doesn't count as < addr.
        let eff_owner = match decoded_owner.get(&addr).copied() {
            Some(0) | None => addr,
            Some(o) => o,
        };
        let overlap_stream = addr == func_start && eff_owner < addr;

        loop {
            if visited.get(&cur).map_or(false, |s| s.contains(&func_start)) || cur >= len {
                break;
            }
            match decoded_owner.get(&cur).copied() {
                Some(o) if o < cur && !overlap_stream => break,
                _ => {}
            }

            if !decoded.contains_key(&cur) {
                let rec = decode_one(&cs, &load_module[cur as usize..], cur).unwrap_or_else(|| {
                    let byte = load_module[cur as usize];
                    InsnRec {
                        address: cur,
                        bytes: format!("{byte:02x}"),
                        mnemonic: "db".to_string(),
                        op_str: format!("0x{byte:02x}"),
                        detail: json!({}),
                        target: None,
                        mem_refs: Vec::new(),
                    }
                });
                let insn_size = rec.size();
                for off in cur..cur + insn_size {
                    match decoded_owner.get(&off).copied() {
                        Some(p) if p <= cur => {}
                        _ => {
                            decoded_owner.insert(off, cur);
                        }
                    }
                }
                if overlap_stream && decoded_owner.get(&cur) != Some(&cur) {
                    decoded_owner.insert(cur, cur);
                }
                decoded.insert(cur, rec);
            }

            functions.get_mut(&func_start).unwrap().push(cur);
            visited.entry(cur).or_default().insert(func_start);
            total_insns += 1;
            if max_insns > 0 && total_insns >= max_insns {
                worklist.clear();
                break;
            }

            let (mnemonic, op_str, size) = {
                let r = &decoded[&cur];
                (r.mnemonic.clone(), r.op_str.clone(), r.size())
            };

            // DOS terminate sequences end a function.
            if mnemonic == "int" {
                if op_str == "0x20" {
                    break;
                }
                if op_str == "0x21" {
                    let flist = &functions[&func_start];
                    let mut terminate = false;
                    if flist.len() >= 2 {
                        let prev = &decoded[&flist[flist.len() - 2]];
                        if prev.mnemonic == "mov" {
                            if prev.op_str == "ah, 0x4c" {
                                terminate = true;
                            } else if let Some(rest) = prev.op_str.strip_prefix("ax, ") {
                                if let Some(v) = parse_imm(rest.trim()) {
                                    if (v >> 8) == 0x4C {
                                        terminate = true;
                                    }
                                }
                            }
                        }
                    }
                    if terminate {
                        break;
                    }
                }
            }

            let this = cur;
            if mnemonic == "call" {
                if let Some(t) = parse_imm(&op_str) {
                    decoded.get_mut(&this).unwrap().target = Some(t);
                    enqueue(
                        t,
                        true,
                        func_start,
                        len,
                        &mut worklist,
                        &mut functions,
                        &visited,
                    );
                }
                cur = this + size;
                continue;
            }
            if mnemonic == "lcall" {
                if let Some((seg, off)) = parse_far(&op_str, parse_imm) {
                    let t = seg * 16 + off - image_base;
                    if far_in_seg(seg) {
                        decoded.get_mut(&this).unwrap().target = Some(t);
                        enqueue(
                            t,
                            true,
                            func_start,
                            len,
                            &mut worklist,
                            &mut functions,
                            &visited,
                        );
                    }
                }
                cur = this + size;
                continue;
            }
            if mnemonic == "jmp" {
                if let Some(t) = parse_imm(&op_str) {
                    decoded.get_mut(&this).unwrap().target = Some(t);
                    enqueue(
                        t,
                        false,
                        func_start,
                        len,
                        &mut worklist,
                        &mut functions,
                        &visited,
                    );
                }
                break;
            }
            if mnemonic == "ljmp" {
                if let Some((seg, off)) = parse_far(&op_str, int_base16) {
                    let t = seg * 16 + off - image_base;
                    if far_in_seg(seg) {
                        decoded.get_mut(&this).unwrap().target = Some(t);
                        extern_labels.insert(t);
                        enqueue(
                            t,
                            true,
                            func_start,
                            len,
                            &mut worklist,
                            &mut functions,
                            &visited,
                        );
                    }
                }
                break;
            }
            if matches!(mnemonic.as_str(), "ret" | "retn" | "retf" | "hlt" | "iret") {
                break;
            }
            if mnemonic.starts_with('j') || mnemonic.starts_with("loop") {
                if let Some(t) = int_base16(&op_str) {
                    enqueue(
                        t,
                        false,
                        func_start,
                        len,
                        &mut worklist,
                        &mut functions,
                        &visited,
                    );
                }
                let fallthrough = this + size;
                enqueue(
                    fallthrough,
                    false,
                    func_start,
                    len,
                    &mut worklist,
                    &mut functions,
                    &visited,
                );
                break;
            }
            cur = this + size;
        }
    }

    // ---- assemble outputs (disassemble-930) ----
    let owned = |a: &i64| decoded_owner.get(a) == Some(a);

    let mut functions_json: Vec<Value> = Vec::new();
    for (start, addrs) in &functions {
        let mut ins: Vec<i64> = addrs.iter().cloned().filter(owned).collect();
        ins.sort();
        let instrs: Vec<Value> = ins.iter().map(|a| decoded[a].to_value()).collect();
        functions_json.push(json!({ "start": *start, "instructions": instrs }));
    }

    let mut code_addrs: BTreeSet<i64> = BTreeSet::new();
    for addrs in functions.values() {
        for a in addrs {
            if owned(a) {
                code_addrs.insert(*a);
            }
        }
    }
    let code: Vec<Value> = code_addrs.iter().map(|a| decoded[a].to_value()).collect();

    let mut code_bytes: HashSet<i64> = HashSet::new();
    for a in &code_addrs {
        for b in *a..*a + decoded[a].size() {
            code_bytes.insert(b);
        }
    }
    let mut data_regions: Vec<Value> = Vec::new();
    let mut start: Option<i64> = None;
    for i in 0..len {
        if !code_bytes.contains(&i) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            data_regions.push(json!({ "start": s, "end": i }));
        }
    }
    if let Some(s) = start {
        data_regions.push(json!({ "start": s, "end": len }));
    }

    let xrefs = compute_xrefs(&decoded, &code_addrs);

    Stage2Out {
        code: Value::Array(code),
        functions: Value::Array(functions_json),
        data_regions: Value::Array(data_regions),
        extern_labels: Value::Array(extern_labels.iter().map(|e| json!(e)).collect()),
        xrefs,
    }
}

/// compute_xrefs over the sorted global instruction list.
fn compute_xrefs(decoded: &HashMap<i64, InsnRec>, code_addrs: &BTreeSet<i64>) -> Value {
    let sorted: Vec<i64> = code_addrs.iter().cloned().collect();
    let mut xrefs: Vec<Value> = Vec::new();
    for (idx, a) in sorted.iter().enumerate() {
        let insn = &decoded[a];
        let size = insn.size();
        let m = insn.mnemonic.as_str();
        let op = insn.op_str.as_str();

        if m == "call" || m == "lcall" {
            let target = insn.target.or_else(|| {
                if m == "lcall" {
                    parse_far(op, int_base16).map(|(s, o)| s * 16 + o)
                } else {
                    int_base16(op)
                }
            });
            if let Some(t) = target {
                xrefs.push(json!(format!("call@0x{:X}", t)));
            }
        }
        if m == "jmp" || m == "ljmp" || m.starts_with('j') || m.starts_with("loop") {
            let target = insn.target.or_else(|| {
                if m == "ljmp" {
                    parse_far(op, int_base16).map(|(s, o)| s * 16 + o)
                } else {
                    int_base16(op)
                }
            });
            if let Some(t) = target {
                xrefs.push(json!(format!("jump@0x{:X}", t)));
            }
        }
        for (seg, disp) in &insn.mem_refs {
            let disp_str = if *disp < 0 {
                format!("-0x{:X}", -*disp)
            } else {
                format!("0x{:X}", disp)
            };
            xrefs.push(json!(format!("mem@{}:{}", seg, disp_str)));
        }
        if !matches!(m, "jmp" | "ljmp" | "hlt" | "iret" | "ret" | "retn" | "retf") {
            let mut terminate = false;
            if m == "int" && op == "0x20" {
                terminate = true;
            } else if m == "int" && op == "0x21" && idx > 0 {
                let prev = &decoded[&sorted[idx - 1]];
                if prev.mnemonic == "mov" {
                    let prev_op = prev.op_str.to_lowercase().replace(' ', "");
                    if prev_op == "ax,0x4c00" || prev_op == "ah,0x4c" {
                        terminate = true;
                    }
                }
            }
            if !terminate {
                xrefs.push(json!(format!("fallthrough@0x{:X}", a + size)));
            }
        }
    }
    Value::Array(xrefs)
}

// ===================== stage3 =====================

/// Full disassemble pipeline (stage1 -> stage2 -> stage3), returning the
/// program.ir.json text. Used by the `disasm` subcommand and the JIT orchestrator.
pub fn disassemble_ir(
    data: &[u8],
    extra_entries: &[i64],
    skip_entry_0000: bool,
    image_base: i64,
    max_insns: i64,
    cs_base: Option<i64>,
) -> String {
    let meta = stage1(data);
    let s2 = stage2(
        &meta.load_module,
        meta.entry_off,
        extra_entries,
        skip_entry_0000,
        image_base,
        max_insns,
        cs_base,
    );
    emit_program_ir(&meta, s2)
}

/// stage3 — merge into program.ir.json. Key order mirrors
/// the reference `program_ir` dict exactly.
pub fn emit_program_ir(meta: &Metadata, s2: Stage2Out) -> String {
    let ir = json!({
        "header": meta.header,
        "code": s2.code,
        "functions": s2.functions,
        "data_regions": s2.data_regions,
        "data": hex(&meta.load_module),
        "relocations": meta.relocations,
        "segment_map": meta.segment_map,
        "xrefs": s2.xrefs,
        "extern_labels": s2.extern_labels,
    });
    serde_json::to_string_pretty(&ir).unwrap()
}
