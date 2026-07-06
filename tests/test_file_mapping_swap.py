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


def test_file_mapping_swap(tmp_path, capfd):
    lib = build_shims(tmp_path)

    shim_log_file_load = lib.shim_log_file_load
    shim_log_file_load.argtypes = [
        ctypes.c_char_p,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.c_size_t,
    ]
    shim_log_file_load.restype = None

    file_mapping_swap = lib.file_mapping_swap_impl
    file_mapping_swap.argtypes = [
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.c_size_t,
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.c_int,
    ]
    file_mapping_swap.restype = None

    virtual_memory = ctypes.c_void_p.in_dll(lib, "virtual_memory")
    addr1 = virtual_memory.value + 0x4000
    addr2 = virtual_memory.value + 0x5000

    shim_log_file_load(b"a.bin", ctypes.c_void_p(addr1), 4, 0)
    shim_log_file_load(b"b.bin", ctypes.c_void_p(addr2), 4, 0)

    file_mapping_swap(
        0x4000 >> 4,
        0x4000 & 0xF,
        0x5000 >> 4,
        0x5000 & 0xF,
        4,
        b"t",
        b"f",
        1,
    )

    out = capfd.readouterr().out
    assert "file_mapping_swap" in out


def test_file_mapping_swap_skips_partial_rebase(tmp_path, capfd):
    lib = build_shims(tmp_path)

    shim_log_file_load = lib.shim_log_file_load
    shim_log_file_load.argtypes = [
        ctypes.c_char_p,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.c_size_t,
    ]
    shim_log_file_load.restype = None

    file_mapping_swap = lib.file_mapping_swap_impl
    file_mapping_swap.argtypes = [
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.c_size_t,
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.c_int,
    ]
    file_mapping_swap.restype = None

    virtual_memory = ctypes.c_void_p.in_dll(lib, "virtual_memory")
    addr1 = virtual_memory.value + 0x4000
    addr2 = virtual_memory.value + 0x5000

    # Register mappings larger than the swap range so rebasing should be
    # skipped; otherwise diagnostics can lose track of the originating file.
    shim_log_file_load(b"a.bin", ctypes.c_void_p(addr1), 8, 0)
    shim_log_file_load(b"b.bin", ctypes.c_void_p(addr2), 8, 0)

    file_mapping_swap(
        0x4000 >> 4,
        0x4000 & 0xF,
        0x5000 >> 4,
        0x5000 & 0xF,
        4,
        b"t",
        b"f",
        1,
    )

    out = capfd.readouterr().out
    assert "skipped rebasing" in out
