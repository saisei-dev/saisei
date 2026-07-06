from pathlib import Path

from compiler.disassemble import stage1, stage2


def test_mid_instruction_extra_entry_is_ignored(tmp_path: Path) -> None:
    # mov bl, byte ptr es:[di + 0xf]
    # shr bl, 1
    # ret
    data = bytes([0x26, 0x8A, 0x5D, 0x0F, 0xD0, 0xEB, 0xC3])
    meta = stage1(data, tmp_path, [0x0, 0x3])

    instructions, _, _, _, _ = stage2(
        meta["load_module"],
        meta["header"],
        tmp_path,
        [0x0, 0x3],
        True,
    )

    assert [insn["address"] for insn in instructions] == [0, 4, 6]
    assert all(insn["mnemonic"] != "db" for insn in instructions)
