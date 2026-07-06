from pathlib import Path

from compiler.disassemble import stage1, stage2


def _decode_disp(data: bytes, tmp_path: Path) -> int:
    meta = stage1(data, tmp_path, [0])
    _, functions, _, _, _ = stage2(
        meta["load_module"],
        meta["header"],
        tmp_path,
        [0],
    )
    mem_refs = functions[0]["instructions"][0]["detail"].get("mem_refs", [])
    assert mem_refs, "Expected memory reference"
    return mem_refs[0]["disp"]


def test_negative_disp(tmp_path: Path) -> None:
    data = bytes([0x8B, 0x46, 0xFE, 0xC3])  # mov ax, [bp-2]; ret
    assert _decode_disp(data, tmp_path) == -2
