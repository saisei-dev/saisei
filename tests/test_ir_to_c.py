import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

import pytest

from compiler.ir_to_c import (  # noqa: E402
    CCodeRenderer,
    UnsupportedInstructionError,
)


def render_function(func, known):
    # Direct calls render as the dispatch-loop continue pattern, which
    # requires a per-binary name prefix on the renderer.
    return CCodeRenderer(name_prefix="app_").render_function(func, known)


def test_call_to_known_address_emits_function_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "0x05F9",
                "bytes": "E80000",
                "target": 0x05F9,
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000, 0x05F9}
    lines = render_function(func, known)
    src = "\n".join(lines)
    # A direct call to a known target is translated to-the-letter: push
    # the return IP and dispatch to the target via the pc switch loop.
    assert "memw_write(ss, sp, (uint16_t)(0x00003U + 0x10100U - ((uint32_t)cs << 4)));" in src
    assert "pc = 0x05F9;" in src
    assert "// TODO ASM: call" not in src


def test_call_to_unknown_address_still_translates_literally():
    # The old "unknown call target stays a // TODO ASM comment" behavior
    # is gone: every direct call now gets literal asm semantics (push
    # retIP, set pc, continue). The dispatch switch resolves the target
    # at runtime, or near_ret_tail aborts on a genuinely bogus one.
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "call",
                "op_str": "0x1234",
                "bytes": "E80000",
                "target": 0x1234,
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0000}
    lines = render_function(func, known)
    src = "\n".join(lines)
    assert "memw_write(ss, sp, (uint16_t)(0x00003U + 0x10100U - ((uint32_t)cs << 4)));" in src
    assert "pc = 0x1234;" in src
    assert "// TODO ASM: call" not in src


def test_ret_instruction_emits_return_statement():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "return;" in src
    assert "// TODO ASM: ret" not in src


