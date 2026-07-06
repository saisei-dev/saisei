from pathlib import Path

from compiler.disassemble import stage1, stage2
from compiler.ir_to_c import CCodeRenderer


def test_backward_jmp_ignored(tmp_path: Path) -> None:
    data = bytes([0x90, 0xEB, 0xFD, 0xC3])
    meta = stage1(data, tmp_path, [0x0])
    _, functions, _, _, extern_labels = stage2(
        meta["load_module"], meta["header"], tmp_path, [0x0]
    )
    assert 0x0000 not in extern_labels
    renderer = CCodeRenderer(extern_labels=set(extern_labels))
    body_lines = renderer.render_function(
        functions[0], {functions[0]["start"]}
    )
    src = "\n".join(body_lines)
    assert "dispatch(" not in src
