from pathlib import Path

from compiler.disassemble import stage1, stage2


def test_lcall_records_target(tmp_path: Path) -> None:
    data = bytes.fromhex("9A00100020C3")
    meta = stage1(data, tmp_path, [0x0])
    instructions, _, _, _, _ = stage2(
        meta["load_module"], meta["header"], tmp_path, [0x0]
    )
    assert instructions[0]["mnemonic"] == "lcall"
    assert instructions[0]["target"] == 0x21000
