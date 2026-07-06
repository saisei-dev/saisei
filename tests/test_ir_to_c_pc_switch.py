"""Tests for PCSwitchRenderer with global dispatcher."""
# flake8: noqa

"""Tests for PCSwitchRenderer with global dispatcher."""

import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import PCSwitchRenderer  # noqa: E402


def _dispatch_from(funcs, known=None):
    renderer = PCSwitchRenderer()
    known = known or set()
    for f in funcs:
        renderer.render_function(f, known)
    return "\n".join(renderer.render_dispatch())


def test_pc_switch_renderer_emits_cases():
    func = {
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "cmp", "op_str": "ax, 1", "bytes": "3D0100"},
            {"address": 0x0103, "mnemonic": "jne", "op_str": "0109", "bytes": "7504"},
            {"address": 0x0105, "mnemonic": "mov", "op_str": "bx, bx", "bytes": "89DB"},
            {"address": 0x0107, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
            {"address": 0x0109, "mnemonic": "mov", "op_str": "cx, cx", "bytes": "89C9"},
            {"address": 0x010B, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    src = _dispatch_from([func])
    switch_section = src.split("switch (pc)")[1]
    assert "case 0x0100:" in switch_section
    assert "pc = 0x0109;" in switch_section
    assert "pc = 0x0105;" in switch_section
    block_0100 = switch_section.split("case 0x0100:")[1].split("case", 1)[0]
    assert block_0100.count("continue;") == 1


def test_pc_switch_ret_far_emits_helper():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "retf", "op_str": "", "bytes": "CB"},
        ],
    }
    src = _dispatch_from([func])
    block = src.split("case 0x0000:")[1].split("default:", 1)[0]
    assert "retf();" in block


def test_pc_switch_call_pushes_retip_and_continues():
    func = {
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "call", "op_str": "0200", "bytes": "E8FD00", "target": 0x0200}
        ],
    }
    src = _dispatch_from([func], known={0x0100, 0x0200})
    block = src.split("case 0x0100:")[1].split("default:", 1)[0]
    # Faithful flat model: push the return IP (next-insn = 0x0103) on the
    # emulated stack and `pc = target; continue;`. No C recursion.
    assert "memw_write(ss, sp, (uint16_t)(0x00103U + 0x10100U - ((uint32_t)cs << 4)));" in block
    assert "pc = 0x0200;" in block
    assert "continue;" in block
    assert "func_0200" not in block


def test_pc_switch_call_to_known_address_sets_pc():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "call", "op_str": "0CAD", "bytes": "E8AA0C", "target": 0x0CAD},
            {"address": 0x0003, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    renderer = PCSwitchRenderer()
    renderer.render_function(func, {0x0000, 0x0CAD})
    src = "\n".join(renderer.render_dispatch())
    block = src.split("case 0x0000:")[1].split("default:", 1)[0]
    # A direct call to a known address pushes the return IP and sets pc to the
    # target -- intra-chunk this routes to the case, extern it exits via default.
    assert "memw_write(ss, sp, (uint16_t)(0x00003U + 0x10100U - ((uint32_t)cs << 4)));" in block
    assert "pc = 0x0CAD;" in block
    assert "func_0CAD" not in block
    assert "// TODO ASM: call 0CAD" not in block


def test_pc_switch_indirect_near_jmp():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "jmp",
                "op_str": "word ptr cs:[bx + 0x588]",
                "bytes": "00",
                "detail": {"mem_refs": [{"segment": "CS", "disp": 0x588, "access": "read"}]},
            }
        ],
    }
    src = _dispatch_from([func])
    assert (
        "jump_table((((uint32_t)cs << 4) + (uint16_t)(memw(cs, (bx + 0x588) & 0xFFFF))) & 0xFFFFF, expected_retip);" in src
    )
    block = src.split("case 0x0000:")[1].split("default:", 1)[0]
    assert "known_case" not in block
    assert block.count("return;") == 1


def test_pc_switch_default_aborts():
    func = {
        "start": 0x0000,
        "instructions": [{"address": 0x0000, "mnemonic": "ret", "op_str": "", "bytes": "C3"}],
    }
    src = _dispatch_from([func])
    default_block = src.split("default:")[1]
    assert "near_ret_tail(popped_ip, expected_retip);" in default_block
    assert "unhandled_pc abort path" in default_block


def test_pc_switch_entry_block_emitted_when_start_is_late():
    func = {
        "start": 0x0100,
        "instructions": [
            {"address": 0x0102, "mnemonic": "mov", "op_str": "ax, ax", "bytes": "89C0"},
            {"address": 0x0104, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    renderer = PCSwitchRenderer()
    renderer.render_function(func, set())
    src = "\n".join(renderer.render_dispatch())
    assert "case 0x0100:" in src
    entry_block = src.split("case 0x0100:")[1].split("case", 1)[0]
    assert "pc = 0x0102;" in entry_block
    assert entry_block.count("continue;") == 1
    assert "case 0x0102:" in src


def test_pc_switch_dos_exit_returns():
    func = {
        "start": 0x0100,
        "instructions": [
            {"address": 0x0100, "mnemonic": "mov", "op_str": "ah, 4Ch", "bytes": "B44C"},
            {"address": 0x0102, "mnemonic": "int", "op_str": "21h", "bytes": "CD21"},
            {"address": 0x0104, "mnemonic": "mov", "op_str": "bx, bx", "bytes": "89DB"},
        ],
    }
    src = _dispatch_from([func])
    block = src.split("case 0x0100:")[1].split("default:", 1)[0]
    assert "dos_exit();" in block
    assert block.count("return;") == 1
    assert "bx = bx;" not in block
