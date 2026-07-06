from pathlib import Path

from compiler.disassemble import stage1, stage2, compute_xrefs


def disassemble_bytes(data: bytes, outdir: Path):
    meta = stage1(data, outdir, [0x0])
    instructions, _, _, _, _ = stage2(
        meta["load_module"], meta["header"], outdir, [0x0]
    )
    xrefs = compute_xrefs(instructions)
    return instructions, xrefs


def test_jmp_xref(tmp_path: Path):
    data = bytes.fromhex("EB0390909090")
    _, xrefs = disassemble_bytes(data, tmp_path)
    assert "jump@0x5" in xrefs
    assert "fallthrough@0x2" not in xrefs


def test_jcc_xref(tmp_path: Path):
    data = bytes.fromhex("7503909090")
    _, xrefs = disassemble_bytes(data, tmp_path)
    assert "jump@0x5" in xrefs
    assert "fallthrough@0x2" in xrefs
