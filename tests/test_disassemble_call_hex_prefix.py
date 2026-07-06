from pathlib import Path

from compiler.disassemble import stage1, stage2
from compiler.ir_to_c import CCodeRenderer


def test_call_with_hex_prefix_generates_function_call(tmp_path: Path) -> None:
    # call 0x0CAD; ret
    data = bytearray(0x0CAD + 1)
    data[0:4] = [0xE8, 0xAA, 0x0C, 0xC3]
    data[0x0CAD] = 0xC3
    meta = stage1(bytes(data), tmp_path)
    _, functions, _, _, _ = stage2(
        meta["load_module"], meta["header"], tmp_path
    )
    renderer = CCodeRenderer(name_prefix="game_")
    known_funcs = {f["start"] for f in functions}
    lines = []
    for f in functions:
        if f["start"] == 0x0000:
            lines = renderer.render_function(f, known_funcs)
            break
    src = "\n".join(lines)
    # The call immediate (0x0CAD) is resolved and routed through the
    # in-loop dispatch: set pc to the target and continue.
    assert "pc = 0x0CAD;" in src
    assert "// TODO ASM: call 0xcad" not in src
