import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render_function(func, known, name_prefix=""):
    return CCodeRenderer(name_prefix=name_prefix).render_function(func, known)
def test_dos_get_version_rendered_before_if():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ah, 0x30",
                "bytes": "B430",
            },
            {
                "address": 0x0002,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0004,
                "mnemonic": "cmp",
                "op_str": "al, 2",
                "bytes": "3C02",
            },
            {
                "address": 0x0006,
                "mnemonic": "jb",
                "op_str": "0010",
                "bytes": "7208",
            },
            {
                "address": 0x0008,
                "mnemonic": "mov",
                "op_str": "ax, bx",
                "bytes": "89D8",
            },
            {
                "address": 0x000A,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_get_version();" in src
    assert "if (CF == 1)" in src
    assert src.index("CF = dos_get_version();") < src.index("if (CF == 1)")


def test_if_else_statement_is_structured():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jz",
                "op_str": "0x8",
                "bytes": "7406",
            },
            {
                "address": 0x0002,
                "mnemonic": "call",
                "op_str": "0x1000",
                "bytes": "E80000",
            },
            {
                "address": 0x0005,
                "mnemonic": "jmp",
                "op_str": "0xB",
                "bytes": "EB04",
            },
            {
                "address": 0x0008,
                "mnemonic": "call",
                "op_str": "0x2000",
                "bytes": "E80000",
            },
            {
                "address": 0x000B,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000, 0x1000, 0x2000}
    lines = render_function(func, known, name_prefix="g_")
    src = "\n".join(lines)
    assert "if (ZF != 1)" in src
    assert "else" in src
    assert "pc = 0x1000;" in src
    assert "pc = 0x2000;" in src


def test_if_else_with_hex_suffix_is_structured():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jz",
                "op_str": "0008h",
                "bytes": "7406",
            },
            {
                "address": 0x0002,
                "mnemonic": "call",
                "op_str": "1000h",
                "bytes": "E80000",
            },
            {
                "address": 0x0005,
                "mnemonic": "jmp",
                "op_str": "000Bh",
                "bytes": "EB04",
            },
            {
                "address": 0x0008,
                "mnemonic": "call",
                "op_str": "2000h",
                "bytes": "E80000",
            },
            {
                "address": 0x000B,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000, 0x1000, 0x2000}
    lines = render_function(func, known, name_prefix="g_")
    src = "\n".join(lines)
    assert "if (ZF != 1)" in src
    assert "else" in src
    assert "pc = 0x1000;" in src
    assert "pc = 0x2000;" in src


def test_exec_stack_fields_rendered_with_names():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "word ptr cs:[0x8c2], sp",
                "bytes": "2e8926c208",
            },
            {
                "address": 0x0005,
                "mnemonic": "mov",
                "op_str": "sp, word ptr cs:[0x8c2]",
                "bytes": "2e8b26c208",
            },
            {
                "address": 0x000A,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "exec_params.saved_sp = sp;" in src
    assert "sp = exec_params.saved_sp;" in src


def test_resident_control_block_fields_named():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "word ptr es:[0xff00], 0x2d9",
                "bytes": "26c70600ffd902",
            },
            {
                "address": 0x0007,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "rcb_write16(FIELD_1, 0x2d9);" in src


def test_sequential_jumps_form_else_if_chain():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jz",
                "op_str": "0x5",
                "bytes": "7403",
            },
            {
                "address": 0x0002,
                "mnemonic": "call",
                "op_str": "0x1000",
                "bytes": "E80000",
            },
            {
                "address": 0x0005,
                "mnemonic": "jz",
                "op_str": "0xA",
                "bytes": "7403",
            },
            {
                "address": 0x0007,
                "mnemonic": "call",
                "op_str": "0x2000",
                "bytes": "E80000",
            },
            {
                "address": 0x000A,
                "mnemonic": "jz",
                "op_str": "0xF",
                "bytes": "7403",
            },
            {
                "address": 0x000C,
                "mnemonic": "call",
                "op_str": "0x3000",
                "bytes": "E80000",
            },
            {
                "address": 0x000F,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000, 0x1000, 0x2000, 0x3000}
    lines = render_function(func, known, name_prefix="g_")
    src = "\n".join(lines)
    assert src.splitlines()[4].startswith("    if (ZF != 1)")
    assert src.count("if (ZF != 1)") == 2
    assert "pc = 0x1000;" in src
    assert "pc = 0x2000;" in src
    assert "pc = 0x3000;" in src


