//! program IR -> readable Rust: the chunk emitter (the one and only backend).
//!
//! Emits a flat pc-state-machine — `loop { pc = match pc { … } }`, one match
//! arm per basic block calling a small per-block `fn … -> c_int` that returns
//! the next pc (-1 to leave the dispatcher), `set_ip`/`SAFEPOINT()` per
//! instruction — plus a `#[no_mangle]`
//! `_impl` wrapper per function, targeting the `saisei_rt` prelude
//! (`saisei-jitc/rt/saisei_rt.rs`). The shared front half (basic blocks, CFG
//! successors) and the operand-rewriting layer (`rewrite_operands`,
//! `match_rcb_access`, `decode_variables`, `fix_negative_imm`) come from
//! `translate`; this module only lowers the resulting operand strings and
//! control flow to Rust.
//!
//! A construct this emitter cannot express returns `Err(Unsupported)`, which
//! the JIT treats as a hard error — there is no fallback backend. Extend the
//! handler coverage here (repro offline with `saisei-jitc emit` or the
//! `gap_sweep` test).
//!
//! Faithfulness note: `mov` writes each register immediately — the plain x86
//! semantics. (The retired C backend deferred DOS-arg register writes to
//! reconstruct int-21h call arguments; interrupts here go through the
//! register-based `dos_api()` dispatcher instead, so no deferral exists.)

use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use regex::Regex;

use crate::translate::{
    build_basic_blocks, cfg_successors, decode_variables, fix_negative_imm, jcc_condition,
    normalize_flags, normalize_indirect_jumps, parse_imm, rewrite_mem_op, BasicBlock, Insn,
    Renderer,
};

/// A construct the Rust backend cannot emit yet. The caller falls back to C.
#[derive(Debug)]
pub struct Unsupported(pub String);
pub type R<T> = Result<T, Unsupported>;

fn uns<T>(what: impl Into<String>) -> R<T> {
    Err(Unsupported(what.into()))
}

/// FS or GS named anywhere in an instruction's operands — as a segment override
/// (a 0x64/0x65 prefix) or as the register itself (`push fs`, `mov ax, gs`).
///
/// They are 386 additions, and the CPU this backend targets does not have them:
/// the prelude's `word_reg!` block declares cs, ds, es and ss, and nothing else.
/// So an operand naming one cannot be lowered — but it was being lowered anyway.
/// `rewrite_mem_op` passes capstone's segment straight through, so `fs:[si]`
/// became `memw(fs, si)`, a call to an `fs()` accessor that does not exist, and
/// rustc failed the **whole chunk** with "cannot find function `fs`". Nothing
/// emits such bytes on purpose — it is the speculative decoder reading data as
/// code — but a chunk that will not compile is a chunk silently lost, and the
/// emitter had no idea it had produced one.
///
/// Reporting it as `Unsupported` puts it on the path built for exactly this case
/// (see `render_block`): the instruction becomes a `jit_unsupported_instruction`
/// trap, the chunk still compiles, and the gap is paid only if control really
/// arrives at those bytes — which, for data decoded as code, it never does.
///
/// Matched against capstone's own operand text, not the variable-renamed copy, so
/// a game annotation that happened to name something `fs` cannot trip it.
fn fs_gs_operand(insn: &Insn, raw_op_str: &str) -> Option<String> {
    let overridden = insn
        .get("detail")
        .and_then(|d| d.get("mem_refs"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("segment").and_then(Value::as_str))
        .find(|s| s.eq_ignore_ascii_case("fs") || s.eq_ignore_ascii_case("gs"));
    // The wording matters: this text is pasted into the emitted chunk as the trap's
    // message, and `runtime_abi_contract` reads that chunk back looking for calls
    // to runtime symbols with `(\w+)\s*\(`. An identifier followed by a space and a
    // bracket — "override (386+)" — reads to it as a call to `override`, and the
    // chunk gets reported for using a symbol the prelude never declared. Keep
    // brackets out of it.
    if let Some(seg) = overridden {
        return Some(format!(
            "{}: segment override on a CPU with no such register",
            seg.to_lowercase()
        ));
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)\b(fs|gs)\b").unwrap());
    re.captures(raw_op_str).map(|c| {
        format!(
            "{}: 386 segment register, and this CPU has none",
            c[1].to_lowercase()
        )
    })
}

/// A Rust `c"..."` literal holding `what`, safe to paste into emitted code.
/// Interior NULs cannot occur (the strings are built from mnemonics), but quotes
/// and backslashes must not be able to end the literal early.
fn c_string_literal(what: &str) -> String {
    let mut out = String::from("c\"");
    for ch in what.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7E => out.push('?'),
            c => out.push(c),
        }
    }
    out.push_str("\".as_ptr()");
    out
}

// ---- tiny IR accessors ------------------------------------------------------

fn s<'a>(i: &'a Insn, k: &str) -> &'a str {
    i.get(k).and_then(Value::as_str).unwrap_or("")
}
fn i64f(i: &Insn, k: &str) -> Option<i64> {
    i.get(k).and_then(Value::as_i64)
}
fn insn_len(i: &Insn) -> i64 {
    (s(i, "bytes").len() / 2) as i64
}

/// `int(s, 16)` — optional sign, optional 0x, base-16 (matches translate::parse_imm).
fn parse_hex16(t: &str) -> Option<i64> {
    let t = t.trim();
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let rest = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))
        .unwrap_or(rest);
    i64::from_str_radix(rest, 16)
        .ok()
        .map(|v| if neg { -v } else { v })
}

// ---- operand model ----------------------------------------------------------

const WORD_REGS: &[&str] = &["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"];
const BYTE_REGS: &[&str] = &["al", "ah", "bl", "bh", "cl", "ch", "dl", "dh"];
const SEG_REGS: &[&str] = &["cs", "ds", "es", "ss"];
/// 16-bit effective-address base/index registers (the only regs legal in `[]`).
const ADDR_REGS: &[&str] = &["bx", "bp", "si", "di"];

fn is_word_reg(l: &str) -> bool {
    WORD_REGS.contains(&l)
}
fn is_byte_reg(l: &str) -> bool {
    BYTE_REGS.contains(&l)
}
fn is_reg(l: &str) -> bool {
    is_word_reg(l) || is_byte_reg(l) || SEG_REGS.contains(&l)
}
fn operand_width8(op: &str) -> bool {
    op.starts_with("memb(") || is_byte_reg(&op.to_lowercase())
}

/// Noreturn runtime calls: after one, the block must `return -1;` — the
/// dispatcher exits (control goes
/// to the target and comes back via the trampoline). Mirrors NORETURN_FUNCS +
/// the `terminates` check of the retired C backend; names carry the `_` suffix the prelude
/// wrappers use (dos_exit is the exception).
fn terminates(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with("return") {
        return true;
    }
    const NORET: &[&str] = &[
        "jit_unsupported_instruction",
        "call_table_",
        "lcall_table_",
        "jump_table_",
        "long_jump_",
        "iret_",
        "retf_",
        "retf_pop_",
        "near_ret_tail_",
        "dos_exit",
    ];
    NORET
        .iter()
        .any(|f| t.starts_with(f) && t[f.len()..].starts_with('('))
}

/// True if capstone recorded `dx` among the instruction's written registers —
/// distinguishes 16-bit `mul r16` (ax*r16 -> dx:ax) from 8-bit `mul r8`.
fn writes_dx(insn: &Insn) -> bool {
    insn.get("detail")
        .and_then(|d| d.get("regs_write"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .any(|v| v.as_str().map_or(false, |s| s.eq_ignore_ascii_case("dx")))
        })
        .unwrap_or(false)
}

/// Rewrite one emitted line against the block-local register cache: register/
/// flag accessors and the runtime-call vocabulary become methods on `r`
/// (`ax()` → `r.ax()`, `memw(` → `r.memw(`, `JIT_BUDGET(` → `r.budget(`), so
/// blocks operate on the `&mut Regs` the dispatch loop threads through them
/// (noalias → rustc keeps guest registers in host registers) instead of the
/// shared `cpu` global. Pure helpers (seg_off, parity8, scanMemoryForAl,
/// exec_saved_*, set_interrupt_shadow) stay free functions.
fn localize_regs(line: &str) -> String {
    static REGS_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static CALLS_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let regs_re = REGS_RE.get_or_init(|| {
        Regex::new(
            r"\b((?:set_)?(?:ax|bx|cx|dx|si|di|bp|sp|ip|cs|ds|es|ss|al|ah|bl|bh|cl|ch|dl|dh|CF|PF|ZF|SF|OF|IF|DF))\(",
        )
        .unwrap()
    });
    let calls_re = CALLS_RE.get_or_init(|| {
        Regex::new(
            r"\b(memb_write|memw_write|memb|memw|rcb_read8|rcb_read16|rcb_write8|rcb_write16|inb|inw|outb|outw|JIT_BUDGET|HLT_WAIT|run_interrupt_resume|run_interrupt|schedule_interrupt|dos_api|dos_exit|bios_keyboard|rep_movsb_block|rep_movsw_block|rep_stosb_block|call_table_|lcall_table_|jump_table_|near_ret_tail_|long_jump_|iret_|retf_pop_|retf_|jit_unsupported_instruction)\(",
        )
        .unwrap()
    });
    let s = regs_re.replace_all(line, "r.$1(");
    let s = calls_re.replace_all(&s, |c: &regex::Captures| {
        let name = c.get(1).unwrap().as_str();
        let m = if name == "JIT_BUDGET" { "budget" } else { name };
        format!("r.{m}(")
    });
    s.into_owned()
}

/// Per-instruction virtual-time weight, in budget units (1 unit = one
/// jit_ns_per_instr quantum ≈ 3–4 cycles of the modeled 386-class CPU). The
/// flat 1-unit-per-instruction model let mul/div/string/memory-heavy code buy
/// far less virtual time than real hardware charges; weighting the block
/// debit by instruction class keeps the virtual clock faithful AND raises the
/// sustainable model speed (a heavy instruction buys proportionally more
/// virtual time per host cycle). Weights approximate 386 cycle counts / 3.3.
/// String instructions count ONLY their setup here — rep iteration costs are
/// debited at run time (the rep_*_block impls debit `count`; the inline rep
/// loops emit a dynamic `JIT_BUDGET(cx() …)` debit).
fn insn_weight(insn: &Insn) -> u32 {
    let mnem = s(insn, "mnemonic");
    let base = mnem.split(' ').last().unwrap_or(mnem); // "rep movsb" -> "movsb"
    match base {
        "mul" | "imul" => 5,  // 386: 12–25 cycles
        "div" | "idiv" => 12, // 386: 38–43 cycles
        "aam" | "aad" => 5,   // 17–19 cycles
        "int" => 10,          // INT dispatch ≈ 37 cycles
        "iret" => 10,
        // An I/O access is not priced in CPU cycles: the core stalls for a whole
        // ISA bus cycle, ~1us, which dwarfs the 12-26 cycles the instruction
        // itself takes. That is not a detail — it is the unit DOS drivers measure
        // delays in. "Wait 80us" is written as "read the status port N times",
        // and the AdLib manual specifies its post-write delays in exactly those
        // terms. Priced at 4 units (160ns) the loop runs ~6x short, so an AdLib
        // presence check starts timer 1 and gives up polling for the overflow
        // before virtual time has reached it — the card is there and is never
        // found. See runtime/src/audio/opl2.rs for the handshake it fails.
        "in" | "out" => 25,
        "enter" => 4,
        "pushaw" | "popaw" => 6,                // 18–24 cycles
        "push" | "pop" | "pushf" | "popf" => 2, // 5–7 cycles
        "leave" | "lds" | "les" | "xlatb" => 2,
        "call" => 3,                                           // near call: 7+m cycles
        "ret" | "retn" | "retf" => 3,                          // plus the runtime transfer debits
        "loop" | "loopne" | "loopnz" | "loope" | "loopz" => 3, // 11–13 cycles
        "shl" | "shr" | "sar" | "sal" | "rol" | "ror" | "rcl" | "rcr" => 2, // 3+n cycles
        // Bare string op ≈ 7 cycles; under rep this is setup only (the
        // per-iteration debit happens at run time).
        "movsb" | "movsw" | "stosb" | "stosw" | "lodsb" | "lodsw" | "cmpsb" | "cmpsw" | "scasb"
        | "scasw" => 2,
        // The port-string ops carry an I/O access, like `in`/`out` above.
        "insb" | "insw" | "outsb" | "outsw" => 25,
        "lea" => 1, // address arithmetic only — no memory access
        "hlt" => 1, // idles at host pace anyway
        "jmp" => 2,
        m if m.starts_with('j') => 2, // jcc: 3 not taken / 7+m taken
        _ => {
            // ALU/mov class: 2 cycles reg-reg; ~6–7 with a memory operand.
            if s(insn, "op_str").contains('[') {
                2
            } else {
                1
            }
        }
    }
}

fn var_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^var_([0-9a-f]+)$").unwrap())
}

fn mem_call_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^mem([bw])\((cs|ds|es|ss), (.+)\)$").unwrap())
}

/// Parse a rewrite_mem_op result `memX(seg, addr)` -> (is_byte, seg, addr).
fn parse_mem(op: &str) -> Option<(bool, String, String)> {
    let c = mem_call_re().captures(op.trim())?;
    let byte = c.get(1).unwrap().as_str().eq_ignore_ascii_case("b");
    let seg = c.get(2).unwrap().as_str().to_lowercase();
    let addr = c.get(3).unwrap().as_str().trim().to_string();
    Some((byte, seg, addr))
}

/// Render one effective-address term (a base/index reg or a constant) as u32.
fn addr_term(tok: &str) -> Option<String> {
    let t = tok.trim();
    let tl = t.to_lowercase();
    if ADDR_REGS.contains(&tl.as_str()) {
        return Some(format!("({tl}() as u32)"));
    }
    let v = parse_hex16(t).or_else(|| t.parse::<i64>().ok())?;
    Some(format!("0x{:X}u32", v & 0xFFFF_FFFF))
}

/// Render an address inner-expression (`bx + 0x10`, `bp + si - 2`, `di`) as a
/// wrapping u32 expression. None if any token isn't a legal addr reg/constant.
fn render_addr_expr(inner: &str) -> Option<String> {
    // Split into signed terms (top level; 16-bit EAs have no parens/mul here).
    let spaced = inner.replace('+', " + ").replace('-', " - ");
    let toks: Vec<&str> = spaced.split_whitespace().collect();
    let mut out: Option<String> = None;
    let mut sign = '+';
    let mut i = 0;
    while i < toks.len() {
        match toks[i] {
            "+" => sign = '+',
            "-" => sign = '-',
            t => {
                let term = addr_term(t)?;
                out = Some(match out {
                    None => {
                        if sign == '-' {
                            format!("0u32.wrapping_sub({term})")
                        } else {
                            term
                        }
                    }
                    Some(acc) if sign == '-' => format!("{acc}.wrapping_sub({term})"),
                    Some(acc) => format!("{acc}.wrapping_add({term})"),
                });
            }
        }
        i += 1;
    }
    out
}