def test_and_instruction_is_rendered():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "and",
                "op_str": "al, dh",
                "bytes": "22F0",
            },
            {
                "address": 0x0002,
                "mnemonic": "and",
                "op_str": "al, 0x44",
                "bytes": "2444",
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "al = (al & dh) & 0xFF;" in src
    assert "al = (al & 0x44) & 0xFF;" in src


def test_dos_interrupt_with_known_ah_is_rendered_as_named_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ah, 9",
                "bytes": "B409",
            },
            {
                "address": 0x0002,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0004,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_print_string((const char *)seg_off(ds, dx));" in src


def test_dos_interrupt_without_ah_uses_generic_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "dos_api();" in src


def test_dos_interrupt_preserves_unrelated_registers():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0x1234", "bytes": "B93412"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ah, 0x02", "bytes": "B402"},
            {"address": 0x0005, "mnemonic": "mov", "op_str": "dl, 0x41", "bytes": "B241"},
            {"address": 0x0007, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0009, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_write_char(0x41);" in src
    assert "cx = 0x1234;" in src
    assert src.index("cx = 0x1234;") < src.index("dos_write_char")


def test_xchg_invalidates_cached_dos_register_arguments():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dx, 0x1234", "bytes": "BA3412"},
            {"address": 0x0003, "mnemonic": "xchg", "op_str": "dx, bx", "bytes": "87DA"},
            {"address": 0x0005, "mnemonic": "mov", "op_str": "ah, 0x3d", "bytes": "B43D"},
            {"address": 0x0007, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0009, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_open_file((const char *)seg_off(ds, dx));" in src
    assert "CF = dos_open_file((const char *)seg_off(cs, 0x1234));" not in src


def test_interrupt_emits_run_interrupt():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "int",
                "op_str": "60",
                "bytes": "CD60",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "ip = 0x0000;" in src
    assert "run_interrupt(0x60);" in src


def test_interrupt_1a_emits_run_interrupt():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "int",
                "op_str": "1a",
                "bytes": "CD1A",
            },
            {
                "address": 0x0002,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "ip = 0x0000;" in src
    assert "run_interrupt(0x1A);" in src


def test_dos_open_file_is_rendered_as_named_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, 3D00h",
                "bytes": "B8003D",
            },
            {
                "address": 0x0003,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_open_file(" in src


def test_dos_read_file_uses_buffer_and_len():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, 3F00h",
                "bytes": "B8003F",
            },
            {
                "address": 0x0003,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_read_file(bx, (void *)seg_off(ds, dx), cx);" in src


def test_dos_print_string_uses_pointer():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "dx, 0x0100",
                "bytes": "BA0001",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "ah, 9",
                "bytes": "B409",
            },
            {
                "address": 0x0005,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0007,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    renderer = CCodeRenderer()
    lines = renderer.render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_print_string((const char *)seg_off(cs, 0x0100));" in src


def test_dos_open_file_uses_pointer():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "dx, 0x0100",
                "bytes": "BA0001",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "ax, 0x3d00",
                "bytes": "B8003D",
            },
            {
                "address": 0x0006,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    renderer = CCodeRenderer()
    lines = renderer.render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_open_file((const char *)seg_off(cs, 0x0100));" in src


def test_mov_ax_without_interrupt_is_emitted():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, 1234h",
                "bytes": "B83412",
            },
            {
                "address": 0x0003,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "ax =" in src


def test_dos_alloc_mem_includes_constant():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "bx, 0x0100",
                "bytes": "BB0001",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "ax, 0x4800",
                "bytes": "B80048",
            },
            {
                "address": 0x0006,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_alloc_mem(0x0100);" in src


def test_dos_exec_uses_pointer():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "dx, 0x0100",
                "bytes": "BA0001",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "bx, 0x0120",
                "bytes": "BB2001",
            },
            {
                "address": 0x0006,
                "mnemonic": "mov",
                "op_str": "ax, 0x4b00",
                "bytes": "B8004B",
            },
            {
                "address": 0x0009,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x000B,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    renderer = CCodeRenderer()
    lines = renderer.render_function(func, set())
    src = "\n".join(lines)
    assert 'CF = dos_exec((void *)0x0120, (const char *)seg_off(cs, 0x0100));' in src


def test_dos_set_interrupt_vector_builds_far_pointer():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, 0x2521",
                "bytes": "B82125",
            },
            {
                "address": 0x0003,
                "mnemonic": "mov",
                "op_str": "dx, 0x0100",
                "bytes": "BA0001",
            },
            {
                "address": 0x0006,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0008,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert (
        'CF = dos_set_interrupt_vector(0x21, ds, 0x0100);'
        in src
    )


def test_dos_get_interrupt_vector_emits_mov_ax():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, 0x3508",
                "bytes": "B80835",
            },
            {
                "address": 0x0003,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "ax = 0x3508;" in src
    assert "CF = dos_get_interrupt_vector();" in src


def test_lds_loads_far_pointer_for_dos_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "mov",
                "op_str": "ax, 0x2560",
                "bytes": "B86025",
            },
            {
                "address": 0x0003,
                "mnemonic": "lds",
                "op_str": "dx, ptr cs:[0x8b9]",
                "bytes": "2EC516B90800",
            },
            {
                "address": 0x0009,
                "mnemonic": "int",
                "op_str": "21",
                "bytes": "CD21",
            },
            {
                "address": 0x000B,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    # Faithful far-pointer load: read the segment word into a temp BEFORE
    # mutating the destination register, so a self-referential `lds dx,[...]`
    # uses the original effective address for both reads.
    assert "uint16_t _far_seg = memw(cs, 0x08BB);" in src
    assert "dx = memw(cs, 0x08B9);" in src
    assert "ds = _far_seg;" in src
    # The intervening lds clears the mov-ax register tracking, so the INT 21h
    # falls back to the generic runtime dispatcher (dos_api reads the real ah
    # and uses the ds:dx the lds just loaded) rather than a statically-resolved
    # dos_set_interrupt_vector call. Both are faithful; the point of this test
    # is the far-pointer load ordering above.
    assert "dos_api();" in src


def test_les_loads_far_pointer():
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "les",
                "op_str": "di, ptr cs:[0xf60]",
                "bytes": "2EC43E600F",
            },
            {
                "address": 0x0005,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    # Faithful far-pointer load: segment word captured before dest mutation.
    assert "uint16_t _far_seg = memw(cs, 0x0F62);" in src
    assert "di = memw(cs, 0x0F60);" in src
    assert "es = _far_seg;" in src


def test_dos_reset_disk_is_rendered_as_named_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ah, 0x0d", "bytes": "B40D"},
            {"address": 0x0002, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_reset_disk();" in src


def test_dos_set_dta_includes_constant():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dx, 0x0100", "bytes": "BA0001"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ah, 0x1a", "bytes": "B41A"},
            {"address": 0x0005, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_set_dta((void *)seg_off(cs, 0x0100));" in src


def test_dos_select_drive_uses_dl():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ah, 0x0e", "bytes": "B40E"},
            {"address": 0x0002, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_select_drive(dl);" in src


def test_dos_get_current_drive_is_rendered():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ah, 0x19", "bytes": "B419"},
            {"address": 0x0002, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_get_current_drive();" in src


def test_dos_get_disk_free_space_uses_dl():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ah, 0x36", "bytes": "B436"},
            {"address": 0x0002, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0004, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_get_disk_free_space(dl);" in src


def test_dos_make_dir_uses_pointer():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dx, 0x0100", "bytes": "BA0001"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ax, 0x3900", "bytes": "B80039"},
            {"address": 0x0006, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    renderer = CCodeRenderer()
    lines = renderer.render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_make_dir((const char *)seg_off(cs, 0x0100));" in src


def test_dos_change_dir_uses_pointer():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dx, 0x0100", "bytes": "BA0001"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ax, 0x3b00", "bytes": "B8003B"},
            {"address": 0x0006, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    renderer = CCodeRenderer()
    lines = renderer.render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_change_dir((const char *)seg_off(cs, 0x0100));" in src


def test_dos_find_first_uses_pointer_and_cx():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dx, 0x0100", "bytes": "BA0001"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ax, 0x4e00", "bytes": "B8004E"},
            {"address": 0x0006, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    renderer = CCodeRenderer()
    lines = renderer.render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_find_first((const char *)seg_off(cs, 0x0100), cx);" in src


def test_dos_lseek_includes_arguments():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "bx, 0x0005", "bytes": "BB0500"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "dx, 0x0010", "bytes": "BA1000"},
            {"address": 0x0006, "mnemonic": "mov", "op_str": "ax, 0x4202", "bytes": "B80242"},
            {"address": 0x0009, "mnemonic": "int", "op_str": "21", "bytes": "CD21"},
            {"address": 0x000B, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "CF = dos_lseek(0x0005, cx, 0x0010, 0x02);" in src


def test_bios_video_mode_interrupt_emits_named_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, 13h", "bytes": "B81300"},
            {"address": 0x0003, "mnemonic": "int", "op_str": "10", "bytes": "CD10"},
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "bios_set_video_mode(0x13);" in src
    assert "// TODO ASM: int 10" not in src
    assert "ax =" not in src


def test_bios_set_palette_interrupt_emits_named_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "cx, 0x0002", "bytes": "B90200"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "dx, 0x0200", "bytes": "BA0002"},
            {"address": 0x0006, "mnemonic": "mov", "op_str": "bx, 0x0001", "bytes": "BB0100"},
            {"address": 0x0009, "mnemonic": "mov", "op_str": "ax, 0x1012", "bytes": "B81210"},
            {"address": 0x000C, "mnemonic": "int", "op_str": "10", "bytes": "CD10"},
            {"address": 0x000E, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "bios_set_palette();" in src
    assert "// TODO ASM: int 10" not in src
    assert "ax = 0x1012;" in src


def test_bios_cga_palette_interrupt_emits_named_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "bx, 0x0101", "bytes": "BB0101"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "ah, 0x0B", "bytes": "B40B"},
            {"address": 0x0005, "mnemonic": "int", "op_str": "10", "bytes": "CD10"},
            {"address": 0x0007, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "bios_set_cga_palette(0x01, 0x01);" in src


def test_bios_cursor_position_interrupt_emits_named_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "dx, 0x0514", "bytes": "BA1405"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "bx, 0x0000", "bytes": "BB0000"},
            {"address": 0x0006, "mnemonic": "mov", "op_str": "ah, 0x02", "bytes": "B402"},
            {"address": 0x0008, "mnemonic": "int", "op_str": "10", "bytes": "CD10"},
            {"address": 0x000A, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "bios_set_cursor_position(0x00, 0x05, 0x14);" in src


def test_bios_teletype_interrupt_emits_named_call():
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, 0x0E41", "bytes": "B8410E"},
            {"address": 0x0003, "mnemonic": "mov", "op_str": "bx, 0x0007", "bytes": "BB0700"},
            {"address": 0x0006, "mnemonic": "int", "op_str": "10", "bytes": "CD10"},
            {"address": 0x0008, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    lines = render_function(func, set())
    src = "\n".join(lines)
    assert "bios_teletype_output(0x41, 0x00, 0x07);" in src


def test_invalidate_register_clears_cached_bx_for_byte_writes():
    renderer = CCodeRenderer()
    renderer.last_bx = 0x1207

    renderer._invalidate_register("bh")

    assert renderer.last_bx is None


def test_unsupported_bios_interrupt_raises():
    # An unsupported BIOS interrupt now hard-fails (UnsupportedInstruction
    # Error) instead of silently emitting a `// TODO ASM` comment, so a
    # dropped/untranslated instruction can never be masked.
    func = {
        "start": 0x0000,
        "instructions": [
            {"address": 0x0000, "mnemonic": "mov", "op_str": "ax, 0x0100", "bytes": "B80001"},
            {"address": 0x0003, "mnemonic": "int", "op_str": "10", "bytes": "CD10"},
            {"address": 0x0005, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
        ],
    }
    with pytest.raises(UnsupportedInstructionError):
        render_function(func, set())
