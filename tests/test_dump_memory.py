import sys
import subprocess
import ctypes  # noqa: F401
from pathlib import Path


def test_dump_memory(tmp_path):
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
    script = f"""
import ctypes
lib = ctypes.CDLL(r"{so_path}")
lib.shim_dump_memory.argtypes = [ctypes.c_uint32, ctypes.c_size_t]
lib.shim_dump_memory.restype = None
vm = ctypes.c_void_p.in_dll(lib, 'virtual_memory').value
buf = (ctypes.c_uint8 * 32).from_address(vm + 0x200)
for i in range(32):
    buf[i] = i
lib.shim_dump_memory(0x200, 32)
"""
    proc = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        cwd=repo_root,
    )
    assert proc.returncode == 0
    assert (
        "000200: 00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F  |................|\n"  # noqa: E501
        in proc.stdout
    )
    assert (
        "000210: 10 11 12 13 14 15 16 17 18 19 1A 1B 1C 1D 1E 1F  |................|\n"  # noqa: E501
        in proc.stdout
    )