/// Strip a single fully-enclosing outer paren pair, recursively: `((x) & y)` ->
/// `(x) & y` (the first `(` matches the LAST `)`), but leaves `(x) & y` alone
/// (its first `(` closes before the end). The C renderer sometimes wraps the
/// whole effective address, e.g. `((bp + 0xFFFC) & 0xFFFF)` for `[bp-4]`.
fn strip_outer_parens(s: &str) -> &str {
    let s = s.trim();
    if !s.starts_with('(') || !s.ends_with(')') {
        return s;
    }
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    // The matching close of the first '('. Fully enclosing iff it's
                    // the final char; otherwise the parens are structural — leave them.
                    return if i == s.len() - 1 {
                        strip_outer_parens(&s[1..s.len() - 1])
                    } else {
                        s
                    };
                }
            }
            _ => {}
        }
    }
    s
}

/// Render a memory operand's offset (the `addr` from `memX(seg, addr)`) as a
/// `u16` Rust expression.
fn render_addr(addr: &str) -> Option<String> {
    let a = strip_outer_parens(addr.trim());
    // `(EXPR) & 0xFFFF`
    if let Some(rest) = a.strip_suffix("& 0xFFFF").map(|x| x.trim()) {
        let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
        let e = render_addr_expr(inner)?;
        return Some(format!("(({e}) & 0xFFFF) as u16"));
    }
    // bare addr register
    if ADDR_REGS.contains(&a.to_lowercase().as_str()) {
        return Some(format!("{}()", a.to_lowercase()));
    }
    // bare constant
    if let Some(v) = parse_hex16(a).or_else(|| a.parse::<i64>().ok()) {
        return Some(format!("0x{:X}", v & 0xFFFF));
    }
    // general effective-address expression (e.g. `bx + 0x10`) with no mask
    let e = render_addr_expr(a)?;
    Some(format!("(({e}) & 0xFFFF) as u16"))
}

// ============================================================================

pub struct RRenderer {
    /// The C renderer, reused purely for its operand-rewriting layer + state
    /// (stack_var_sizes, reloc_offsets, current_func_name). We never emit its C.
    t: Renderer,
    cs_base: i64,
    load_base: i64,
    dispatch_cases: Vec<String>,
    /// One small `fn {prefix}blk_{addr} -> c_int` per basic block (returns the
    /// next pc, -1 to leave the dispatcher). Kept out of the dispatch fn so no
    /// single fn body grows with the chunk — rustc's per-body analyses
    /// (borrowck especially) are superlinear and dominate JIT compile time.
    block_fns: Vec<String>,
    seen_cases: BTreeSet<i64>,
    known_funcs: BTreeSet<i64>,
    /// Constructs this chunk could not translate. Each one became a run-time trap
    /// rather than a compile-time failure (see render_block), but they are still
    /// the gap frontier — jit-compile logs them and `gap_sweep` aggregates them.
    unsupported: Vec<String>,
}

impl RRenderer {
    fn dispatch_name(&self) -> String {
        format!("{}dispatch", self.t.prefix)
    }
    fn func_name(&self, addr: i64) -> String {
        format!("{}func_{:04X}", self.t.prefix, addr)
    }
    fn block_name(&self, addr: i64) -> String {
        format!("{}blk_{:04X}", self.t.prefix, addr)
    }

    // ---- operand lowering --------------------------------------------------

    /// Lower a (rewrite_operands-produced) operand to a Rust rvalue expression.
    /// An rvalue read as a signed value of its operand width.
    ///
    /// Not `({v}) as i16`: `rvalue` renders an immediate as a bare hex literal,
    /// and rustc then types that literal as `i16`, where `0xCD00` does not fit —
    /// the chunk does not compile. (Nor does a *negative* immediate, which
    /// `rvalue` already hands over as its 16-bit two's complement, `0xFFB4`.)
    /// Going through the unsigned width first is a no-op for a register or a
    /// `memw` read, and for a literal it is the truncate-then-reinterpret that
    /// x86 does anyway.
    fn signed16(v: &str) -> String {
        format!("({v}) as u16 as i16")
    }

    fn signed8(v: &str) -> String {
        format!("({v}) as u8 as i8")
    }

    fn rvalue(&self, op: &str) -> R<String> {
        let l = op.to_lowercase();
        if is_reg(&l) {
            return Ok(format!("{l}()"));
        }
        if op == "exec_params.saved_sp" {
            return Ok("exec_saved_sp()".into());
        }
        if op == "exec_params.saved_ss" {
            return Ok("exec_saved_ss()".into());
        }
        if let Some((byte, seg, addr)) = parse_mem(op) {
            let off = render_addr(&addr).ok_or_else(|| Unsupported(format!("addr:{addr}")))?;
            let f = if byte { "memb" } else { "memw" };
            return Ok(format!("{f}({seg}(), {off})"));
        }
        if let Some(v) = parse_imm(op) {
            // Disassembler-signed immediates (e.g. `-0x4c`) render as their 16-bit
            // two's-complement; the consuming arithmetic masks to the operand
            // width (& 0xFF / & 0xFFFF), so this is correct for byte and word ops.
            if v < 0 {
                return Ok(format!("0x{:X}", (v as u32) & 0xFFFF));
            }
            return Ok(format!("0x{v:X}"));
        }
        uns(format!("rvalue:{op}"))
    }

    /// Lower a store into a (rewrite_operands-produced) destination operand.
    fn store(&self, dest: &str, val: &str) -> R<Vec<String>> {
        let l = dest.to_lowercase();
        if dest == "exec_params.saved_sp" {
            return Ok(vec![format!("set_exec_saved_sp({val});")]);
        }
        if dest == "exec_params.saved_ss" {
            return Ok(vec![format!("set_exec_saved_ss({val});")]);
        }
        if is_reg(&l) {
            let mut v = vec![format!("set_{l}({val});")];
            if l == "ss" {
                v.push("set_interrupt_shadow(1);".into());
            }
            return Ok(v);
        }
        if let Some((byte, seg, addr)) = parse_mem(dest) {
            let off = render_addr(&addr).ok_or_else(|| Unsupported(format!("addr:{addr}")))?;
            let f = if byte { "memb_write" } else { "memw_write" };
            return Ok(vec![format!("{f}({seg}(), {off}, {val});")]);
        }
        uns(format!("store:{dest}"))
    }

