from pathlib import Path

from compiler.disassemble import stage1, stage2
from compiler.ir_to_c import CCodeRenderer


def test_cross_func_backward_jmp_no_label(tmp_path: Path) -> None:
    data = bytes([0xC3, 0x90, 0xE9, 0xFB, 0xFF])
    meta = stage1(data, tmp_path, [0x0002])
    _, functions, _, _, extern_labels = stage2(
        meta["load_module"], meta["header"], tmp_path, extra_entries=[0x0002]
    )
    assert 0x0000 not in extern_labels
    renderer = CCodeRenderer(extern_labels=set(extern_labels))
    known_funcs = {f["start"] for f in functions}
    func_lines = []
    for f in functions:
        lines = renderer.render_function(f, known_funcs)
        if f["start"] == 0x0002:
            func_lines = lines
    src = "\n".join(func_lines)
    assert "label_0000" not in src
    assert "dispatch(" not in src
