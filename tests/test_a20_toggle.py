import ctypes
import subprocess
from pathlib import Path


def test_a20_toggle(tmp_path):
    repo_root = Path(__file__).resolve().parents[1]
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
            "-o",
            str(so_path),
        ],
        cwd=repo_root,
    )
    lib = ctypes.CDLL(str(so_path))
    lib.outb.argtypes = [ctypes.c_uint16, ctypes.c_uint8]
    lib.memb_write_impl.argtypes = [
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.c_uint8,
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.c_int,
    ]

    vm = ctypes.c_void_p.in_dll(lib, "virtual_memory").value

    # ensure A20 disabled
    lib.outb(0x92, 0x00)
    lib.memb_write_impl(0xFFFF, 0x0010, 0xAA, b"", b"", 0)
    assert ctypes.c_uint8.from_address(vm + 0).value == 0xAA
    assert ctypes.c_uint8.from_address(vm + 0x100000).value == 0x00

    # enable A20
    lib.outb(0x92, 0x02)
    lib.memb_write_impl(0xFFFF, 0x0010, 0x55, b"", b"", 0)
    assert ctypes.c_uint8.from_address(vm + 0x100000).value == 0x55
    assert ctypes.c_uint8.from_address(vm + 0).value == 0xAA