    fn split2(&self, op_str: &str) -> R<(String, String)> {
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            return uns(format!("operands:{op_str}"));
        }
        Ok((parts[0].clone(), parts[1].clone()))
    }

    // ---- per-instruction handlers ------------------------------------------

    /// The target expression for an indirect jump (register or memory), mirroring
    /// indirect_jump_target: reg -> `reg()`, mem -> `memw(seg(),off)` / rcb_read.
    fn indirect_jump_target(&self, insn: &Insn) -> R<String> {
        if let Some(reg) = insn.get("reg").and_then(Value::as_str) {
            return Ok(format!("{}()", reg.to_lowercase()));
        }
        let seg = insn.get("seg").and_then(Value::as_str).unwrap_or("ds");
        let ea = insn.get("ea").and_then(Value::as_str).unwrap_or("0");
        let width = i64f(insn, "width").unwrap_or(16);
        let size_tok = if width == 8 { "byte" } else { "word" };
        let mem = rewrite_mem_op(&format!("{size_tok} ptr [{ea}]"), Some(seg));
        if let Some((size, field)) = self.t.match_rcb_access(&mem) {
            let f = if size == "w" {
                "rcb_read16"
            } else {
                "rcb_read8"
            };
            return Ok(format!("{f}({field})"));
        }
        self.rvalue(&mem)
    }

    /// Render a far seg/off argument (constant, memw(...), or an RCB field) to
    /// Rust. Mirrors the C `rcb_read` transform on memw expressions.
    fn render_arg(&self, e: &str) -> R<String> {
        let t = e.trim();
        if t.starts_with("rcb_read") {
            return Ok(t.to_string());
        }
        if let Some((size, field)) = self.t.match_rcb_access(t) {
            let f = if size == "w" {
                "rcb_read16"
            } else {
                "rcb_read8"
            };
            return Ok(format!("{f}({field})"));
        }
        if t.starts_with("memw(") || t.starts_with("memb(") {
            return self.rvalue(t);
        }
        if let Some(v) = parse_hex16(t).or_else(|| t.parse::<i64>().ok()) {
            return Ok(format!("0x{:04X}", v & 0xFFFF));
        }
        self.rvalue(t)
    }

    /// lcall — far call. Emits `lcall_table_(ret, seg, off)` (noreturn -> return).
    fn handle_lcall(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let mut op = op_str.to_lowercase().trim().to_string();
        let ret = self.call_return_arg(insn);
        if op.contains(':') {
            op = op.splitn(2, ':').nth(1).unwrap().trim().to_string();
        }
        let seg_of_ref = insn
            .get("detail")
            .and_then(|d| d.get("mem_refs"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|m| m.get("segment"))
            .and_then(Value::as_str)
            .map(|s| s.to_lowercase());
        // imm seg, off
        if let Some((a, b)) = op.split_once(',') {
            if let (Some(mut seg), Some(off)) = (parse_hex16(a.trim()), parse_hex16(b.trim())) {
                let sz = insn_len(insn);
                if self
                    .t
                    .reloc_offsets
                    .contains(&(i64f(insn, "address").unwrap_or(0) + sz - 2))
                {
                    seg = (seg + self.t.load_segment) & 0xFFFF;
                }
                return Ok(vec![format!(
                    "lcall_table_({ret}, 0x{:04X}, 0x{:04X});",
                    seg & 0xFFFF,
                    off & 0xFFFF
                )]);
            }
        }
        // memory forms: build the two memw(seg, ..) exprs and render them.
        let (seg_mem, off_mem) = self.lcall_far_ptr(insn, &op, seg_of_ref)?;
        let sa = self.render_arg(&seg_mem)?;
        let oa = self.render_arg(&off_mem)?;
        Ok(vec![format!("lcall_table_({ret}, {sa}, {oa});")])
    }

    /// Build the (seg_memw, off_memw) C-form expressions for a memory lcall/ljmp.
    fn lcall_far_ptr(&self, insn: &Insn, op: &str, seg_ref: Option<String>) -> R<(String, String)> {
        if let Some(cap) = var_re().captures(op) {
            let mr = insn
                .get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first());
            let (seg, disp) = match mr {
                Some(m) => (
                    m.get("segment")
                        .and_then(Value::as_str)
                        .unwrap_or("ss")
                        .to_lowercase(),
                    m.get("disp").and_then(Value::as_i64).unwrap_or(0),
                ),
                None => ("ss".into(), -i64::from_str_radix(&cap[1], 16).unwrap_or(0)),
            };
            let off = format!("memw({seg}, ((bp + 0x{:04X}) & 0xFFFF))", disp & 0xFFFF);
            let seg_e = format!(
                "memw({seg}, ((bp + 0x{:04X}) & 0xFFFF))",
                (disp + 2) & 0xFFFF
            );
            return Ok((seg_e, off));
        }
        if let Some(inner) = op.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            let inner = inner.trim();
            let seg = seg_ref.unwrap_or_else(|| "ds".into());
            let off_c = parse_hex16(inner).or_else(|| inner.parse::<i64>().ok());
            match off_c {
                Some(off) => {
                    let off = off & 0xFFFF;
                    return Ok((
                        format!("memw({seg}, 0x{:04X})", (off + 2) & 0xFFFF),
                        format!("memw({seg}, 0x{off:04X})"),
                    ));
                }
                None => {
                    return Ok((
                        rewrite_mem_op(&format!("word ptr [{inner}+2]"), Some(&seg)),
                        rewrite_mem_op(&format!("word ptr [{inner}]"), Some(&seg)),
                    ));
                }
            }
        }
        uns(format!("lcall {op}"))
    }

    /// ljmp — far jump. Emits `long_jump_(seg, off); return;`.
    fn handle_ljmp(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ':')
            .map(|p| p.trim().to_string())
            .collect();
        // seg:off immediate
        if parts.len() == 2 {
            if let (Some(mut seg), Some(off)) = (parse_hex16(&parts[0]), parse_hex16(&parts[1])) {
                let sz = insn_len(insn);
                if self
                    .t
                    .reloc_offsets
                    .contains(&(i64f(insn, "address").unwrap_or(0) + sz - 2))
                {
                    seg = (seg + self.t.load_segment) & 0xFFFF;
                }
                return Ok(vec![
                    format!(
                        "long_jump_(0x{:04X}, 0x{:04X});",
                        seg & 0xFFFF,
                        off & 0xFFFF
                    ),
                    "return -1;".into(),
                ]);
            }
            // seg_reg:[mem]
            let seg_reg = parts[0].to_lowercase();
            let mem = parts[1].trim();
            if matches!(seg_reg.as_str(), "cs" | "ds" | "es" | "ss") {
                if let Some(inner) = mem.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                    return self.ljmp_mem(inner.trim(), &seg_reg);
                }
            }
        }
        // [mem] no prefix
        let mem = op_str.trim();
        if let Some(inner) = mem.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            let seg = insn
                .get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|m| m.get("segment"))
                .and_then(Value::as_str)
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "ds".into());
            return self.ljmp_mem(inner.trim(), &seg);
        }
        uns(format!("ljmp {op_str}"))
    }

    fn ljmp_mem(&self, inner: &str, seg: &str) -> R<Vec<String>> {
        let off_c = parse_hex16(inner).or_else(|| inner.parse::<i64>().ok());
        let (seg_mem, off_mem) = match off_c {
            Some(off) => {
                let off = off & 0xFFFF;
                (
                    format!("memw({seg}, 0x{:04X})", (off + 2) & 0xFFFF),
                    format!("memw({seg}, 0x{off:04X})"),
                )
            }
            None => (
                rewrite_mem_op(&format!("word ptr [{inner}+2]"), Some(seg)),
                rewrite_mem_op(&format!("word ptr [{inner}]"), Some(seg)),
            ),
        };
        let sa = self.render_arg(&seg_mem)?;
        let oa = self.render_arg(&off_mem)?;
        Ok(vec![
            format!("long_jump_({sa}, {oa});"),
            "return -1;".into(),
        ])
    }

    fn handle_ret(&mut self, insn: &Insn) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic");
        let imm = parse_imm(s(insn, "op_str"));
        if mnem == "retf" {
            let mut lines = Vec::new();
            match imm {
                Some(v) => lines.push(format!("retf_pop_(0x{:X});", v & 0xFFFF)),
                None => lines.push("retf_();".into()),
            }
            lines.push("return -1;".into());
            return Ok(lines);
        }
        let mut lines = vec![
            "{".into(),
            "    let popped_ip = memw(ss(), sp());".into(),
            "    set_sp((sp().wrapping_add(2)) & 0xFFFF);".into(),
        ];
        if let Some(v) = imm {
            lines.push(format!(
                "    set_sp((sp().wrapping_add(0x{:X})) & 0xFFFF);",
                v & 0xFFFF
            ));
        }
        if self.cs_base != 0 {
            lines.push(format!(
                "    return ((popped_ip.wrapping_sub(0x{:04X})) & 0xFFFF) as i32;",
                self.cs_base
            ));
        } else {
            lines.push(format!(
                "    return (((cs() as u32) << 4).wrapping_add(popped_ip as u32).wrapping_sub(0x{:05X})) as i32;",
                self.load_base
            ));
        }
        lines.push("}".into());
        Ok(lines)
    }

    fn handle_mov(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let (d, spart) = self.split2(&op_str)?;
        let rw = self.t.rewrite_operands(insn, &[d, spart]);
        let dest = rw[0].clone();
        let mut src = rw[1].clone();

        // RCB field access (es:0xFF..) -> rcb_readN / rcb_writeN.
        if let Some((size, field)) = self.t.match_rcb_access(&dest) {
            let sv = self.rvalue(&src)?;
            let f = if size == "w" {
                "rcb_write16"
            } else {
                "rcb_write8"
            };
            return Ok(vec![format!("{f}({field}, {sv});")]);
        }
        if let Some((size, field)) = self.t.match_rcb_access(&src) {
            let f = if size == "w" {
                "rcb_read16"
            } else {
                "rcb_read8"
            };
            return self.store(&dest, &format!("{f}({field})"));
        }

        // Relocation-adjusted immediate (segment fixups), mirroring handle_mov.
        if let Some(mut imm_value) = parse_imm(&src) {
            let size_bytes = insn_len(insn);
            if size_bytes >= 3 {
                let imm_offset = i64f(insn, "address").unwrap_or(0) + size_bytes - 2;
                if self.t.reloc_offsets.contains(&imm_offset) {
                    imm_value = (imm_value + self.t.load_segment) & 0xFFFF;
                    src = format!("0x{imm_value:04X}");
                }
            }
        }

        let sv = self.rvalue(&src)?;
        self.store(&dest, &sv)
    }

    fn handle_cmp_test(&mut self, insn: &Insn, is_test: bool) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic");
        let op_str = decode_variables(s(insn, "op_str"));
        let (l0, r0) = self.split2(&op_str)?;
        let rw = self.t.rewrite_operands(insn, &[l0, r0]);
        let mut left = rw[0].clone();
        let mut right = rw[1].clone();
        left = fix_negative_imm(&left, &right);
        right = fix_negative_imm(&right, &left);
        // RCB operands: fall back to C (rare in cmp/test).
        if self.t.match_rcb_access(&left).is_some() || self.t.match_rcb_access(&right).is_some() {
            return uns(format!("{mnem} rcb"));
        }
        let lv = self.rvalue(&left)?;
        let rv = self.rvalue(&right)?;
        let is_byte = left.starts_with("memb(") || is_byte_reg(&left.to_lowercase());
        let (shift, sign, rtype) = if is_byte {
            (7u32, 0x80u32, "u8")
        } else {
            (15, 0x8000, "u16")
        };
        let mut lines = vec![
            "{".into(),
            format!("    let left_val: u32 = ({lv}) as u32;"),
            format!("    let right_val: u32 = ({rv}) as u32;"),
        ];
        if is_test {
            lines.push(format!(
                "    let result = (left_val & right_val) as {rtype};"
            ));
            lines.push("    set_CF(0);".into());
            lines.push("    set_OF(0);".into());
        } else {
            lines.push("    set_CF((left_val < right_val) as u8);".into());
            lines.push("    let tmp = left_val.wrapping_sub(right_val);".into());
            lines.push(format!("    let result = tmp as {rtype};"));
        }
        lines.push("    set_ZF((result == 0) as u8);".into());
        lines.push(format!("    set_SF(((result >> {shift}) & 1) as u8);"));
        lines.push("    set_PF(parity8(result as u8));".into());
        if !is_test {
            lines.push(format!(
                "    set_OF((((left_val ^ right_val) & (left_val ^ (result as u32)) & 0x{sign:X}) != 0) as u8);"
            ));
        }
        lines.push("}".into());
        Ok(lines)
    }

    fn handle_push(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let rw = self.t.rewrite_operands(insn, &[op_str]);
        let op = rw[0].clone();
        if op.eq_ignore_ascii_case("sp") {
            return Ok(vec![
                "let push_value = sp();".into(),
                "set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into(),
                "memw_write(ss(), sp(), push_value);".into(),
            ]);
        }
        let v = self.rvalue(&op)?;
        Ok(vec![
            "set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into(),
            format!("memw_write(ss(), sp(), {v});"),
        ])
    }

    fn handle_pop(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let rw = self.t.rewrite_operands(insn, &[op_str]);
        let dest = rw[0].clone();
        if dest.eq_ignore_ascii_case("sp") {
            return Ok(vec![
                "{".into(),
                "    let tmp = memw(ss(), sp());".into(),
                "    set_sp((sp().wrapping_add(2)) & 0xFFFF);".into(),
                "    set_sp(tmp);".into(),
                "}".into(),
            ]);
        }
        let mut lines = self.store(&dest, "memw(ss(), sp())")?;
        lines.push("set_sp((sp().wrapping_add(2)) & 0xFFFF);".into());
        Ok(lines)
    }

    fn simple(&self, lines: &[&str]) -> R<Vec<String>> {
        Ok(lines.iter().map(|s| s.to_string()).collect())
    }

    fn fallthrough_off(&self, insn: &Insn) -> Option<i64> {
        if let Some(n) = i64f(insn, "_next_addr") {
            return Some(n);
        }
        let size = insn_len(insn);
        if size <= 0 {
            return None;
        }
        i64f(insn, "address").map(|a| a + size)
    }

    /// Return-IP argument pushed by a call (cs-relative), matching call_return_arg.
    fn call_return_arg(&self, insn: &Insn) -> String {
        match self.fallthrough_off(insn) {
            None => "ip()".into(),
            Some(off) => format!(
                "((0x{off:X}u32).wrapping_add(0x{:05X}).wrapping_sub((cs() as u32) << 4)) as u16",
                self.load_base
            ),
        }
    }

    /// _string_source_segment — the source segment for a string op (ds default,
    /// honoring a seg override recorded on a read/readwrite mem_ref).
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

    fn h_lods(&mut self, insn: &Insn, w: i64, reg: &str, m: &str) -> R<Vec<String>> {
        let seg = self.string_source_segment(insn);
        Ok(vec![
            "{".into(),
            format!("    let delta: i32 = if DF() != 0 {{ -{w} }} else {{ {w} }};"),
            format!("    set_{reg}({m}({seg}(), si()));"),
            "    set_si(((si() as i32 + delta) & 0xFFFF) as u16);".into(),
            "}".into(),
        ])
    }
    fn h_stos(&mut self, w: i64, reg: &str, m: &str) -> R<Vec<String>> {
        Ok(vec![
            "{".into(),
            format!("    let delta: i32 = if DF() != 0 {{ -{w} }} else {{ {w} }};"),
            format!("    {m}_write(es(), di(), {reg}());"),
            "    set_di(((di() as i32 + delta) & 0xFFFF) as u16);".into(),
            "}".into(),
        ])
    }
    fn h_movs(&mut self, insn: &Insn, w: i64, m: &str) -> R<Vec<String>> {
        let seg = self.string_source_segment(insn);
        Ok(vec![
            "{".into(),
            format!("    let delta: i32 = if DF() != 0 {{ -{w} }} else {{ {w} }};"),
            format!("    {m}_write(es(), di(), {m}({seg}(), si()));"),
            "    set_si(((si() as i32 + delta) & 0xFFFF) as u16);".into(),
            "    set_di(((di() as i32 + delta) & 0xFFFF) as u16);".into(),
            "}".into(),
        ])
    }
    fn h_xlatb(&mut self, insn: &Insn) -> R<Vec<String>> {
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
        Ok(vec![format!(
            "set_al(memb({seg}(), ((bx() as u32).wrapping_add(al() as u32) & 0xFFFF) as u16));"
        )])
    }
    /// cmpsb/scasb/scasw — the subtract-and-set-flags string compares. Mirrors
    /// the C handlers exactly (note: C omits PF for cmpsb/scasb but sets it for
    /// scasw — matched here for byte-parity with the validated C path).
    fn h_cmp_str(&mut self, left: &str, right_seg: &str, w: i64) -> R<Vec<String>> {
        let (rw, shift, sign, set_pf) = if w == 1 {
            ("memb", 7u32, 0x80u32, false)
        } else {
            ("memw", 15, 0x8000, true)
        };
        let rtype = if w == 1 { "u8" } else { "u16" };
        let mut lines = vec![
            format!("let delta: i32 = if DF() != 0 {{ -{w} }} else {{ {w} }};"),
            "{".into(),
            format!("    let left_val: u32 = ({left}) as u32;"),
            format!("    let right_val: u32 = {rw}({right_seg}(), di()) as u32;"),
            "    set_CF((left_val < right_val) as u8);".into(),
            "    let tmp = left_val.wrapping_sub(right_val);".into(),
            format!("    let result = tmp as {rtype};"),
            "    set_ZF((result == 0) as u8);".into(),
            format!("    set_SF(((result >> {shift}) & 1) as u8);"),
        ];
        if set_pf {
            lines.push("    set_PF(parity8(result as u8));".into());
        }
        lines.push(format!(
            "    set_OF((((left_val ^ right_val) & (left_val ^ (result as u32)) & 0x{sign:X}) != 0) as u8);"
        ));
        lines.push("}".into());
        lines.push("set_di(((di() as i32 + delta) & 0xFFFF) as u16);".into());
        Ok(lines)
    }
    fn h_cmpsb(&mut self, insn: &Insn) -> R<Vec<String>> {
        let seg = self.string_source_segment(insn);
        // cmpsb compares [ds:si] vs [es:di]; left reads memb(seg,si), then si advances.
        let mut lines = vec![
            format!("let delta: i32 = if DF() != 0 {{ -1 }} else {{ 1 }};"),
            "{".into(),
            format!("    let left_val: u32 = memb({seg}(), si()) as u32;"),
            "    let right_val: u32 = memb(es(), di()) as u32;".into(),
            "    set_CF((left_val < right_val) as u8);".into(),
            "    let tmp = left_val.wrapping_sub(right_val);".into(),
            "    let result = tmp as u8;".into(),
            "    set_ZF((result == 0) as u8);".into(),
            "    set_SF(((result >> 7) & 1) as u8);".into(),
            "    set_OF((((left_val ^ right_val) & (left_val ^ (result as u32)) & 0x80) != 0) as u8);".into(),
            "}".into(),
            "set_si(((si() as i32 + delta) & 0xFFFF) as u16);".into(),
            "set_di(((di() as i32 + delta) & 0xFFFF) as u16);".into(),
        ];
        let _ = &mut lines;
        Ok(lines)
    }

    /// insb/insw — read a port into [es:di] and step di. The port is DX; the
    /// destination segment is ES and cannot be overridden.
    fn h_ins(&mut self, w: i64) -> R<Vec<String>> {
        let (m, i) = if w == 1 {
            ("memb", "inb")
        } else {
            ("memw", "inw")
        };
        Ok(vec![
            "{".into(),
            format!("    let delta: i32 = if DF() != 0 {{ -{w} }} else {{ {w} }};"),
            format!("    {m}_write(es(), di(), {i}(dx()));"),
            "    set_di(((di() as i32 + delta) & 0xFFFF) as u16);".into(),
            "}".into(),
        ])
    }
    /// outsb/outsw — write [ds:si] to the port in DX and step si. The source
    /// segment defaults to DS and *can* be overridden.
    fn h_outs(&mut self, insn: &Insn, w: i64) -> R<Vec<String>> {
        let seg = self.string_source_segment(insn);
        let (m, o) = if w == 1 {
            ("memb", "outb")
        } else {
            ("memw", "outw")
        };
        Ok(vec![
            "{".into(),
            format!("    let delta: i32 = if DF() != 0 {{ -{w} }} else {{ {w} }};"),
            format!("    {o}(dx(), {m}({seg}(), si()));"),
            "    set_si(((si() as i32 + delta) & 0xFFFF) as u16);".into(),
            "}".into(),
        ])
    }

    /// A rep'd port-string op. Unlike rep movs/stos, this cannot be collapsed into
    /// a block helper: every iteration is a separate port access, and a device
    /// hands back a different byte each time it is read (that is the whole point —
    /// it is how a game streams a sector or a palette through a single port).
    fn rep_port_string(&mut self, insn: &Insn, base: &str, w: i64) -> R<Vec<String>> {
        let inner = if base.starts_with("ins") {
            self.h_ins(w)?
        } else {
            self.h_outs(insn, w)?
        };
        let mut l = vec![
            "{".into(),
            "    JIT_BUDGET(cx() as u32); // rep iterations debit virtual time".into(),
            "    while cx() != 0 {".into(),
        ];
        for line in inner {
            l.push(format!("    {line}"));
        }
        l.push("        set_cx(cx().wrapping_sub(1));".into());
        l.push("    }".into());
        l.push("}".into());
        Ok(l)
    }

    fn handle_rep(&mut self, insn: &Insn, base: &str) -> R<Vec<String>> {
        let seg = self.string_source_segment(insn);
        match base {
            "insb" => self.rep_port_string(insn, base, 1),
            "insw" => self.rep_port_string(insn, base, 2),
            "outsb" => self.rep_port_string(insn, base, 1),
            "outsw" => self.rep_port_string(insn, base, 2),
            "movsb" => Ok(vec![format!("rep_movsb_block(es(), {seg}());")]),
            "movsw" => Ok(vec![format!("rep_movsw_block(es(), {seg}());")]),
            "stosb" => Ok(vec!["rep_stosb_block(es());".into()]),
            "stosw" => Ok(vec![
                "{".into(),
                "    JIT_BUDGET(cx() as u32); // rep iterations debit virtual time".into(),
                "    let delta: i32 = if DF() != 0 { -2 } else { 2 };".into(),
                "    while cx() != 0 {".into(),
                "        memw_write(es(), di(), ax());".into(),
                "        set_di(((di() as i32 + delta) & 0xFFFF) as u16);".into(),
                "        set_cx(cx().wrapping_sub(1));".into(),
                "    }".into(),
                "}".into(),
            ]),
            // Every iteration but the last is overwritten by the next, so the
            // observable effect of a rep lods is the final element and the walked
            // si/cx — no loop needed. (Memory is not re-read, but nothing can
            // observe that: a lods reads guest RAM, not a port.)
            "lodsb" => Ok(vec![
                "if cx() != 0 {".into(),
                "    JIT_BUDGET(cx() as u32); // rep iterations debit virtual time".into(),
                "    let delta: i32 = if DF() != 0 { -1 } else { 1 };".into(),
                format!("    set_al(memb({seg}(), ((si() as i32 + (cx() as i32 - 1) * delta) & 0xFFFF) as u16));"),
                "    set_si(((si() as i32 + cx() as i32 * delta) & 0xFFFF) as u16);".into(),
                "    set_cx(0);".into(),
                "}".into(),
            ]),
            "lodsw" => Ok(vec![
                "if cx() != 0 {".into(),
                "    JIT_BUDGET(cx() as u32); // rep iterations debit virtual time".into(),
                "    let delta: i32 = if DF() != 0 { -2 } else { 2 };".into(),
                format!("    set_ax(memw({seg}(), ((si() as i32 + (cx() as i32 - 1) * delta) & 0xFFFF) as u16));"),
                "    set_si(((si() as i32 + cx() as i32 * delta) & 0xFFFF) as u16);".into(),
                "    set_cx(0);".into(),
                "}".into(),
            ]),
            // F3 in front of a string *compare* is REPE — same encoding, same
            // semantics; only the spelling differs from one decoder to the next.
            "cmpsb" | "cmpsw" | "scasb" | "scasw" => self.handle_repe(insn, base),
            other => uns(format!("rep {other}")),
        }
    }

    /// (is_byte, width-type, mask, top-bit-shift) for a dest operand.
    fn width_of(&self, dest: &str) -> (bool, &'static str, u32, u32) {
        let is_byte = dest.starts_with("memb(") || is_byte_reg(&dest.to_lowercase());
        if is_byte {
            (true, "u8", 0xFF, 7)
        } else {
            (false, "u16", 0xFFFF, 15)
        }
    }

    fn handle_shift(&mut self, insn: &Insn) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic").to_string();
        let op_str = decode_variables(s(insn, "op_str"));
        let (dp, srcp) = self.split2(&op_str)?;
        let rw = self.t.rewrite_operands(insn, &[dp, srcp]);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        if self.t.match_rcb_access(&dest).is_some() {
            return uns(format!("{mnem} rcb"));
        }
        let (is_byte, wt, _mask, shift) = self.width_of(&dest);
        let dv = self.rvalue(&dest)?;
        let sv = self.rvalue(&src)?;
        let mut l = vec![
            "{".into(),
            format!("    let mut count: u32 = ({sv}) as u32 & 0x1F;"),
            "    if count != 0 {".into(),
            "        let orig_count = count;".into(),
        ];
        let store_into = |slf: &Self, val: &str| -> R<Vec<String>> {
            Ok(slf
                .store(&dest, val)?
                .into_iter()
                .map(|x| format!("        {x}"))
                .collect())
        };
        if mnem == "sar" {
            let st = if is_byte { "i8" } else { "i16" };
            l.push(format!("        let mut tmp: {st} = ({dv}) as {st};"));
            l.push("        while count != 0 { count -= 1;".into());
            l.push("            set_CF((tmp & 1) as u8);".into());
            l.push("            tmp >>= 1;".into());
            l.push("        }".into());
            l.extend(store_into(self, &format!("(tmp as {wt})"))?);
            l.push("        set_OF(0);".into());
        } else if mnem == "shr" {
            l.push(format!("        let mut tmp: {wt} = ({dv}) as {wt};"));
            l.push(format!(
                "        let orig_sign = ((tmp >> {shift}) & 1) as u8;"
            ));
            l.push("        while count != 0 { count -= 1;".into());
            l.push("            set_CF((tmp & 1) as u8);".into());
            l.push("            tmp >>= 1;".into());
            l.push("        }".into());
            l.extend(store_into(self, "tmp")?);
            l.push("        set_OF(if orig_count == 1 { orig_sign } else { 0 });".into());
        } else {
            // shl / sal
            l.push(format!("        let mut tmp: {wt} = ({dv}) as {wt};"));
            l.push("        while count != 0 { count -= 1;".into());
            l.push(format!("            set_CF(((tmp >> {shift}) & 1) as u8);"));
            l.push(format!("            tmp = ((tmp as u32) << 1) as {wt};"));
            l.push("        }".into());
            l.extend(store_into(self, "tmp")?);
            l.push(format!(
                "        set_OF(if orig_count == 1 {{ CF() ^ (((tmp >> {shift}) & 1) as u8) }} else {{ 0 }});"
            ));
        }
        let rv = self.rvalue(&dest)?;
        l.push(format!("        set_ZF((({rv}) == 0) as u8);"));
        l.push(format!("        set_PF(parity8(({rv}) as u8));"));
        l.push(format!("        set_SF(((({rv}) >> {shift}) & 1) as u8);"));
        l.push("    }".into());
        l.push("}".into());
        Ok(l)
    }

    fn handle_rol(&mut self, insn: &Insn) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic").to_string();
        let op_str = decode_variables(s(insn, "op_str"));
        let (dp, srcp) = self.split2(&op_str)?;
        let rw = self.t.rewrite_operands(insn, &[dp, srcp]);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        if self.t.match_rcb_access(&dest).is_some() {
            return uns(format!("{mnem} rcb"));
        }
        let (_is_byte, wt, mask, shift) = self.width_of(&dest);
        let width: u32 = shift + 1;
        let dv = self.rvalue(&dest)?;
        let sv = self.rvalue(&src)?;
        let mut l = vec![
            "{".into(),
            format!("    let count: u32 = ({sv}) as u32 & {shift};"),
            "    if count != 0 {".into(),
            format!("        let d0 = ({dv}) as u32;"),
            format!("        let v = ((d0 << count) | (d0 >> ({width} - count))) & 0x{mask:X};"),
        ];
        for x in self.store(&dest, &format!("v as {wt}"))? {
            l.push(format!("        {x}"));
        }
        let rv = self.rvalue(&dest)?;
        l.push(format!("        set_CF((({rv}) & 1) as u8);"));
        l.push(format!(
            "        set_OF(if count == 1 {{ ((({rv}) >> {shift}) & 1) as u8 ^ CF() }} else {{ 0 }});"
        ));
        l.push("    }".into());
        l.push("}".into());
        Ok(l)
    }

    fn handle_ror(&mut self, insn: &Insn) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic").to_string();
        let op_str = decode_variables(s(insn, "op_str"));
        let (dp, srcp) = self.split2(&op_str)?;
        let rw = self.t.rewrite_operands(insn, &[dp, srcp]);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        if self.t.match_rcb_access(&dest).is_some() {
            return uns(format!("{mnem} rcb"));
        }
        let (_is_byte, wt, mask, shift) = self.width_of(&dest);
        let width: u32 = shift + 1;
        let dv = self.rvalue(&dest)?;
        let sv = self.rvalue(&src)?;
        let mut l = vec![
            "{".into(),
            format!("    let count: u32 = ({sv}) as u32 & {shift};"),
            "    if count != 0 {".into(),
            format!("        let d0 = ({dv}) as u32;"),
            format!("        let v = ((d0 >> count) | (d0 << ({width} - count))) & 0x{mask:X};"),
        ];
        for x in self.store(&dest, &format!("v as {wt}"))? {
            l.push(format!("        {x}"));
        }
        let rv = self.rvalue(&dest)?;
        l.push(format!("        set_CF(((({rv}) >> {shift}) & 1) as u8);"));
        l.push(format!(
            "        set_OF(if count == 1 {{ (((({rv}) >> {shift}) & 1) ^ ((({rv}) >> {}) & 1)) as u8 }} else {{ 0 }});",
            shift.wrapping_sub(1)
        ));
        l.push("    }".into());
        l.push("}".into());
        Ok(l)
    }

    /// rcl/rcr — rotate through carry. Only CF/OF are affected.
    fn handle_rc(&mut self, insn: &Insn, is_left: bool) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic").to_string();
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        let rw = self.t.rewrite_operands(insn, &[parts[0].clone()]);
        let dest = rw[0].clone();
        if self.t.match_rcb_access(&dest).is_some() {
            return uns(format!("{mnem} rcb"));
        }
        let count_op = if parts.len() == 2 {
            parts[1].clone()
        } else {
            "1".into()
        };
        let cv = self.rvalue(&count_op)?;
        let (_is_byte, wt, mask, shift) = self.width_of(&dest);
        let width: u32 = shift + 1;
        let dv = self.rvalue(&dest)?;
        let mut l = vec![
            "{".into(),
            format!(
                "    let mut count: u32 = (({cv}) as u32 & 0x1F) % {};",
                width + 1
            ),
            "    let orig_count = count;".into(),
            format!("    let mut tmp: {wt} = ({dv}) as {wt};"),
            "    while count != 0 { count -= 1;".into(),
        ];
        if is_left {
            l.push(format!(
                "        let new_cf: u8 = ((tmp >> {shift}) & 1) as u8;"
            ));
            l.push(format!(
                "        tmp = ((((tmp as u32) << 1) | (CF() as u32)) & 0x{mask:X}) as {wt};"
            ));
        } else {
            l.push("        let new_cf: u8 = (tmp & 1) as u8;".into());
            l.push(format!("        tmp = ((((tmp as u32) >> 1) | ((CF() as u32) << {shift})) & 0x{mask:X}) as {wt};"));
        }
        l.push("        set_CF(new_cf);".into());
        l.push("    }".into());
        for x in self.store(&dest, "tmp")? {
            l.push(format!("    {x}"));
        }
        l.push("    if orig_count == 1 {".into());
        if is_left {
            l.push(format!(
                "        set_OF((((tmp >> {shift}) & 1) as u8) ^ CF());"
            ));
        } else {
            l.push(format!(
                "        set_OF((((tmp >> {shift}) & 1) ^ ((tmp >> {}) & 1)) as u8);",
                shift.wrapping_sub(1)
            ));
        }
        l.push("    }".into());
        l.push("}".into());
        Ok(l)
    }

    /// enter alloc, level — set up a stack frame (ports the retired C backend's handle_enter).
    fn handle_enter(&mut self, insn: &Insn) -> R<Vec<String>> {
        let raw = s(insn, "op_str");
        let parts: Vec<&str> = raw.split(',').map(|p| p.trim()).collect();
        let alloc = parts
            .first()
            .filter(|p| !p.is_empty())
            .and_then(|p| parse_imm(p))
            .unwrap_or(0)
            & 0xFFFF;
        let level = (parts
            .get(1)
            .filter(|p| !p.is_empty())
            .and_then(|p| parse_imm(p))
            .unwrap_or(0)
            & 0xFF)
            % 32;
        let mut l = vec![
            format!("// enter 0x{alloc:X}, {level}"),
            "set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into(),
            "memw_write(ss(), sp(), bp());".into(),
            "{".into(),
            "    let frame_temp: u16 = sp();".into(),
        ];
        for _ in 1..level {
            l.push("    set_bp((bp().wrapping_sub(2)) & 0xFFFF);".into());
            l.push("    set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into());
            l.push("    memw_write(ss(), sp(), memw(ss(), bp()));".into());
        }
        if level > 0 {
            l.push("    set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into());
            l.push("    memw_write(ss(), sp(), frame_temp);".into());
        }
        l.push("    set_bp(frame_temp);".into());
        l.push("}".into());
        if alloc != 0 {
            l.push(format!(
                "set_sp((sp().wrapping_sub(0x{alloc:X})) & 0xFFFF);"
            ));
        }
        Ok(l)
    }

    /// bound reg, dword ptr [m] — array-index bounds check (ports the retired C backend's
    /// handle_bound). On out-of-range, raise the BOUND fault (INT 5), the faithful
    /// x86 behavior — never taken by correct code.
    fn handle_bound(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let parts: Vec<String> = op_str
            .splitn(2, ',')
            .map(|p| p.trim().to_string())
            .collect();
        if parts.len() != 2 {
            return uns(format!("bound {op_str}"));
        }
        let rw = self.t.rewrite_operands(insn, &[parts[0].clone()]);
        let dest = self.rvalue(&rw[0])?;
        let inner = match parts[1]
            .strip_prefix("dword ptr [")
            .or_else(|| parts[1].strip_prefix("DWORD PTR ["))
            .and_then(|x| x.strip_suffix(']'))
        {
            Some(s) => s.trim().to_string(),
            None => return uns(format!("bound {op_str}")),
        };
        let mem = rewrite_mem_op(&format!("word ptr [{inner}]"), None);
        let (byte, seg, addr) = match parse_mem(&mem) {
            Some(x) => x,
            None => return uns(format!("bound {op_str}")),
        };
        if byte {
            return uns(format!("bound {op_str}"));
        }
        let off = render_addr(&addr).ok_or_else(|| Unsupported(format!("bound addr:{addr}")))?;
        Ok(vec![
            "{".into(),
            format!("    let lower: u16 = memw({seg}(), {off});"),
            format!(
                "    let upper: u16 = memw({seg}(), (((({off}) as u32) + 2) & 0xFFFF) as u16);"
            ),
            format!("    let value: u16 = {dest};"),
            "    if value < lower || value > upper { run_interrupt(0x05); return -1; }".into(),
            "}".into(),
        ])
    }

    fn handle_mul(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let op_str = op_str.trim().to_string();
        let rw = self.t.rewrite_operands(insn, &[op_str]);
        let operand = rw[0].clone();
        if self.t.match_rcb_access(&operand).is_some() {
            return uns("mul rcb");
        }
        let ov = self.rvalue(&operand)?;
        if writes_dx(insn) {
            Ok(vec![
                "{".into(),
                format!("    let tmp: u32 = (ax() as u32).wrapping_mul(({ov}) as u32);"),
                "    set_dx(((tmp >> 16) & 0xFFFF) as u16);".into(),
                "    set_ax((tmp & 0xFFFF) as u16);".into(),
                "}".into(),
                "set_CF((dx() != 0) as u8);".into(),
                "set_OF((dx() != 0) as u8);".into(),
            ])
        } else {
            Ok(vec![
                format!("set_ax(((al() as u16).wrapping_mul(({ov}) as u16)) & 0xFFFF);"),
                "set_CF((ah() != 0) as u8);".into(),
                "set_OF((ah() != 0) as u8);".into(),
            ])
        }
    }

    fn handle_div(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let rw = self.t.rewrite_operands(insn, &[op_str.trim().to_string()]);
        let operand = rw[0].clone();
        if self.t.match_rcb_access(&operand).is_some() {
            return uns("div rcb");
        }
        let ov = self.rvalue(&operand)?;
        // x86 DIV raises #DE (INT 0) on a zero divisor OR when the quotient does
        // not fit the destination register — NOT a truncated result. checked_div
        // covers divide-by-zero; the range guard covers quotient overflow.
        if writes_dx(insn) {
            Ok(vec![
                "{".into(),
                "    let tmp: u32 = ((dx() as u32) << 16) | (ax() as u32);".into(),
                format!("    let divisor = ({ov}) as u32;"),
                "    match tmp.checked_div(divisor) {".into(),
                "        Some(q) if q <= 0xFFFF => { set_ax((q & 0xFFFF) as u16); set_dx(((tmp % divisor) & 0xFFFF) as u16); }".into(),
                "        _ => { run_interrupt(0x00); }".into(),
                "    }".into(),
                "}".into(),
            ])
        } else {
            Ok(vec![
                "{".into(),
                "    let tmp: u16 = ax();".into(),
                format!("    let divisor = ({ov}) as u16;"),
                "    match tmp.checked_div(divisor) {".into(),
                "        Some(q) if q <= 0xFF => { set_al((q & 0xFF) as u8); set_ah(((tmp % divisor) & 0xFF) as u8); }".into(),
                "        _ => { run_interrupt(0x00); }".into(),
                "    }".into(),
                "}".into(),
            ])
        }
    }

    fn handle_idiv(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let rw = self.t.rewrite_operands(insn, &[op_str.trim().to_string()]);
        let operand = rw[0].clone();
        if self.t.match_rcb_access(&operand).is_some() {
            return uns("idiv rcb");
        }
        let ov = self.rvalue(&operand)?;
        // x86 IDIV: #DE (INT 0) on zero divisor, on INT_MIN/-1 overflow, or when
        // the signed quotient is out of the destination's range. checked_div
        // returns None for the first two; the range guard covers the third.
        if writes_dx(insn) {
            Ok(vec![
                "{".into(),
                "    let dividend: i32 = ((dx() as i16 as i32) << 16) | (ax() as i32);".into(),
                format!("    let divisor = {} as i32;", Self::signed16(&ov)),
                "    match dividend.checked_div(divisor) {".into(),
                "        Some(q) if (-32768..=32767).contains(&q) => { set_ax(q as u16); set_dx((dividend % divisor) as u16); }".into(),
                "        _ => { run_interrupt(0x00); }".into(),
                "    }".into(),
                "}".into(),
            ])
        } else {
            Ok(vec![
                "{".into(),
                "    let dividend: i16 = ax() as i16;".into(),
                format!("    let divisor = {} as i16;", Self::signed8(&ov)),
                "    match dividend.checked_div(divisor) {".into(),
                "        Some(q) if (-128..=127).contains(&q) => { set_al((q & 0xFF) as u8); set_ah((dividend % divisor) as u8); }".into(),
                "        _ => { run_interrupt(0x00); }".into(),
                "    }".into(),
                "}".into(),
            ])
        }
    }

    fn handle_imul(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let op_str = op_str.trim().to_string();
        let parts: Vec<String> = if op_str.is_empty() {
            vec![]
        } else {
            op_str
                .splitn(3, ',')
                .map(|p| p.trim().to_string())
                .collect()
        };
        if parts.len() >= 2 {
            let dest = self.t.rewrite_operands(insn, &[parts[0].clone()])[0].clone();
            let (fa, fb) = if parts.len() >= 3 {
                (
                    self.t.rewrite_operands(insn, &[parts[1].clone()])[0].clone(),
                    self.t.rewrite_operands(insn, &[parts[2].clone()])[0].clone(),
                )
            } else {
                (
                    dest.clone(),
                    self.t.rewrite_operands(insn, &[parts[1].clone()])[0].clone(),
                )
            };
            let (fav, fbv) = (self.rvalue(&fa)?, self.rvalue(&fb)?);
            let mut l = vec![
                "{".into(),
                format!(
                    "    let tmp: i32 = ({} as i32).wrapping_mul({} as i32);",
                    Self::signed16(&fav),
                    Self::signed16(&fbv)
                ),
            ];
            for x in self.store(&dest, "(tmp & 0xFFFF) as u16")? {
                l.push(format!("    {x}"));
            }
            l.push("    let v = (tmp != (tmp as i16 as i32)) as u8;".into());
            l.push("    set_CF(v); set_OF(v);".into());
            l.push("}".into());
            return Ok(l);
        }
        let rw = self.t.rewrite_operands(insn, &[op_str]);
        let operand = rw[0].clone();
        let ov = self.rvalue(&operand)?;
        if operand_width8(&operand) && !writes_dx(insn) {
            Ok(vec![
                "{".into(),
                format!(
                    "    let tmp: i16 = (al() as i8 as i16) * ({} as i16);",
                    Self::signed8(&ov)
                ),
                "    set_ax((tmp as u16) & 0xFFFF);".into(),
                "    let v = (tmp < -128 || tmp > 127) as u8;".into(),
                "    set_CF(v); set_OF(v);".into(),
                "}".into(),
            ])
        } else {
            Ok(vec![
                "{".into(),
                format!(
                    "    let tmp: i32 = (ax() as i16 as i32).wrapping_mul({} as i32);",
                    Self::signed16(&ov)
                ),
                "    set_dx(((tmp >> 16) & 0xFFFF) as u16);".into(),
                "    set_ax((tmp & 0xFFFF) as u16);".into(),
                "    let v = (tmp != (ax() as i16 as i32)) as u8;".into(),
                "    set_CF(v); set_OF(v);".into(),
                "}".into(),
            ])
        }
    }

    fn handle_not(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let op_str = op_str.trim().to_string();
        let rw = self.t.rewrite_operands(insn, &[op_str]);
        let operand = rw[0].clone();
        if self.t.match_rcb_access(&operand).is_some() {
            return uns("not rcb");
        }
        let (_is_byte, wt, mask, _shift) = self.width_of(&operand);
        let dv = self.rvalue(&operand)?;
        let mut l = self.store(
            &operand,
            &format!("((!(({dv}) as u32)) & 0x{mask:X}) as {wt}"),
        )?;
        let rv = self.rvalue(&operand)?;
        l.push(format!("set_ZF((({rv}) == 0) as u8);"));
        Ok(l)
    }

    fn handle_neg(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let op_str = op_str.trim().to_string();
        let rw = self.t.rewrite_operands(insn, &[op_str]);
        let operand = rw[0].clone();
        if self.t.match_rcb_access(&operand).is_some() {
            return uns("neg rcb");
        }
        let (is_byte, wt, mask, shift) = self.width_of(&operand);
        let sign_bit: u32 = if is_byte { 0x80 } else { 0x8000 };
        let dv = self.rvalue(&operand)?;
        let mut l = vec!["{".into(), format!("    let tmp: {wt} = ({dv}) as {wt};")];
        for x in self.store(
            &operand,
            &format!("((0u32.wrapping_sub(tmp as u32)) & 0x{mask:X}) as {wt}"),
        )? {
            l.push(format!("    {x}"));
        }
        l.push("    set_CF((tmp != 0) as u8);".into());
        let rv = self.rvalue(&operand)?;
        l.push(format!("    set_ZF((({rv}) == 0) as u8);"));
        l.push(format!("    set_PF(parity8(({rv}) as u8));"));
        l.push(format!("    set_SF(((({rv}) >> {shift}) & 1) as u8);"));
        l.push(format!("    set_OF((tmp == 0x{sign_bit:X}) as u8);"));
        l.push("}".into());
        Ok(l)
    }

    fn handle_aaa_aas(&self, is_add: bool) -> R<Vec<String>> {
        let o = if is_add {
            "wrapping_add"
        } else {
            "wrapping_sub"
        };
        Ok(vec![
            "{".into(),
            "    let tmp: u8 = al();".into(),
            "    if (tmp & 0x0F) > 9 {".into(),
            format!("        set_al(tmp.{o}(6) & 0x0F);"),
            format!("        set_ah(ah().{o}(1) & 0xFF);"),
            "        set_CF(1);".into(),
            "    } else {".into(),
            "        set_al(tmp & 0x0F);".into(),
            "        set_CF(0);".into(),
            "    }".into(),
            "}".into(),
        ])
    }
    fn handle_daa_das(&self, is_add: bool) -> R<Vec<String>> {
        let o = if is_add {
            "wrapping_add"
        } else {
            "wrapping_sub"
        };
        Ok(vec![
            "{".into(),
            "    let old_al: u8 = al();".into(),
            "    let old_cf: u8 = CF();".into(),
            "    let mut new_cf: u8 = 0;".into(),
            "    if (old_al & 0x0F) > 9 {".into(),
            format!("        set_al(al().{o}(6));"),
            "    }".into(),
            "    if old_cf != 0 || old_al > 0x99 {".into(),
            format!("        set_al(al().{o}(0x60));"),
            "        new_cf = 1;".into(),
            "    }".into(),
            "    set_CF(new_cf);".into(),
            "    set_ZF((al() == 0) as u8);".into(),
            "    set_SF((al() >> 7) & 1);".into(),
            "    set_PF(parity8(al()));".into(),
            "}".into(),
        ])
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
    fn handle_aam(&mut self, insn: &Insn) -> R<Vec<String>> {
        let base = self.aad_aam_base(insn);
        let mut l = Vec::new();
        if base != 0 {
            l.push(format!("set_ah(al() / {base}); set_al(al() % {base});"));
        }
        l.push("set_ZF((al() == 0) as u8); set_SF((al() >> 7) & 1); set_PF(parity8(al()));".into());
        Ok(l)
    }
    fn handle_aad(&mut self, insn: &Insn) -> R<Vec<String>> {
        let base = self.aad_aam_base(insn);
        Ok(vec![
            format!("set_al(((al() as u16).wrapping_add((ah() as u16).wrapping_mul({base})) & 0xFF) as u8); set_ah(0);"),
            "set_ZF((al() == 0) as u8); set_SF((al() >> 7) & 1); set_PF(parity8(al()));".into(),
        ])
    }

    fn handle_io(&mut self, insn: &Insn) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic");
        let op_str = decode_variables(s(insn, "op_str"));
        let (a, b) = self.split2(&op_str)?;
        if mnem == "in" {
            let dl = a.to_lowercase();
            if dl == "al" || dl == "ax" {
                let func = if dl == "al" { "inb" } else { "inw" };
                let port = self.rvalue(&b)?;
                return Ok(vec![format!("set_{dl}(unsafe {{ {func}({port}) }});")]);
            }
        }
        if mnem == "out" {
            let sl = b.to_lowercase();
            if sl == "al" || sl == "ax" {
                let func = if sl == "al" { "outb" } else { "outw" };
                let port = self.rvalue(&a)?;
                return Ok(vec![format!("unsafe {{ {func}({port}, {sl}()); }}")]);
            }
        }
        uns(format!("{mnem} {op_str}"))
    }

    fn handle_pushf(&mut self) -> R<Vec<String>> {
        Ok(vec![
            "set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into(),
            "memw_write(ss(), sp(), 0x0002u16 | CF() as u16 | ((PF() as u16) << 2) | ((ZF() as u16) << 6) | ((SF() as u16) << 7) | ((IF() as u16) << 9) | ((DF() as u16) << 10) | ((OF() as u16) << 11));".into(),
        ])
    }
    fn handle_popf(&mut self) -> R<Vec<String>> {
        // POPF does NOT create an interrupt shadow. Only STI, MOV SS and POP SS
        // inhibit interrupt recognition for the following instruction; after a
        // POPF that enables IF, a pending maskable interrupt is recognized
        // immediately. (Setting the shadow here was the same unfaithfulness as
        // the IRET case — under the per-block safepoint model a POPF-in-loop
        // would re-arm it between the rare safepoints and starve IRQ delivery.)
        Ok(vec![
            "{".into(),
            "    let flags = memw(ss(), sp());".into(),
            "    set_CF((flags & 0x0001) as u8);".into(),
            "    set_PF(((flags >> 2) & 1) as u8);".into(),
            "    set_ZF(((flags >> 6) & 1) as u8);".into(),
            "    set_SF(((flags >> 7) & 1) as u8);".into(),
            "    set_IF(((flags >> 9) & 1) as u8);".into(),
            "    set_DF(((flags >> 10) & 1) as u8);".into(),
            "    set_OF(((flags >> 11) & 1) as u8);".into(),
            "    set_sp((sp().wrapping_add(2)) & 0xFFFF);".into(),
            // POPF can raise IF, and unlike STI it creates no shadow: a waiting
            // interrupt is recognizable at the very next boundary. Arm it.
            "    irq_arm();".into(),
            "}".into(),
        ])
    }
    fn handle_pushaw(&mut self) -> R<Vec<String>> {
        let mut lines = vec!["{".into(), "    let orig_sp = sp();".into()];
        for reg in ["ax", "cx", "dx", "bx"] {
            lines.push("    set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into());
            lines.push(format!("    memw_write(ss(), sp(), {reg}());"));
        }
        lines.push("    set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into());
        lines.push("    memw_write(ss(), sp(), orig_sp);".into());
        for reg in ["bp", "si", "di"] {
            lines.push("    set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into());
            lines.push(format!("    memw_write(ss(), sp(), {reg}());"));
        }
        lines.push("}".into());
        Ok(lines)
    }
    fn handle_popaw(&mut self) -> R<Vec<String>> {
        let mut lines = vec!["{".into()];
        for reg in ["di", "si", "bp"] {
            lines.push(format!("    set_{reg}(memw(ss(), sp()));"));
            lines.push("    set_sp((sp().wrapping_add(2)) & 0xFFFF);".into());
        }
        lines
            .push("    set_sp((sp().wrapping_add(2)) & 0xFFFF); // saved SP slot discarded".into());
        for reg in ["bx", "dx", "cx", "ax"] {
            lines.push(format!("    set_{reg}(memw(ss(), sp()));"));
            lines.push("    set_sp((sp().wrapping_add(2)) & 0xFFFF);".into());
        }
        lines.push("}".into());
        Ok(lines)
    }
    fn handle_leave(&mut self) -> R<Vec<String>> {
        Ok(vec![
            "set_sp(bp());".into(),
            "set_bp(memw(ss(), sp()));".into(),
            "set_sp((sp().wrapping_add(2)) & 0xFFFF);".into(),
        ])
    }
    fn handle_xchg(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let (a, b) = self.split2(&op_str)?;
        let rw = self.t.rewrite_operands(insn, &[a, b]);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        if dest == src {
            return Ok(Vec::new());
        }
        if self.t.match_rcb_access(&dest).is_some() || self.t.match_rcb_access(&src).is_some() {
            return uns("xchg rcb");
        }
        let is_byte = dest.starts_with("memb(")
            || is_byte_reg(&dest.to_lowercase())
            || src.starts_with("memb(")
            || is_byte_reg(&src.to_lowercase());
        let wt = if is_byte { "u8" } else { "u16" };
        let dv = self.rvalue(&dest)?;
        let sv = self.rvalue(&src)?;
        let mut lines = vec!["{".into(), format!("    let tmp: {wt} = {dv};")];
        for l in self.store(&dest, &sv)? {
            lines.push(format!("    {l}"));
        }
        for l in self.store(&src, "tmp")? {
            lines.push(format!("    {l}"));
        }
        lines.push("}".into());
        Ok(lines)
    }

    /// Interrupt. Mirrors handle_interrupt's *fallback* path (register-based
    /// dispatch): faithful here because Rust movs write registers immediately,
    /// so dos_api()/run_interrupt() read the correct cpu state — no need for the
    /// C path's arg reconstruction / pending_dos deferral.
    fn handle_interrupt(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op = s(insn, "op_str").to_lowercase();
        let op = op.trim().trim_end_matches('h');
        let n = i64::from_str_radix(op.trim_start_matches("0x"), 16).ok();
        match n {
            Some(0x21) => self.simple(&["dos_api();"]),
            Some(0x20) => self.simple(&["dos_exit();"]),
            Some(0x16) => self.simple(&["bios_keyboard();"]),
            // Not `run_interrupt(n); <next instruction>`: that hard-codes both
            // where the handler returns to and what is there when it does. An
            // IRET goes to the cs:ip on the stack, and the handler may have
            // changed it — or overwritten the code we were translated from, which
            // is exactly what an overlay manager (INT 3Fh) does to the stub that
            // called it. Either way, leave and let the machine re-resolve.
            Some(n) => {
                let next_ip =
                    (i64f(insn, "address").unwrap_or(0) + insn_len(insn) + self.cs_base) & 0xFFFF;
                Ok(vec![format!(
                    "if run_interrupt_resume(0x{n:02X}, 0x{next_ip:04X}) != 0 {{ return -1; }}"
                )])
            }
            None => uns(format!("int {}", s(insn, "op_str"))),
        }
    }

    /// repe/repz (ZF=1) and repne/repnz (ZF=0) over the string compares
    /// cmpsb/cmpsw/scasb/scasw. One loop serves both: each iteration compares,
    /// advances di (and si, for cmps), and counts down cx; the prefixes differ
    /// only in which ZF ends the run, so `break_when_equal` is the whole of it.
    /// cx=0 on entry executes no iteration and leaves the flags untouched.
    fn handle_rep_cmp(
        &mut self,
        insn: &Insn,
        base: &str,
        break_when_equal: bool,
    ) -> R<Vec<String>> {
        let seg = self.string_source_segment(insn);
        let (w, rd, shift, sign): (i64, &str, u32, u32) = match base {
            "cmpsb" | "scasb" => (1, "memb", 7, 0x80),
            "cmpsw" | "scasw" => (2, "memw", 15, 0x8000),
            other => {
                let prefix = if break_when_equal { "repne" } else { "repe" };
                return uns(format!("{prefix} {other}"));
            }
        };
        let rt = if w == 1 { "u8" } else { "u16" };
        // left value: cmps reads [seg:si]; scas uses al/ax.
        let left = if base.starts_with("cmp") {
            format!("{rd}({seg}(), si())")
        } else if w == 1 {
            "al()".into()
        } else {
            "ax()".into()
        };
        let advance_si = base.starts_with("cmp");
        let mut l = vec![
            "{".into(),
            format!("    let delta: i32 = if DF() != 0 {{ -{w} }} else {{ {w} }};"),
            "    let count: u16 = cx();".into(),
            "    let mut i: u16 = 0;".into(),
            format!("    let mut lv: {rt} = 0; let mut rv: {rt} = 0;"),
            "    while i < count {".into(),
            format!("        lv = {left};"),
            format!("        rv = {rd}(es(), di());"),
        ];
        if advance_si {
            l.push("        set_si(((si() as i32 + delta) & 0xFFFF) as u16);".into());
        }
        l.push("        set_di(((di() as i32 + delta) & 0xFFFF) as u16);".into());
        l.push("        i += 1;".into());
        l.push(
            if break_when_equal {
                "        if lv == rv { break; }" // ZF->1 ends repne
            } else {
                "        if lv != rv { break; }" // ZF->0 ends repe
            }
            .into(),
        );
        l.push("    }".into());
        l.push(format!(
            "    JIT_BUDGET((i as u32).wrapping_mul({})); // rep iterations debit virtual time",
            if advance_si { 3 } else { 2 }
        ));
        l.push("    set_cx(count.wrapping_sub(i));".into());
        l.push("    if i > 0 {".into());
        l.push("        let l32 = lv as u32; let r32 = rv as u32;".into());
        l.push(format!("        let res = l32.wrapping_sub(r32) as {rt};"));
        l.push("        set_CF((l32 < r32) as u8);".into());
        l.push("        set_ZF((res == 0) as u8);".into());
        l.push("        set_PF(parity8(res as u8));".into());
        l.push(format!("        set_SF(((res >> {shift}) & 1) as u8);"));
        l.push(format!(
            "        set_OF((((l32 ^ r32) & (l32 ^ (res as u32)) & 0x{sign:X}) != 0) as u8);"
        ));
        l.push("    }".into());
        l.push("}".into());
        Ok(l)
    }

    /// Is this string op one whose repeat can end early on ZF? Only the compares
    /// set flags, so only they test one. On every other string op the F2 and F3
    /// prefixes mean the same thing — repeat CX times — because there is no ZF
    /// for the repeat to look at. MechWarrior clears a buffer with `repne stosw`,
    /// which is `rep stosw`, and refusing it was refusing a plain REP.
    fn rep_tests_zf(base: &str) -> bool {
        matches!(base, "cmpsb" | "cmpsw" | "scasb" | "scasw")
    }

    /// repe/repz cmpsb/cmpsw/scasb/scasw — repeat while equal (ZF=1).
    fn handle_repe(&mut self, insn: &Insn, base: &str) -> R<Vec<String>> {
        if !Self::rep_tests_zf(base) {
            return self.handle_rep(insn, base);
        }
        self.handle_rep_cmp(insn, base, false)
    }

    /// repne/repnz cmpsb/cmpsw/scasb/scasw — repeat while not equal (ZF=0).
    fn handle_repne(&mut self, insn: &Insn, base: &str) -> R<Vec<String>> {
        if !Self::rep_tests_zf(base) {
            return self.handle_rep(insn, base);
        }
        if base != "scasb" {
            return self.handle_rep_cmp(insn, base, true);
        }
        // scasb scans for al — the memchr/strlen idiom, kept on the native block
        // helper rather than the generic per-byte loop.
        Ok(vec![
            "{".into(),
            "    let delta: i32 = if DF() != 0 { -1 } else { 1 };".into(),
            "    let count: u16 = cx();".into(),
            "    let mut last_byte: u8 = 0;".into(),
            "    let index = unsafe { scanMemoryForAl(seg_off(es(), di()) as *const u8, al(), count, delta, &mut last_byte) };".into(),
            "    let advance = if index < count { index + 1 } else { index };".into(),
            "    JIT_BUDGET((advance as u32).wrapping_mul(2)); // rep iterations debit virtual time".into(),
            "    if advance > 0 {".into(),
            "        let l32 = al() as u32; let r32 = last_byte as u32;".into(),
            "        let res = l32.wrapping_sub(r32) as u8;".into(),
            "        set_CF((l32 < r32) as u8);".into(),
            "        set_ZF((res == 0) as u8);".into(),
            "        set_PF(parity8(res));".into(),
            "        set_SF(((res >> 7) & 1) as u8);".into(),
            "        set_OF((((l32 ^ r32) & (l32 ^ (res as u32)) & 0x80) != 0) as u8);".into(),
            "    }".into(),
            "    set_cx(count.wrapping_sub(advance));".into(),
            "    set_di(((di() as i32 + advance as i32 * delta) & 0xFFFF) as u16);".into(),
            "}".into(),
        ])
    }

    fn handle_loop(&mut self, insn: &Insn) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic");
        let target = match parse_imm(s(insn, "op_str")) {
            Some(t) => t,
            None => return uns(format!("{mnem} {}", s(insn, "op_str"))),
        };
        let mut cond = "cx() != 0".to_string();
        if matches!(mnem, "loopne" | "loopnz") {
            cond += " && ZF() == 0";
        } else if matches!(mnem, "loope" | "loopz") {
            cond += " && ZF() == 1";
        }
        Ok(vec![
            "set_cx(cx().wrapping_sub(1));".into(),
            format!("if {cond} {{"),
            format!("    return 0x{:04X};", target & 0xFFFF),
            "}".into(),
        ])
    }

    /// (off, off+2) as u16 Rust exprs for a far-pointer memory offset.
    fn off_plus_2(&self, addr: &str) -> R<(String, String)> {
        let a = addr.trim();
        if let Some(v) = parse_hex16(a).or_else(|| a.parse::<i64>().ok()) {
            return Ok((
                format!("0x{:04X}", v & 0xFFFF),
                format!("0x{:04X}", (v + 2) & 0xFFFF),
            ));
        }
        // strip an outer `(X) & 0xFFFF` to get the raw effective-address expr
        let inner = a
            .strip_suffix("& 0xFFFF")
            .map(|x| {
                let x = x.trim();
                x.strip_prefix('(')
                    .and_then(|y| y.strip_suffix(')'))
                    .unwrap_or(x)
            })
            .unwrap_or(a);
        let e = render_addr_expr(inner).ok_or_else(|| Unsupported(format!("far-off {addr}")))?;
        Ok((
            format!("(({e}) & 0xFFFF) as u16"),
            format!("(({e}.wrapping_add(2)) & 0xFFFF) as u16"),
        ))
    }

    /// lds/les — load far pointer (dest <- [mem], seg_reg <- [mem+2]).
    fn handle_load_far(&mut self, insn: &Insn, seg_reg: &str) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let (dest, mut src) = self.split2(&op_str)?;
        let dl = dest.to_lowercase();
        if !is_word_reg(&dl) {
            return uns(format!("l{seg_reg} dest {dest}"));
        }
        if src.to_lowercase().starts_with("ptr ") {
            src = src[4..].trim().to_string();
        }
        // bp/sp-relative -> var_X
        if let Some(cap) = var_re().captures(&src.to_lowercase()) {
            let mr = insn
                .get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first());
            let (mseg, disp) = match mr {
                Some(m) => (
                    m.get("segment")
                        .and_then(Value::as_str)
                        .unwrap_or("ss")
                        .to_lowercase(),
                    m.get("disp").and_then(Value::as_i64).unwrap_or(0),
                ),
                None => ("ss".into(), -i64::from_str_radix(&cap[1], 16).unwrap_or(0)),
            };
            let off1 = format!("((bp().wrapping_add(0x{:04X})) & 0xFFFF)", disp & 0xFFFF);
            let off2 = format!(
                "((bp().wrapping_add(0x{:04X})) & 0xFFFF)",
                (disp + 2) & 0xFFFF
            );
            return Ok(vec![
                "{".into(),
                format!("    let _far_seg = memw({mseg}(), {off2});"),
                format!("    set_{dl}(memw({mseg}(), {off1}));"),
                format!("    set_{seg_reg}(_far_seg);"),
                "}".into(),
            ]);
        }
        // [mem] form (with optional seg prefix)
        let mem = rewrite_mem_op(&format!("word ptr {src}"), None);
        let (_, mseg, addr) =
            parse_mem(&mem).ok_or_else(|| Unsupported(format!("l{seg_reg} {src}")))?;
        let (off1, off2) = self.off_plus_2(&addr)?;
        Ok(vec![
            "{".into(),
            format!("    let _far_seg = memw({mseg}(), {off2});"),
            format!("    set_{dl}(memw({mseg}(), {off1}));"),
            format!("    set_{seg_reg}(_far_seg);"),
            "}".into(),
        ])
    }

    fn handle_lea(&mut self, insn: &Insn) -> R<Vec<String>> {
        let op_str = decode_variables(s(insn, "op_str"));
        let (dest, src) = self.split2(&op_str)?;
        let dl = dest.to_lowercase();
        if !is_word_reg(&dl) {
            return uns(format!("lea dest {dest}"));
        }
        let src_l = src.to_lowercase();
        // bp/sp-relative -> var_X (disp from mem_refs).
        if let Some(cap) = var_re().captures(&src_l) {
            let disp = insn
                .get("detail")
                .and_then(|d| d.get("mem_refs"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|m| m.get("disp").and_then(Value::as_i64))
                .unwrap_or_else(|| -i64::from_str_radix(&cap[1], 16).unwrap_or(0));
            return Ok(vec![format!(
                "set_{dl}(((bp() as u32).wrapping_add(0x{:X}) & 0xFFFF) as u16);",
                disp & 0xFFFF
            )]);
        }
        if let Some(inner) = src.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            let e = render_addr_expr(inner.trim())
                .ok_or_else(|| Unsupported(format!("lea addr {inner}")))?;
            return Ok(vec![format!("set_{dl}((({e}) & 0xFFFF) as u16);")]);
        }
        let v = parse_hex16(&src).or_else(|| src.parse::<i64>().ok());
        match v {
            Some(v) => Ok(vec![format!("set_{dl}(0x{:04X});", v & 0xFFFF)]),
            None => uns(format!("lea {op_str}")),
        }
    }

    fn handle_call(&mut self, insn: &Insn) -> R<Vec<String>> {
        // A near transfer's destination is an offset in the current segment, and
        // it wraps there. Capstone reports address+disp un-wrapped, so a jump or
        // call backwards from low in the segment comes back as 0xFFFFFFxx — which
        // is not an ip, does not fit the i32 a block key is, and used to be masked
        // only for the one instruction length someone had seen it on. Mask it: the
        // CPU does, and an ip is what a block key means.
        let target = i64f(insn, "target")
            .or_else(|| parse_imm(s(insn, "op_str")))
            .map(|t| t & 0xFFFF);
        let ret_arg = self.call_return_arg(insn);
        if let Some(t) = target {
            if self.known_funcs.contains(&t) {
                return Ok(vec![
                    "{".into(),
                    "    set_sp((sp().wrapping_sub(2)) & 0xFFFF);".into(),
                    format!("    memw_write(ss(), sp(), {ret_arg});"),
                    format!("    return 0x{:04X};", t & 0xFFFF),
                    "}".into(),
                ]);
            }
            // Direct near-call to a target this chunk didn't decode (a different
            // segment, or code past the decode limit): dispatch it through the
            // call table at the live cs-relative linear address, exactly like the
            // register/memory-indirect path below. call_table_impl pushes nothing
            // (ret_arg IS the pushed return IP) and JIT-compiles the target on
            // reach. Faithful to a near call that leaves the decoded region.
            return Ok(vec![format!(
                "call_table_({ret_arg}, (((cs() as u32) << 4).wrapping_add(0x{t:04X})) & 0xFFFFF);"
            )]);
        }
        // Indirect call (register or memory operand).
        let op_str = decode_variables(s(insn, "op_str"));
        let op = op_str.to_lowercase();
        let op = op.trim();
        if WORD_REGS.contains(&op) {
            return Ok(vec![format!(
                "call_table_({ret_arg}, (((cs() as u32) << 4).wrapping_add({op}() as u32)) & 0xFFFFF);"
            )]);
        }
        // Memory-indirect: reuse rewrite_mem_op via rewrite_operands, then lower.
        let rw = self.t.rewrite_operands(insn, &[op_str.clone()]);
        let mem = rw[0].clone();
        if let Ok(memv) = self.rvalue(&mem) {
            return Ok(vec![format!(
                "call_table_({ret_arg}, (((cs() as u32) << 4).wrapping_add(({memv}) as u32)) & 0xFFFFF);"
            )]);
        }
        uns(format!("call {op_str}"))
    }

    /// (is_byte, mask, sign, shift, store-cast-type, read-back-expr) for an
    /// arithmetic destination operand.
    fn dest_meta(&self, dest: &str) -> R<(u32, u32, u32, &'static str, String)> {
        let is_byte = dest.starts_with("memb(") || is_byte_reg(&dest.to_lowercase());
        let dr = self.rvalue(dest)?;
        if is_byte {
            Ok((0xFF, 0x80, 7, "u8", dr))
        } else {
            Ok((0xFFFF, 0x8000, 15, "u16", dr))
        }
    }

    /// Emit `set_ZF/PF/SF` from a result read-back expression (byte-truncated).
    fn flags_zpf_sf(&self, lines: &mut Vec<String>, dr: &str, shift: u32) {
        lines.push(format!("set_ZF((({dr}) == 0) as u8);"));
        lines.push(format!("set_PF(parity8(({dr}) as u8));"));
        lines.push(format!("set_SF(((({dr}) >> {shift}) & 1) as u8);"));
    }

    fn handle_arithmetic(&mut self, insn: &Insn) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic").to_string();
        let op_str = decode_variables(s(insn, "op_str"));

        if matches!(mnem.as_str(), "inc" | "dec") {
            let rw = self.t.rewrite_operands(insn, &[op_str.trim().to_string()]);
            let operand = rw[0].clone();
            let (mask, sign, shift, wt, dr) = self.dest_meta(&operand)?;
            let is_inc = mnem == "inc";
            let opc = if is_inc {
                "wrapping_add"
            } else {
                "wrapping_sub"
            };
            let limit = if is_inc {
                if mask == 0xFF {
                    0x7Fu32
                } else {
                    0x7FFF
                }
            } else {
                sign
            };
            let mut lines = vec!["{".into(), format!("    let old: u32 = ({dr}) as u32;")];
            let store = self.store(&operand, &format!("(old.{opc}(1) & 0x{mask:X}) as {wt}"))?;
            for l in store {
                lines.push(format!("    {l}"));
            }
            let mut tail = Vec::new();
            self.flags_zpf_sf(&mut tail, &dr, shift);
            for l in tail {
                lines.push(format!("    {l}"));
            }
            lines.push(format!("    set_OF((old == 0x{limit:X}) as u8);"));
            lines.push("}".into());
            return Ok(lines);
        }

        let (d, sp) = self.split2(&op_str)?;
        let rw = self.t.rewrite_operands(insn, &[d, sp]);
        let (dest, src) = (rw[0].clone(), rw[1].clone());
        // RCB / unrenderable operands -> fall back to C.
        if self.t.match_rcb_access(&dest).is_some() || self.t.match_rcb_access(&src).is_some() {
            return uns(format!("{mnem} rcb"));
        }
        let (mask, sign, shift, wt, dr) = self.dest_meta(&dest)?;

        match mnem.as_str() {
            "xor" if dest == src => {
                // `xor x,x` zeroes the operand; flags of a zero result:
                // CF=0 OF=0 ZF=1 SF=0 PF=parity8(0)=1.
                let mut lines = if dest.starts_with("memb(") || dest.starts_with("memw(") {
                    self.store(&dest, "0")?
                } else {
                    vec![format!("set_{}(0);", dest.to_lowercase())]
                };
                for f in [
                    "set_CF(0);",
                    "set_OF(0);",
                    "set_ZF(1);",
                    "set_PF(1);",
                    "set_SF(0);",
                ] {
                    lines.push(f.into());
                }
                Ok(lines)
            }
            "add" | "adc" | "sub" | "sbb" => {
                let srcv = self.rvalue(&src)?;
                let mut lines = vec!["{".into(), format!("    let old: u32 = ({dr}) as u32;")];
                match mnem.as_str() {
                    "add" => {
                        lines.push(format!("    let src: u32 = ({srcv}) as u32;"));
                        lines.push("    let tmp: u32 = old.wrapping_add(src);".into());
                        lines.push(format!("    set_CF((tmp > 0x{mask:X}) as u8);"));
                    }
                    "adc" => {
                        lines.push(format!("    let src: u32 = ({srcv}) as u32;"));
                        lines.push(
                            "    let tmp: u32 = old.wrapping_add(src).wrapping_add(CF() as u32);"
                                .into(),
                        );
                        lines.push(format!("    set_CF((tmp > 0x{mask:X}) as u8);"));
                    }
                    "sub" => {
                        lines.push(format!("    let src: u32 = ({srcv}) as u32;"));
                        lines.push("    set_CF((old < src) as u8);".into());
                        lines.push("    let tmp: u32 = old.wrapping_sub(src);".into());
                    }
                    _ => {
                        // sbb
                        lines.push(format!(
                            "    let src: u32 = ({srcv} as u32).wrapping_add(CF() as u32);"
                        ));
                        lines.push("    set_CF((old < src) as u8);".into());
                        lines.push("    let tmp: u32 = old.wrapping_sub(src);".into());
                    }
                }
                let store = self.store(&dest, &format!("(tmp & 0x{mask:X}) as {wt}"))?;
                for l in store {
                    lines.push(format!("    {l}"));
                }
                let is_addlike = matches!(mnem.as_str(), "add" | "adc");
                if is_addlike {
                    lines.push(format!(
                        "    set_OF(((!(old ^ src) & (old ^ tmp) & 0x{sign:X}) != 0) as u8);"
                    ));
                } else {
                    lines.push(format!(
                        "    set_OF((((old ^ src) & (old ^ tmp) & 0x{sign:X}) != 0) as u8);"
                    ));
                }
                lines.push("}".into());
                self.flags_zpf_sf(&mut lines, &dr, shift);
                Ok(lines)
            }
            "and" | "or" | "xor" => {
                let srcv = self.rvalue(&src)?;
                let op = match mnem.as_str() {
                    "and" => '&',
                    "or" => '|',
                    _ => '^',
                };
                let mut lines = vec![
                    "{".into(),
                    format!("    let tmp: {wt} = ((({dr}) as u32 {op} ({srcv}) as u32) & 0x{mask:X}) as {wt};"),
                ];
                let store = self.store(&dest, "tmp")?;
                for l in store {
                    lines.push(format!("    {l}"));
                }
                lines.push("    set_CF(0);".into());
                lines.push("    set_OF(0);".into());
                lines.push("    set_ZF((tmp == 0) as u8);".into());
                lines.push("    set_PF(parity8(tmp as u8));".into());
                lines.push(format!("    set_SF(((tmp >> {shift}) & 1) as u8);"));
                lines.push("}".into());
                Ok(lines)
            }
            other => uns(format!("arith:{other}")),
        }
    }

    /// Dispatch on mnemonic. Unported mnemonics -> Unsupported (fall back to C).
    fn format_instruction(&mut self, insn: &Insn) -> R<Vec<String>> {
        let mnem = s(insn, "mnemonic");
        // Prefixed string ops arrive as a space-joined mnemonic ("rep movsb").
        if let Some((prefix, base)) = mnem.split_once(' ') {
            return match prefix {
                "rep" => self.handle_rep(insn, base),
                "repe" | "repz" => self.handle_repe(insn, base),
                "repne" | "repnz" => self.handle_repne(insn, base),
                // LOCK asserts the bus lock for the duration of the instruction,
                // which is a promise made to *other bus masters* — that no one
                // else reads or writes the line half-way through. There is no one
                // else: one CPU, and a translated instruction is a single step on
                // a single thread. The instruction under the prefix is unchanged,
                // operands and flags alike, so emit it.
                "lock" => {
                    let mut base_insn = insn.clone();
                    base_insn.insert("mnemonic".into(), Value::String(base.to_string()));
                    self.format_instruction(&base_insn)
                }
                other => uns(format!("prefix:{other} {base}")),
            };
        }
        match mnem {
            "nop" | "db" => Ok(Vec::new()),
            "lodsb" => self.h_lods(insn, 1, "al", "memb"),
            "lodsw" => self.h_lods(insn, 2, "ax", "memw"),
            "stosb" => self.h_stos(1, "al", "memb"),
            "stosw" => self.h_stos(2, "ax", "memw"),
            "movsb" => self.h_movs(insn, 1, "memb"),
            "movsw" => self.h_movs(insn, 2, "memw"),
            "cmpsb" => self.h_cmpsb(insn),
            "scasb" => self.h_cmp_str("al()", "es", 1),
            "scasw" => self.h_cmp_str("ax()", "es", 2),
            "insb" => self.h_ins(1),
            "insw" => self.h_ins(2),
            "outsb" => self.h_outs(insn, 1),
            "outsw" => self.h_outs(insn, 2),
            "xlatb" => self.h_xlatb(insn),
            "ret" | "retn" | "retf" => self.handle_ret(insn),
            "mov" => self.handle_mov(insn),
            "cmp" => self.handle_cmp_test(insn, false),
            "test" => self.handle_cmp_test(insn, true),
            "push" => self.handle_push(insn),
            "pop" => self.handle_pop(insn),
            "cld" => self.simple(&["set_DF(0);"]),
            "std" => self.simple(&["set_DF(1);"]),
            "clc" => self.simple(&["set_CF(0);"]),
            "cmc" => self.simple(&["set_CF(CF() ^ 1);"]),
            "stc" => self.simple(&["set_CF(1);"]),
            "cli" => self.simple(&["set_IF(0);"]),
            // STI raises IF, so a waiting interrupt becomes recognizable at the
            // next instruction boundary — arm the safepoint (irq_arm) or the
            // recognition point is left to wherever the budget happens to
            // expire, which a `cli`..`sti` loop can capture in its IF=0 window
            // forever. The shadow still suppresses the boundary immediately
            // after STI itself.
            "sti" => self.simple(&["set_IF(1);", "set_interrupt_shadow(1);", "irq_arm();"]),
            "cwde" => self.simple(&["set_ax(((al() as i8) as i16) as u16);"]),
            "cdq" => self.simple(&["set_dx(if (ax() & 0x8000) != 0 { 0xFFFF } else { 0 });"]),
            "iret" => self.simple(&["iret_();"]),
            "lahf" => {
                self.simple(&["set_ah((SF() << 7) | (ZF() << 6) | (PF() << 2) | 0x02 | CF());"])
            }
            "sahf" => self.simple(&[
                "set_SF((ah() >> 7) & 1);",
                "set_ZF((ah() >> 6) & 1);",
                "set_PF((ah() >> 2) & 1);",
                "set_CF(ah() & 1);",
            ]),
            "salc" => self.simple(&["set_al(if CF() != 0 { 0xFF } else { 0x00 });"]),
            "aaa" => self.handle_aaa_aas(true),
            "aas" => self.handle_aaa_aas(false),
            "daa" => self.handle_daa_das(true),
            "das" => self.handle_daa_das(false),
            "aam" => self.handle_aam(insn),
            "aad" => self.handle_aad(insn),
            "add" | "adc" | "sub" | "sbb" | "and" | "or" | "xor" | "inc" | "dec" => {
                self.handle_arithmetic(insn)
            }
            "call" => self.handle_call(insn),
            "loop" | "loopne" | "loopnz" | "loope" | "loopz" => self.handle_loop(insn),
            "lea" => self.handle_lea(insn),
            "int" => self.handle_interrupt(insn),
            "lcall" => self.handle_lcall(insn),
            "ljmp" => self.handle_ljmp(insn),
            "lds" => self.handle_load_far(insn, "ds"),
            "les" => self.handle_load_far(insn, "es"),
            "in" | "out" => self.handle_io(insn),
            "pushf" => self.handle_pushf(),
            "popf" => self.handle_popf(),
            "pushaw" => self.handle_pushaw(),
            "popaw" => self.handle_popaw(),
            "leave" => self.handle_leave(),
            "xchg" => self.handle_xchg(insn),
            "shl" | "shr" | "sar" | "sal" => self.handle_shift(insn),
            "rol" => self.handle_rol(insn),
            "ror" => self.handle_ror(insn),
            "rcl" => self.handle_rc(insn, true),
            "rcr" => self.handle_rc(insn, false),
            "mul" => self.handle_mul(insn),
            "imul" => self.handle_imul(insn),
            "div" => self.handle_div(insn),
            "idiv" => self.handle_idiv(insn),
            "not" => self.handle_not(insn),
            "neg" => self.handle_neg(insn),
            "enter" => self.handle_enter(insn),
            "bound" => self.handle_bound(insn),
            // hlt: wait for the next interrupt (the `sti; hlt` idle-loop
            // idiom). HLT_WAIT lets machine time flow at host pace while the
            // guest retires nothing, then services pending IRQs; the C backend
            // treated hlt as a block terminator, so nothing else follows in-block.
            "hlt" => self.simple(&["HLT_WAIT();"]),
            "cbw" => self.simple(&["set_ax(((al() as i8) as i16) as u16);"]),
            "cwd" => self.simple(&["set_dx(if (ax() & 0x8000) != 0 { 0xFFFF } else { 0 });"]),
            other => uns(format!("mnemonic:{other}")),
        }
    }

    // ---- state machine (mirror render_block_state_machine) -----------------

    /// Render one basic block as the body of its per-block `fn … -> c_int`.
    /// Control transfers `return` the next pc (or -1: dispatch returns) instead
    /// of mutating a shared `pc` — each block being its own small fn keeps
    /// rustc's per-body analyses (borrowck is superlinear) off the JIT path.
    fn render_block(
        &mut self,
        block: &BasicBlock,
        succ: &HashMap<i64, Vec<i64>>,
    ) -> R<Vec<String>> {
        let indent = "    ";
        let mut lines: Vec<String> = Vec::new();
        let insns = &block.instructions;
        let n = insns.len();

        for (idx, insn) in insns.iter().enumerate() {
            let ip = (i64f(insn, "address").unwrap_or(0) + self.cs_base) & 0xFFFF;
            lines.push(format!("{indent}set_ip(0x{ip:04X});"));
            if idx == 0 {
                // One safepoint poll per basic block, debiting the block's
                // summed per-class instruction weights (insn_weight): IRQs
                // deliver on block boundaries (still instruction boundaries),
                // and the budget is what advances the virtual clock.
                let cost: u32 = insns.iter().map(insn_weight).sum();
                lines.push(format!("{indent}JIT_BUDGET({cost});"));
            }

            match self.render_insn(insn, idx, n, block, succ) {
                Ok((out, block_ends)) => {
                    lines.extend(out);
                    if block_ends {
                        return Ok(lines);
                    }
                }
                Err(Unsupported(what)) => {
                    // Bytes the translator cannot express. They may well never
                    // execute — a packed game's CFG runs into its own ciphertext,
                    // which the stub rewrites into real code before jumping there —
                    // so the chunk still compiles and the gap becomes a *run-time*
                    // one, paid only if control actually arrives. set_ip is already
                    // emitted above, so the crash names the exact instruction.
                    self.unsupported.push(what.clone());
                    lines.push(format!(
                        "{indent}jit_unsupported_instruction({});",
                        c_string_literal(&what)
                    ));
                    lines.push(format!("{indent}return -1;"));
                    return Ok(lines);
                }
            }
        }
        Ok(lines)
    }

    /// Render one instruction of a block. The bool is "this instruction ended
    /// the block" — it transferred control and already emitted its `return`.
    fn render_insn(
        &mut self,
        insn: &Insn,
        idx: usize,
        n: usize,
        block: &BasicBlock,
        succ: &HashMap<i64, Vec<i64>>,
    ) -> R<(Vec<String>, bool)> {
        let indent = "    ";
        let mut out: Vec<String> = Vec::new();
        let succs = |a: i64| succ.get(&a).cloned().unwrap_or_default();
        let is_last = idx == n - 1;
        let mnem = s(insn, "mnemonic").to_string();
        let raw_op_str = s(insn, "op_str").to_string();
        let op_str = decode_variables(s(insn, "op_str"));

        // Before anything is lowered: this CPU has no FS or GS to lower it to.
        if let Some(what) = fs_gs_operand(insn, &raw_op_str) {
            return uns(format!("{mnem} {raw_op_str}: {what}"));
        }

        if is_last && insn.get("op").and_then(Value::as_str) == Some("INDIRECT_NEAR_JMP") {
            let expr = self.indirect_jump_target(insn)?;
            out.push(format!(
                "{indent}jump_table_((((cs() as u32) << 4).wrapping_add(({expr}) as u32)) & 0xFFFFF, expected_retip);"
            ));
            out.push(format!("{indent}return -1;"));
            return Ok((out, true));
        }
        if is_last
            && mnem == "jmp"
            && ["ax", "bx", "cx", "dx", "si", "di", "bp", "sp"]
                .contains(&op_str.to_lowercase().trim())
        {
            let op = op_str.to_lowercase();
            let op = op.trim();
            out.push(format!(
                "{indent}jump_table_((((cs() as u32) << 4).wrapping_add({op}() as u32)) & 0xFFFFF, expected_retip);"
            ));
            out.push(format!("{indent}return -1;"));
            return Ok((out, true));
        }
        if is_last && mnem == "jmp" {
            match parse_hex16(&op_str) {
                Some(target) => {
                    out.push(format!("{indent}return 0x{:04X};", target & 0xFFFF));
                    return Ok((out, true));
                }
                None => return uns(format!("jmp {op_str}")),
            }
        }
        // Conditional jcc (ends the block).
        if is_last && mnem.starts_with('j') && mnem != "jmp" {
            let target = match parse_hex16(&op_str) {
                Some(t) => t,
                None => return uns(format!("{mnem} {op_str}")),
            };
            let cond_c = jcc_condition(
                &mnem,
                insn.get("cond_prev").and_then(|v| v.as_object()),
                i64f(insn, "address"),
            );
            let cond = match rustify_cond(&cond_c) {
                Some(c) => c,
                None => return uns(format!("jcc-cond:{cond_c}")),
            };
            let ss = succs(block.start);
            // The not-taken edge continues at the next instruction — computed
            // from the instruction itself, never by elimination from the
            // successor list: a jcc aimed at its own fallthrough (`jz $+0`,
            // the two-byte settle-delay idiom) has one successor serving both
            // edges, and "whichever successor isn't the target" reads that as
            // no fallthrough at all — emitted as a dispatcher exit with ip
            // still on the jcc, which can never advance.
            let fall = self.fallthrough_off(insn).filter(|f| ss.contains(f));
            match fall {
                Some(f) => {
                    out.push(format!("{indent}if {cond} {{"));
                    out.push(format!("{indent}    return 0x{:04X};", target & 0xFFFF));
                    out.push(format!("{indent}}} else {{"));
                    out.push(format!("{indent}    return 0x{:04X};", f & 0xFFFF));
                    out.push(format!("{indent}}}"));
                }
                None => {
                    out.push(format!("{indent}if {cond} {{"));
                    out.push(format!("{indent}    return 0x{:04X};", target & 0xFFFF));
                    out.push(format!("{indent}}}"));
                    // No decoded block at the fallthrough: leave the
                    // dispatcher, but with ip advanced to where control
                    // actually flows, so run_machine resolves (and JITs)
                    // the next instruction instead of re-running this one.
                    if let Some(f) = self.fallthrough_off(insn) {
                        out.push(format!("{indent}set_ip(0x{:04X});", f & 0xFFFF));
                    }
                    out.push(format!("{indent}return -1;"));
                }
            }
            return Ok((out, true));
        }
        if is_last && matches!(mnem.as_str(), "ret" | "retn" | "retf") {
            let body = self.format_instruction(insn)?;
            let mut last = String::new();
            for l in &body {
                out.push(format!("{indent}{l}"));
                last = l.trim().to_string();
            }
            if !last.starts_with("return") && !last.ends_with('}') {
                out.push(format!("{indent}return -1;"));
            }
            return Ok((out, true));
        }

        let body = self.format_instruction(insn)?;
        let mut last = String::new();
        for l in &body {
            out.push(format!("{indent}{l}"));
            if !l.trim().is_empty() {
                last = l.trim().to_string();
            }
        }

        // A noreturn call (call_table_/jump_table_/dos_exit/...) transfers
        // control and returns via the trampoline: emit `return;` and end the
        // block, mirroring the C state machine's `terminates` check. Without
        // this the block would wrongly fall through to the next pc.
        if terminates(&last) {
            if !last.starts_with("return") {
                out.push(format!("{indent}return -1;"));
            }
            return Ok((out, true));
        }

        // The block's last instruction was not a control transfer: fall through to
        // the single successor (or leave the dispatcher if there is none).
        if is_last {
            let ss = succs(block.start);
            if let Some(&first) = ss.first() {
                out.push(format!("{indent}return 0x{:04X};", first & 0xFFFF));
            } else {
                out.push(format!("{indent}return -1;"));
            }
            return Ok((out, true));
        }
        Ok((out, false))
    }

    fn render_function(&mut self, func: &Value) -> R<()> {
        let start = func.get("start").and_then(Value::as_i64).unwrap_or(0);
        self.t.current_func_name = self.func_name(start);
        let raw: Vec<Insn> = func
            .get("instructions")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_object().cloned()).collect())
            .unwrap_or_default();
        let instrs = normalize_flags(&raw);
        let instrs = normalize_indirect_jumps(&instrs);
        let mut start_set = BTreeSet::new();
        start_set.insert(start);
        let blocks = build_basic_blocks(&instrs, &start_set, Some(start));
        let succ = cfg_successors(&blocks);

        let impl_ = format!("{}_impl", self.func_name(start));
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
            match blocks.get(&addr) {
                None => {
                    // Entry alias with no decoded block of its own: forward to
                    // the function's first real block (or leave the dispatcher).
                    if !block_addrs.is_empty() {
                        self.dispatch_cases.push(format!(
                            "{indent}0x{addr:04X} => 0x{first_real:04X}, // {impl_}@{addr:04X}"
                        ));
                    } else {
                        self.dispatch_cases
                            .push(format!("{indent}0x{addr:04X} => -1, // {impl_}@{addr:04X}"));
                    }
                }
                Some(block) => {
                    let body = self.render_block(block, &succ)?;
                    let blk = self.block_name(addr);
                    self.dispatch_cases.push(format!(
                        "{indent}0x{addr:04X} => {blk}(r, expected_retip), // {impl_}@{addr:04X}"
                    ));
                    self.block_fns.push(format!("// {impl_}@{addr:04X}"));
                    self.block_fns.push(format!(
                        "fn {blk}(r: &mut Regs, expected_retip: u16) -> c_int {{"
                    ));
                    self.block_fns.extend(body.iter().map(|l| localize_regs(l)));
                    self.block_fns.push("    return -1;".into());
                    self.block_fns.push("}".into());
                    self.block_fns.push(String::new());
                }
            }
        }
        Ok(())
    }

    fn render_dispatch(&self) -> Vec<String> {
        let mut lines = vec![
            "#[no_mangle]".into(),
            format!(
                "pub extern \"C\" fn {}(mut pc: c_int, expected_retip: u16, _file: *const c_char, _func: *const c_char, _line: c_int) {{",
                self.dispatch_name()
            ),
            // The block-local register cache lives for the whole dispatch:
            // blocks hand registers to each other in host registers, spilling
            // to the shared `cpu` only around runtime calls and at exit.
            "    let r = &mut Regs::load();".into(),
            "    loop {".into(),
            "        // Each arm runs one basic block and yields the next pc (-1: done).".into(),
            "        pc = match pc {".into(),
        ];
        lines.extend(self.dispatch_cases.iter().cloned());
        let default_popped = if self.cs_base != 0 {
            format!(
                "                let popped_ip = ((pc as u32).wrapping_add(0x{:04X}) & 0xFFFF) as u16;",
                self.cs_base
            )
        } else {
            format!(
                "                let popped_ip = ((pc as u32).wrapping_add(0x{:05X}).wrapping_sub((r.cs() as u32) << 4)) as u16;",
                self.load_base
            )
        };
        lines.extend([
            "            _ => {".into(),
            "                // Not a case in this chunk's switch — a cross-binary RET/jmp.".into(),
            default_popped,
            "                r.near_ret_tail_(popped_ip, expected_retip);".into(),
            "                return;".into(),
            "            }".into(),
            "        };".into(),
            "        if pc < 0 {".into(),
            "            r.spill();".into(),
            "            return;".into(),
            "        }".into(),
            "    }".into(),
            "}".into(),
        ]);
        lines
    }
}