def test_empty_then_branch_does_not_emit_if():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jz",
                "op_str": "0x6",
                "bytes": "7404",
            },
            {
                "address": 0x0002,
                "mnemonic": "jmp",
                "op_str": "0xA",
                "bytes": "EB06",
            },
            {
                "address": 0x0006,
                "mnemonic": "call",
                "op_str": "0x1000",
                "bytes": "E80000",
            },
            {
                "address": 0x0009,
                "mnemonic": "jmp",
                "op_str": "0xA",
                "bytes": "EBFF",
            },
            {
                "address": 0x000A,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000, 0x1000}
    lines = render_function(func, known, name_prefix="g_")
    src = "\n".join(lines)
    assert "if (ZF != 1)" in src
    assert "pc = 0x1000;" in src


def test_test_and_jz_fold_into_if():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "test",
                "op_str": "ax, ax",
                "bytes": "85C0",
            },
            {
                "address": 0x0002,
                "mnemonic": "jz",
                "op_str": "0007",
                "bytes": "7403",
            },
            {
                "address": 0x0004,
                "mnemonic": "mov",
                "op_str": "bx, 1",
                "bytes": "BB0100",
            },
            {
                "address": 0x0007,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "if (ZF == 1)" in src


def test_or_and_jnz_fold_into_if():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "or",
                "op_str": "dx, dx",
                "bytes": "09D2",
            },
            {
                "address": 0x0002,
                "mnemonic": "jnz",
                "op_str": "0007",
                "bytes": "7503",
            },
            {
                "address": 0x0004,
                "mnemonic": "mov",
                "op_str": "ax, 0",
                "bytes": "B80000",
            },
            {
                "address": 0x0007,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "if (ZF == 0)" in src


def test_cmp_followed_by_multiple_jumps_preserves_flags():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "cmp",
                "op_str": "al, 0x20",
                "bytes": "3C20",
            },
            {
                "address": 0x0002,
                "mnemonic": "je",
                "op_str": "000C",
                "bytes": "7408",
            },
            {
                "address": 0x0004,
                "mnemonic": "jae",
                "op_str": "0014",
                "bytes": "730E",
            },
            {
                "address": 0x0006,
                "mnemonic": "mov",
                "op_str": "bl, 0",
                "bytes": "B300",
            },
            {
                "address": 0x0008,
                "mnemonic": "jmp",
                "op_str": "001A",
                "bytes": "EB10",
            },
            {
                "address": 0x000C,
                "mnemonic": "mov",
                "op_str": "bl, 1",
                "bytes": "B301",
            },
            {
                "address": 0x000E,
                "mnemonic": "jmp",
                "op_str": "001A",
                "bytes": "EB0A",
            },
            {
                "address": 0x0014,
                "mnemonic": "mov",
                "op_str": "bl, 2",
                "bytes": "B302",
            },
            {
                "address": 0x0016,
                "mnemonic": "jmp",
                "op_str": "001A",
                "bytes": "EB02",
            },
            {
                "address": 0x001A,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "ZF" in src
    assert "CF" in src
    assert "// TODO ASM: jae" not in src


def test_jcc_in_separate_block_uses_prior_cmp():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "cmp", "op_str": "al, 0x20", "bytes": "3C20"},
            {"address": 0x0002, "mnemonic": "jne", "op_str": "0008", "bytes": "7504"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0008, "mnemonic": "jae", "op_str": "0012", "bytes": "7308"},
            {"address": 0x000A, "mnemonic": "mov", "op_str": "bl, 2", "bytes": "B302"},
            {"address": 0x000C, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0012, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF" in src
    assert "// TODO ASM: jae" not in src


def test_bp_relative_operands_are_named():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, word ptr [bp - 4]",
                "bytes": "8B46FC",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "word ptr [bp + 6], ax",
                "bytes": "894606",
            },
            {
                "address": 0x0006,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "ax = memw(ss, ((bp + 0xFFFC) & 0xFFFF));" in src
    assert "memw_write(ss, (bp + 6) & 0xFFFF, ax);" in src


def test_nested_bp_relative_operands_are_named():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, [si + [bp - 4]]",
                "bytes": "8B00",
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "[si + var_4]" in src

def test_if_then_spans_multiple_blocks():
    func = {
        "start": 0x0016,
        "instructions": [
            {
                "address": 0x0016,
                "mnemonic": "mov",
                "op_str": "ax, 0x3d00",
                "bytes": "B8003D",
            },
            {
                "address": 0x0019,
                "mnemonic": "int",
                "op_str": "0x21",
                "bytes": "CD21",
            },
            {
                "address": 0x001B,
                "mnemonic": "jae",
                "op_str": "0x25",
                "bytes": "7308",
            },
            {
                "address": 0x001D,
                "mnemonic": "call",
                "op_str": "0x520",
                "bytes": "E80005",
            },
            {
                "address": 0x0020,
                "mnemonic": "mov",
                "op_str": "ax, 0x4c00",
                "bytes": "B8004C",
            },
            {
                "address": 0x0023,
                "mnemonic": "int",
                "op_str": "0x21",
                "bytes": "CD21",
            },
            {
                "address": 0x0025,
                "mnemonic": "mov",
                "op_str": "bx, ax",
                "bytes": "8BD8",
            },
        ],
    }
    known = {0x0016, 0x0520}
    lines = render_function(func, known, name_prefix="g_")
    src = "\n".join(lines)
    assert "CF = dos_open_file((const char *)seg_off(ds, dx));" in src
    assert "if (CF != 0)" in src
    assert "pc = 0x0520;" in src
    assert "dos_exit();" in src
    assert "bx = ax;" in src


def test_if_else_spans_multiple_blocks():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jz",
                "op_str": "0x8",
                "bytes": "7406",
            },
            {
                "address": 0x0002,
                "mnemonic": "call",
                "op_str": "0x1000",
                "bytes": "E80000",
            },
            {
                "address": 0x0005,
                "mnemonic": "jmp",
                "op_str": "0xE",
                "bytes": "EB07",
            },
            {
                "address": 0x0008,
                "mnemonic": "call",
                "op_str": "0x2000",
                "bytes": "E80000",
            },
            {
                "address": 0x000B,
                "mnemonic": "call",
                "op_str": "0x3000",
                "bytes": "E80000",
            },
            {
                "address": 0x000E,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000, 0x1000, 0x2000, 0x3000}
    lines = render_function(func, known, name_prefix="g_")
    src = "\n".join(lines)
    assert "if (ZF != 1)" in src
    assert "else" in src
    assert "pc = 0x1000;" in src
    assert "pc = 0x2000;" in src
    assert "pc = 0x3000;" in src


