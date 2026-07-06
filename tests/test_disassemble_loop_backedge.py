from pathlib import Path
import sys

# Ensure repository root is on sys.path
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.disassemble import stage1, stage2  # noqa: E402


def test_loop_backward_edge(tmp_path: Path):
    # Program layout:
    #   0000: nop
    #   0001: nop
    #   0002: inc dx
    #   0003: nop
    #   0004: loop 0002
    #   0006: int 0x20
    data = bytes([0x90, 0x90, 0x42, 0x90, 0xE2, 0xFC, 0xCD, 0x20])
    meta = stage1(data, tmp_path, extra_entries=[0x4])
    instructions, *_ = stage2(
        meta["load_module"],
        meta["header"],
        tmp_path,
        extra_entries=[0x4],
    )
    addrs = {insn["address"] for insn in instructions}
    assert 0x0004 in addrs  # loop instruction
    assert 0x0002 in addrs  # backward target
    assert 0x0006 in addrs  # fallthrough