/// Translate a `jcc_condition` C expression (flag tokens + `==`/`!=`/`&&`/`||`)
/// into Rust (flag accessors). Rejects loop/jcxz forms (`--cx`, `ecx`) and the
/// unsupported-jcc placeholder -> caller falls back to C.
fn rustify_cond(cond: &str) -> Option<String> {
    if cond.contains("--cx") || cond.contains("ecx") || cond.contains("/*") || cond == "1" {
        return None;
    }
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b(ZF|CF|SF|OF|PF|DF|IF)\b").unwrap());
    let s = re.replace_all(cond, "$1()").to_string();
    // jcxz: `cx == 0` -> `cx() == 0`
    static CXRE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let cxre = CXRE.get_or_init(|| Regex::new(r"\bcx\b").unwrap());
    Some(cxre.replace_all(&s, "cx()").to_string())
}

/// emit_chunk — returns the chunk `.rs` text, or `Unsupported` if the chunk uses
/// a construct the emitter can't express yet (a hard error for the JIT caller).
pub fn emit_chunk(ir: &Value, prefix: &str, image_base: Option<i64>, rt_path: &str) -> R<String> {
    emit_chunk_known(ir, prefix, image_base, rt_path, &BTreeSet::new())
}

/// emit_chunk, plus the constructs it could not translate. Each of those became a
/// run-time trap instead of failing the compile (a packed game's decode runs into
/// its own ciphertext, and refusing the chunk would throw away the real code that
/// decrypts it) — but they are still the gap frontier, so callers that want to
/// *see* the gaps ask for them here rather than by catching an error.
pub fn emit_chunk_gaps(
    ir: &Value,
    prefix: &str,
    image_base: Option<i64>,
    rt_path: &str,
) -> R<(String, Vec<String>)> {
    emit_chunk_inner(ir, prefix, image_base, rt_path, &BTreeSet::new())
}

