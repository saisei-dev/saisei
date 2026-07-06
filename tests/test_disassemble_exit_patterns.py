from pathlib import Path

from compiler.disassemble import stage1, stage2, compute_xrefs


def disassemble_bytes(data: bytes, outdir: Path):
    meta = stage1(data, outdir, [0x0])
    instructions, _, _, _, _ = stage2(
        meta["load_module"], meta["header"], outdir, [0x0]
    )
    xrefs = compute_xrefs(instructions)
    return instructions, xrefs


def test_ret_terminates(tmp_path: Path):
    data = bytes([0xC3, 0x00, 0x00, 0x00])
    instructions, xrefs = disassemble_bytes(data, tmp_path)
    assert len(instructions) == 1
    assert instructions[0]["mnemonic"] == "ret"
    assert all("fallthrough@" not in x for x in xrefs)


def test_int_20_terminates(tmp_path: Path):
    data = bytes([0xCD, 0x20, 0x90, 0x90])
    instructions, xrefs = disassemble_bytes(data, tmp_path)
    assert len(instructions) == 1
    assert instructions[0]["mnemonic"] == "int"
    assert instructions[0]["op_str"] == "0x20"
    assert all("fallthrough@" not in x for x in xrefs)


def test_mov_ax_4c00_int_21_terminates(tmp_path: Path):
    data = bytes([0xB8, 0x00, 0x4C, 0xCD, 0x21, 0x90, 0x90])
    instructions, xrefs = disassemble_bytes(data, tmp_path)
    assert len(instructions) == 2
    assert instructions[1]["mnemonic"] == "int"
    assert instructions[1]["op_str"] == "0x21"
    assert "fallthrough@0x3" in xrefs
    assert "fallthrough@0x5" not in xrefs
