from pathlib import Path

from compiler.disassemble import stage1, stage2


def test_only_requested_entry_points_are_decoded(tmp_path: Path) -> None:
    data = bytes([0xC3, 0x00, 0x00, 0x00])
    meta = stage1(data, tmp_path, [0x1])
    instructions, _, _, _, _ = stage2(
        meta["load_module"],
        meta["header"],
        tmp_path,
        [0x1],
        True,
    )
    # The decoder should start at the requested entry point (0x1) and record
    # the subsequent undecodable byte as a raw ``db`` instruction, allowing
    # later stages to emit a ``// TODO ASM`` comment.
    assert [insn["address"] for insn in instructions] == [1, 3]
    assert instructions[0]["mnemonic"] == "add"
    assert instructions[0]["op_str"] == "byte ptr [bx + si], al"
    assert instructions[1]["mnemonic"] == "db"
    assert instructions[1]["op_str"] == "0x00"
