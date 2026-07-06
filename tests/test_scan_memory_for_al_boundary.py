import ctypes
import subprocess
from pathlib import Path


def compile_lib(tmp_path):
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
    lib.scanMemoryForAl.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint8,
        ctypes.c_uint16,
        ctypes.c_int,
        ctypes.POINTER(ctypes.c_uint8),
    ]
    lib.scanMemoryForAl.restype = ctypes.c_uint16
    vm_base = ctypes.c_void_p.in_dll(lib, "virtual_memory").value
    return lib, vm_base


def wrap_addr(base, offset):
    return (base & ~0xFFFF) | ((base + offset) & 0xFFFF)


def test_scan_forward_crosses_boundary(tmp_path):
    lib, vm_base = compile_lib(tmp_path)
    vm = ctypes.cast(vm_base, ctypes.POINTER(ctypes.c_uint8))

    base = 0x1FFFE
    vm[wrap_addr(base, 0)] = 1
    vm[wrap_addr(base, 1)] = 2
    vm[wrap_addr(base, 2)] = 7

    last_byte = ctypes.c_uint8()
    result = lib.scanMemoryForAl(
        ctypes.c_void_p(vm_base + base), 7, 3, 1, ctypes.byref(last_byte)
    )
    assert result == 2


def test_scan_backward_crosses_boundary(tmp_path):
    lib, vm_base = compile_lib(tmp_path)
    vm = ctypes.cast(vm_base, ctypes.POINTER(ctypes.c_uint8))

    base = 0x1FFFE
    vm[wrap_addr(base, 0)] = 7
    vm[wrap_addr(base, 1)] = 2
    vm[wrap_addr(base, 2)] = 1

    start = wrap_addr(base, 2)
    last_byte = ctypes.c_uint8()
    result = lib.scanMemoryForAl(
        ctypes.c_void_p(vm_base + start), 7, 3, -1, ctypes.byref(last_byte)
    )
    assert result == 2
