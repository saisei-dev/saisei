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
    lib.compareMemoryUntilMismatch.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_uint16,
        ctypes.c_int,
    ]
    lib.compareMemoryUntilMismatch.restype = ctypes.c_uint8
    vm_base = ctypes.c_void_p.in_dll(lib, "virtual_memory").value
    return lib, vm_base


def test_compare_forward_and_backward(tmp_path):
    lib, vm_base = compile_lib(tmp_path)
    vm = ctypes.cast(vm_base, ctypes.POINTER(ctypes.c_uint8))

    src_addr = 0x200
    dst_ok_addr = 0x400
    dst_bad_addr = 0x600

    values = [1, 2, 3, 4]
    for i, val in enumerate(values):
        vm[src_addr + i] = val
        vm[dst_ok_addr + i] = val
        vm[dst_bad_addr + i] = val if i < 3 else 5

    assert (
        lib.compareMemoryUntilMismatch(
            ctypes.c_void_p(vm_base + src_addr),
            ctypes.c_void_p(vm_base + dst_ok_addr),
            4,
            1,
        )
        == 1
    )
    assert (
        lib.compareMemoryUntilMismatch(
            ctypes.c_void_p(vm_base + src_addr),
            ctypes.c_void_p(vm_base + dst_bad_addr),
            4,
            1,
        )
        == 0
    )

    src_end = src_addr + 3
    dst_ok_end = dst_ok_addr + 3
    dst_bad_end = dst_bad_addr + 3
    assert (
        lib.compareMemoryUntilMismatch(
            ctypes.c_void_p(vm_base + src_end),
            ctypes.c_void_p(vm_base + dst_ok_end),
            4,
            -1,
        )
        == 1
    )
    assert (
        lib.compareMemoryUntilMismatch(
            ctypes.c_void_p(vm_base + src_end),
            ctypes.c_void_p(vm_base + dst_bad_end),
            4,
            -1,
        )
        == 0
    )


def wrap_addr(base, offset):
    return (base & ~0xFFFF) | ((base + offset) & 0xFFFF)


def test_compare_crosses_boundary(tmp_path):
    lib, vm_base = compile_lib(tmp_path)
    vm = ctypes.cast(vm_base, ctypes.POINTER(ctypes.c_uint8))

    src_addr = 0x1FFFE
    dst_ok_addr = 0x20FFE
    dst_bad_addr = 0x21FFE
    values = [1, 2, 3]
    for i, val in enumerate(values):
        vm[wrap_addr(src_addr, i)] = val
        vm[wrap_addr(dst_ok_addr, i)] = val
        vm[wrap_addr(dst_bad_addr, i)] = val if i < 2 else 9

    assert (
        lib.compareMemoryUntilMismatch(
            ctypes.c_void_p(vm_base + src_addr),
            ctypes.c_void_p(vm_base + dst_ok_addr),
            3,
            1,
        )
        == 1
    )
    assert (
        lib.compareMemoryUntilMismatch(
            ctypes.c_void_p(vm_base + src_addr),
            ctypes.c_void_p(vm_base + dst_bad_addr),
            3,
            1,
        )
        == 0
    )

    src_end = wrap_addr(src_addr, 2)
    dst_ok_end = wrap_addr(dst_ok_addr, 2)
    dst_bad_end = wrap_addr(dst_bad_addr, 2)
    assert (
        lib.compareMemoryUntilMismatch(
            ctypes.c_void_p(vm_base + src_end),
            ctypes.c_void_p(vm_base + dst_ok_end),
            3,
            -1,
        )
        == 1
    )
    assert (
        lib.compareMemoryUntilMismatch(
            ctypes.c_void_p(vm_base + src_end),
            ctypes.c_void_p(vm_base + dst_bad_end),
            3,
            -1,
        )
        == 0
    )