/// emit_chunk with extra known-function addresses beyond the IR's own function
/// starts. Test seam: unit tests render a single function while declaring
/// sibling call targets "known" so direct calls render as intra-chunk transfers.
pub fn emit_chunk_known(
    ir: &Value,
    prefix: &str,
    image_base: Option<i64>,
    rt_path: &str,
    extra_known: &BTreeSet<i64>,
) -> R<String> {
    emit_chunk_inner(ir, prefix, image_base, rt_path, extra_known).map(|(text, _gaps)| text)
}

fn emit_chunk_inner(
    ir: &Value,
    prefix: &str,
    image_base: Option<i64>,
    rt_path: &str,
    extra_known: &BTreeSet<i64>,
) -> R<(String, Vec<String>)> {
    let empty = Vec::new();
    let functions = ir
        .get("functions")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let load_segment: i64 = 0x1010;
    let load_base = image_base.unwrap_or(load_segment << 4) & 0xFFFFF;

    let mut c = Renderer::for_test(prefix);
    c.load_segment = load_segment;
    c.reloc_offsets = ir
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

    let mut known_funcs: BTreeSet<i64> = functions
        .iter()
        .filter_map(|f| f.get("start").and_then(Value::as_i64))
        .collect();
    known_funcs.extend(extra_known.iter().copied());

    let mut r = RRenderer {
        t: c,
        cs_base: 0,
        load_base,
        dispatch_cases: Vec::new(),
        block_fns: Vec::new(),
        seen_cases: BTreeSet::new(),
        known_funcs,
        unsupported: Vec::new(),
    };

    let binary_name = prefix.trim_end_matches('_').to_string();
    let mut wrappers: Vec<String> = Vec::new();
    for f in functions {
        let start = f.get("start").and_then(Value::as_i64).unwrap_or(0);
        r.render_function(f)?;
        let impl_ = format!("{}_impl", r.func_name(start));
        wrappers.push(format!(
            "#[no_mangle]\npub extern \"C\" fn {impl_}(expected_retip: u16, file: *const c_char, func: *const c_char, line: c_int) {{"
        ));
        wrappers.push(format!("    enter_binary(c\"{binary_name}\".as_ptr());"));
        wrappers.push(
            "    let mut saved_tail = ShimTailDispatchState { pending: 0, addr: 0, expected: 0 };"
                .into(),
        );
        wrappers.push("    tail_dispatch_save(&mut saved_tail);".into());
        wrappers.push(format!(
            "    {}(0x{start:04X}, expected_retip, file, func, line);",
            r.dispatch_name()
        ));
        wrappers.push("    drain_pending_tail_dispatch();".into());
        wrappers.push("    tail_dispatch_restore(&saved_tail);".into());
        wrappers.push("    leave_binary();".into());
        wrappers.push("}".into());
        wrappers.push(String::new());
    }

    let dispatch = r.render_dispatch();

    // rt_path is retained in the signature for callers, but chunks no longer
    // `include!` the prelude — it is precompiled once as the `saisei_rt` rlib
    // (built beside the chunks from this same file) and linked via `--extern`.
    let _ = rt_path;
    let mut out: Vec<String> = vec![
        "// Generated by saisei-jitc (Rust chunk backend). Do not edit by hand;".to_string(),
        "// to hand-instrument, edit and recompile the .so with".to_string(),
        "//   rustc --edition 2021 --crate-type cdylib -Copt-level=0 --extern saisei_rt=libsaisei_rt_<hash>.rlib ...".to_string(),
        "#![no_std]".to_string(),
        "#![allow(dead_code, non_snake_case, non_upper_case_globals, non_camel_case_types)]"
            .to_string(),
        "#![allow(unused_parens, unused_mut, unused_assignments, unused_unsafe, unused_variables)]"
            .to_string(),
        "#![allow(unreachable_code, unreachable_patterns)]".to_string(),
        // The precompiled runtime prelude (rlib), re-exported unqualified.
        "extern crate saisei_rt;".to_string(),
        "use saisei_rt::*;".to_string(),
        "use core::ffi::{c_char, c_int, c_void};".to_string(),
        // The chunk's own name, mirroring the C backend's `__FILE__`
        // ("jit_<segbase5>_<off4>_<sha>.c"). The runtime's cross-binary-write
        // tripwire (warn_on_mutation) parses this to recognize a chunk writing
        // within its own decode segment as legitimate self-modification. The
        // prelude lives upstream in the rlib, so it reads this name back through
        // the linker symbol `saisei_site_name` (see rt::site()).
        format!(
            "#[no_mangle] pub extern \"C\" fn saisei_site_name() -> *const c_char {{ c\"{binary_name}\".as_ptr() }}"
        ),
        String::new(),
    ];
    out.extend(dispatch);
    out.push(String::new());
    out.extend(r.block_fns.iter().cloned());
    out.extend(wrappers);
    Ok((out.join("\n"), r.unsupported.clone()))
}
