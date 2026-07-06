import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.disassemble import stage1, stage2  # noqa: E402
from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_invalid_opcode_is_noop(tmp_path: Path) -> None:
    # mov ax, 0x1234; followed by an incomplete opcode 0x0F.
    # disassemble.py emits the undecodable trailing byte as `db 0x0f`.
    # The translator now treats `db` (EOF tail / padding) as a noop with
    # no output, rather than emitting a "// TODO ASM" comment.
    data = bytes([0xB8, 0x34, 0x12, 0x0F])
    meta = stage1(data, tmp_path)
    _, functions, _, _, _ = stage2(
        meta["load_module"], meta["header"], tmp_path
    )
    renderer = CCodeRenderer()
    known_funcs = {f["start"] for f in functions}
    lines = []
    for f in functions:
        if f["start"] == 0x0000:
            lines = renderer.render_function(f, known_funcs)
            break
    src = "\n".join(lines)
    assert "// TODO ASM" not in src
    assert "0x0f" not in src
    assert "ax = 0x1234;" in src