def test_if_else_when_then_returns():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jz",
                "op_str": "0x5",
                "bytes": "7403",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
            {
                "address": 0x0005,
                "mnemonic": "call",
                "op_str": "0x1000",
                "bytes": "E80000",
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000, 0x1000}
    lines = render_function(func, known, name_prefix="g_")
    src = "\n".join(lines)
    assert "if (ZF != 1)" in src
    assert "pc = 0x1000;" in src
    assert src.count("return;") == 2


def test_detect_if_else_avoids_self_referential_region():
    from compiler.basic_block import BasicBlock
    from compiler.ir_to_c import CCodeRenderer
    from compiler.cfg import build_cfg, compute_immediate_postdominators
    from compiler.patterns import PatternContext
    from compiler.patterns.conditionals import detect_if_else

    b0 = BasicBlock(0)
    b0.instructions = [
        {"address": 0, "mnemonic": "jz", "op_str": "0004", "bytes": "7404"}
    ]
    b2 = BasicBlock(2)
    b2.instructions = [
        {"address": 2, "mnemonic": "jmp", "op_str": "0000", "bytes": "EBFE"}
    ]
    b4 = BasicBlock(4)
    b4.instructions = [
        {"address": 4, "mnemonic": "ret", "op_str": "", "bytes": "C3"}
    ]

    blocks = {0: b0, 2: b2, 4: b4}
    graph = build_cfg(blocks)
    ipost = compute_immediate_postdominators(graph)
    ctx = PatternContext(blocks, graph, set(), CCodeRenderer(), {}, ipost, set())

    assert detect_if_else(0, ctx) is None


def test_guard_if_flattens_else():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "jz", "op_str": "0x5", "bytes": "7403"},
            {"address": 0x0002, "mnemonic": "jmp", "op_str": "0x100", "bytes": "E90001"},
            {"address": 0x0005, "mnemonic": "call", "op_str": "0x2000", "bytes": "E80000"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, {0x2000}, name_prefix="g_")
    src = "\n".join(lines)
    assert "else" not in src
    # Faithful flat model: the unconditional `jmp 0x100` lowers to pc=0x0100;
    # continue (a same-segment jmp the top-level loop re-resolves), not a C
    # tail-call into g_func_0100().
    assert "g_func_0100();" not in src
    assert "pc = 0x0100;" in src
    assert "pc = 0x2000;" in src



