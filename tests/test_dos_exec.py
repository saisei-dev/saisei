"""Tests for dos_exec shim."""
# flake8: noqa
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
    return so_path


def define_cpu_state(lib):
    class RegisterBytes(ctypes.Structure):
        _fields_ = [("l", ctypes.c_uint8), ("h", ctypes.c_uint8)]

    class Register16(ctypes.Union):
        _fields_ = [("x", ctypes.c_uint16), ("byte", RegisterBytes)]

    class Flags(ctypes.Structure):
        _fields_ = [
            ("CF", ctypes.c_uint8),
            ("ZF", ctypes.c_uint8),
            ("SF", ctypes.c_uint8),
            ("OF", ctypes.c_uint8),
            ("IF", ctypes.c_uint8),
            ("DF", ctypes.c_uint8),
        ]

    class CPUState(ctypes.Structure):
        _fields_ = [
            ("r_ax", Register16),
            ("r_bx", Register16),
            ("r_cx", Register16),
            ("r_dx", Register16),
            ("si", ctypes.c_uint16),
            ("di", ctypes.c_uint16),
            ("bp", ctypes.c_uint16),
            ("sp", ctypes.c_uint16),
            ("r_ip", ctypes.c_uint16),
            ("r_cs", ctypes.c_uint16),
            ("r_ds", ctypes.c_uint16),
            ("r_es", ctypes.c_uint16),
            ("r_ss", ctypes.c_uint16),
            ("flags", Flags),
        ]

    return CPUState


def test_dos_exec_missing_file(tmp_path):
    so_path = build_shims(tmp_path)
    lib = ctypes.CDLL(str(so_path))
    CPUState = define_cpu_state(lib)
    cpu = CPUState.in_dll(lib, "cpu")
    lib.dos_exec.restype = ctypes.c_uint8

    result = lib.dos_exec(ctypes.c_void_p(0), b"no_such_file.exe")

    assert result == 1
    assert cpu.flags.CF == 1

