"""Far-call/jump segment immediates that the EXE relocation table fixes up
must have the load segment applied (mirrors the DOS loader). Without this a
relocated `lcall 0:0` calls null -- a null-segment relocated-lcall startup bug."""
from compiler.ir_to_c import CCodeRenderer


def _render(insn, reloc_seg_word_off):
    # reloc target = the instruction's segment word (last 2 bytes)
    r = CCodeRenderer(
        relocations=[{"segment": reloc_seg_word_off >> 4,
                      "offset": reloc_seg_word_off & 0xF}],
        load_segment=0x1010,
    )
    func = {"start": insn["address"], "instructions": [insn]}
    return "\n".join(r.render_function(func, set()))


def test_lcall_immediate_segment_is_relocated():
    # lcall 0:0 (9A 00 00 00 00) at 0x1518B; seg word at 0x1518E is relocated.
    src = _render(
        {"address": 0x1518B, "mnemonic": "lcall", "op_str": "0, 0",
         "bytes": "9a00000000"},
        0x1518E,
    )
    assert (
        "lcall_table((uint16_t)(0x15190U + 0x10100U - ((uint32_t)cs << 4)), "
        "0x1010, 0x0000);" in src
    )  # 0 + load_segment


def test_lcall_immediate_segment_not_relocated_unchanged():
    # No relocation on the seg word -> segment stays as decoded.
    r = CCodeRenderer(relocations=[], load_segment=0x1010)
    func = {"start": 0x100, "instructions": [
        {"address": 0x100, "mnemonic": "lcall", "op_str": "0x6c0, 0x4a7",
         "bytes": "9aa704c006"}]}
    src = "\n".join(r.render_function(func, set()))
    assert (
        "lcall_table((uint16_t)(0x00105U + 0x10100U - ((uint32_t)cs << 4)), "
        "0x06C0, 0x04A7);" in src
    )


def test_ljmp_immediate_segment_is_relocated():
    # ljmp 0:0x100 (EA 00 01 00 00) at 0x2000; seg word at 0x2003 relocated.
    src = _render(
        {"address": 0x2000, "mnemonic": "ljmp", "op_str": "0:0x100",
         "bytes": "ea00010000"},
        0x2003,
    )
    assert "long_jump(0x1010, 0x0100);" in src  # 0 + load_segment
