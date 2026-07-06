from pathlib import Path

from compiler.disassemble import stage1, stage2


def test_stage2_returns_sorted_instructions(tmp_path: Path) -> None:
    data = bytes.fromhex("B80100B90200C3")
    meta = stage1(data, tmp_path, None)
    instructions, _, _, _, _ = stage2(
        meta["load_module"], meta["header"], tmp_path
    )
    addrs = [ins["address"] for ins in instructions]
    assert addrs == sorted(addrs)
