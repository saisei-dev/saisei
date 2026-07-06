import ctypes
import subprocess
import textwrap
from pathlib import Path


def test_memb_returns_unsigned(tmp_path):
    repo_root = Path(__file__).resolve().parents[1]
    helper_c = tmp_path / "helper.c"
    helper_c.write_text(
        textwrap.dedent(
            """
            #include <stdint.h>
            #include "shims.h"

            uint8_t call_memb(uint16_t seg, uint16_t off) {
                return memb(seg, off);
            }

            void call_memb_write(uint16_t seg, uint16_t off, uint8_t value) {
                memb_write(seg, off, value);
            }
            """
        )
    )
    so_path = tmp_path / "shims.so"
    subprocess.check_call(
        [
            "gcc",
            "-shared",
            "-fPIC",
            "-Iruntime/include",
            "runtime/core/shims.c",
            "runtime/core/save_manager.c",
            "runtime/core/snapshot.c",
            "runtime/hw/io_bus.c",
            "runtime/hw/audio.c",
            "runtime/hw/video.c",
            "runtime/hw/keyboard.c",
            "runtime/hw/timer.c",
            "runtime/os/dos.c",
            "runtime/os/bios.c",
            "runtime/os/mouse.c",
            str(helper_c),
            "-o",
            str(so_path),
        ],
        cwd=repo_root,
    )
    lib = ctypes.CDLL(str(so_path))
    lib.call_memb.argtypes = [ctypes.c_uint16, ctypes.c_uint16]
    lib.call_memb.restype = ctypes.c_uint8
    lib.call_memb_write.argtypes = [
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.c_uint8,
    ]
    lib.call_memb_write.restype = None

    seg = 0x2000
    off = 0x0000
    value = 0xAB

    lib.call_memb_write(seg, off, value)
    assert lib.call_memb(seg, off) == value
