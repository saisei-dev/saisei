import ctypes
import subprocess
from pathlib import Path


def build_shims(tmp_path):
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
    return ctypes.CDLL(str(so_path))


def test_warn_on_file_overwrite(tmp_path, capfd):
    lib = build_shims(tmp_path)

    shim_log_file_load = lib.shim_log_file_load
    shim_log_file_load.argtypes = [
        ctypes.c_char_p,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.c_size_t,
    ]
    shim_log_file_load.restype = None

    virtual_memory = ctypes.c_void_p.in_dll(lib, "virtual_memory")
    addr1 = ctypes.c_void_p(virtual_memory.value + 0x3000)
    addr2 = ctypes.c_void_p(virtual_memory.value + 0x3002)

    ctypes.memmove(addr1, b"\x01\x02\x03\x04", 4)
    shim_log_file_load(b"a.bin", addr1, 4, 0)
    ctypes.memmove(addr2, b"\xFF\xEE\xDD\xCC", 4)
    shim_log_file_load(b"b.bin", addr2, 4, 0)

    out = capfd.readouterr().out
    assert "WARNING: file b.bin overwrote a.bin at 0x03002-0x03004" in out
    assert "old bytes: 03 04" in out
    assert "new bytes: FF EE" in out
