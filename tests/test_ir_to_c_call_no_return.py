import sys
from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import PCSwitchRenderer  # noqa: E402


def test_handle_call_omits_return_for_fallthrough_calls():
    renderer = PCSwitchRenderer()
    renderer.func_names[0x0100] = "func_0100"
    renderer.current_blocks = {}
    insn = {
        "address": 0x0000,
        "mnemonic": "call",
        "op_str": "0100",
        "bytes": "E80000",
        "target": 0x0100,
    }
    lines = renderer.handle_call(insn, {0x0100})
    # Faithful flat model: a direct near call pushes the return IP on the
    # emulated stack and sets pc to the target, then `continue`s the dispatch
    # loop. No C recursion, no `return;` after a fall-through call -- the same
    # rule covers intra-chunk (the switch routes to the case) and extern (the
    # `default:` arm exits to the top-level loop) targets.
    assert lines == [
        "// ASM: call 0100",
        "{",
        "    sp = (sp - 2) & 0xFFFF;",
        "    memw_write(ss, sp, (uint16_t)(0x00003U + 0x10100U - ((uint32_t)cs << 4)));",
        "    pc = 0x0100;",
        "    continue;",
        "}",
    ]
    assert "return;" not in lines
