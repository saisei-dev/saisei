from pathlib import Path

from compiler.disassemble import stage1, stage2


def _decode_mem_ref(data: bytes, tmp_path: Path) -> dict:
    meta = stage1(data, tmp_path, [0])
    _, functions, _, _, _ = stage2(
        meta["load_module"],
        meta["header"],
        tmp_path,
        [0],
    )
    mem_refs = functions[0]["instructions"][0]["detail"].get("mem_refs", [])
    assert mem_refs, "Expected memory reference"
    return mem_refs[0]


def _decode_segment(data: bytes, tmp_path: Path) -> str:
    return _decode_mem_ref(data, tmp_path)["segment"]


def test_si_bp_defaults_to_ss(tmp_path: Path) -> None:
    data = bytes([0x8B, 0x02, 0xC3])  # mov ax, [bp+si]; ret
    assert _decode_segment(data, tmp_path) == "SS"


def test_bp_di_defaults_to_ss(tmp_path: Path) -> None:
    data = bytes([0x8B, 0x03, 0xC3])  # mov ax, [bp+di]; ret
    assert _decode_segment(data, tmp_path) == "SS"


def test_mov_mem_reg_marks_readwrite_access(tmp_path: Path) -> None:
    data = bytes([0x01, 0x07, 0xC3])  # add word ptr [bx], ax; ret
    mem_ref = _decode_mem_ref(data, tmp_path)
    assert mem_ref["segment"] == "DS"
    assert mem_ref["access"] == "readwrite"


def test_segment_override_recorded(tmp_path: Path) -> None:
    data = bytes([0x2E, 0xD7, 0xC3])  # cs: xlatb; ret
    meta = stage1(data, tmp_path, [0])
    _, functions, _, _, _ = stage2(
        meta["load_module"],
        meta["header"],
        tmp_path,
        [0],
    )
    insn = functions[0]["instructions"][0]
    assert insn["detail"]["seg_override"] == "CS"
