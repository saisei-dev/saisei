from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def render(func):
    return "\n".join(CCodeRenderer().render_function(func, set()))


def _wrap(instr):
    return {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, **instr},
            {"address": 0x0010, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }


def test_leave_is_mov_sp_bp_then_pop_bp():
    # LEAVE (0xC9): SP <- BP; BP <- pop(). Real, common epilogue instruction.
    src = render(_wrap({"mnemonic": "leave", "op_str": "", "bytes": "C9"}))
    body = src[src.index("sp = bp;"):]
    assert "sp = bp;" in body
    assert "bp = memw(ss, sp);" in body
    assert "sp = (sp + 2) & 0xFFFF;" in body
    # order matters: SP is set from BP *before* the pop reads [SS:SP].
    assert body.index("sp = bp;") < body.index("bp = memw(ss, sp);")
    assert body.index("bp = memw(ss, sp);") < body.index("sp = (sp + 2) & 0xFFFF;")


def test_enter_level0_pushes_bp_sets_frame_and_allocs():
    # ENTER 0x10, 0: push bp; bp = sp; sp -= 0x10 (no frame-pointer copies).
    src = render(_wrap({"mnemonic": "enter", "op_str": "0x10, 0", "bytes": "C81000"}))
    assert "sp = (sp - 2) & 0xFFFF;" in src
    assert "memw_write(ss, sp, bp);" in src
    assert "bp = frame_temp;" in src
    assert "sp = (sp - 0x10) & 0xFFFF;" in src
    # level 0 copies no enclosing frame pointers
    assert "memw_write(ss, sp, memw(ss, bp));" not in src


def test_enter_zero_alloc_emits_no_sub():
    # ENTER 0, 0: push bp; bp = sp (alloc 0 => no SP subtraction).
    src = render(_wrap({"mnemonic": "enter", "op_str": "0, 0", "bytes": "C80000"}))
    assert "memw_write(ss, sp, bp);" in src
    assert "bp = frame_temp;" in src
    assert "(sp - 0x0)" not in src


def test_enter_level1_pushes_frame_temp():
    # ENTER 8, 1: nesting level 1 pushes the just-saved FrameTemp once.
    src = render(_wrap({"mnemonic": "enter", "op_str": "8, 1", "bytes": "C80800"}))
    assert "uint16_t frame_temp = sp;" in src
    assert "memw_write(ss, sp, frame_temp);" in src
    # level 1 has no interior bp-walk copies (that starts at level 2)
    assert "memw_write(ss, sp, memw(ss, bp));" not in src


def test_enter_level2_copies_one_enclosing_pointer():
    # ENTER 0, 2: level 2 walks bp once and copies one enclosing frame pointer.
    src = render(_wrap({"mnemonic": "enter", "op_str": "0, 2", "bytes": "C80002"}))
    assert "bp = (bp - 2) & 0xFFFF;" in src
    assert "memw_write(ss, sp, memw(ss, bp));" in src
    assert "memw_write(ss, sp, frame_temp);" in src
