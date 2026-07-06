from pathlib import Path

from compiler.disassemble import stage1, stage2, compute_xrefs
from compiler.ir_to_c import build_basic_blocks
from compiler import cfg


def disassemble_bytes(data: bytes, outdir: Path):
    meta = stage1(data, outdir, [0x0])
    instructions, _, _, _, _ = stage2(
        meta["load_module"], meta["header"], outdir, [0x0]
    )
    xrefs = compute_xrefs(instructions)
    return instructions, xrefs


def test_ljmp_terminates(tmp_path: Path):
    data = bytes.fromhex("EA001000209090")
    instructions, xrefs = disassemble_bytes(data, tmp_path)
    assert len(instructions) == 1
    assert instructions[0]["mnemonic"] == "ljmp"
    assert instructions[0]["target"] == 0x21000
    assert all("fallthrough@" not in x for x in xrefs)


def test_cfg_no_fallthrough():
    instrs = [
        {
            "address": 0x0,
            "mnemonic": "ljmp",
            "op_str": "0x2000:0x1000",
            "bytes": "EA00100020",
        },
        {"address": 0x5, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]
    blocks = build_basic_blocks(instrs)
    assert list(blocks) == [0x0, 0x5]
    graph = cfg.build_cfg(blocks)
    assert graph.out_degree(0x0) == 0
