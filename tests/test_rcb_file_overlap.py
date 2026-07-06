import ctypes
import subprocess
from pathlib import Path

# flake8: noqa


def test_file_read_warns_on_rcb_overlap(tmp_path, capfd, monkeypatch):
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
    lib.dos_open_file.argtypes = [ctypes.c_char_p]
    lib.dos_open_file.restype = ctypes.c_uint8
    lib.dos_read_file.argtypes = [ctypes.c_uint16, ctypes.c_void_p, ctypes.c_uint16]
    lib.dos_read_file.restype = ctypes.c_uint8
    lib.dos_close_file.argtypes = [ctypes.c_uint16]
    lib.dos_close_file.restype = ctypes.c_uint8

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

    cpu = CPUState.in_dll(lib, "cpu")
    virtual_memory = ctypes.c_void_p.in_dll(lib, "virtual_memory")

    # dos_open resolves DOS paths within the process working dir (stripping any
    # leading drive/root separators), so run from tmp_path and open by name.
    monkeypatch.chdir(tmp_path)
    test_file = tmp_path / "file.bin"
    test_file.write_bytes(b"abcdef")
    assert lib.dos_open_file(b"file.bin") == 0
    handle = cpu.r_ax.x

    rcb_base = (cpu.r_es << 4) + 0xFF00
    buf_addr = virtual_memory.value + rcb_base - 1
    cpu.r_ds = cpu.r_es
    cpu.r_dx.x = 0xFF00 - 1
    lib.dos_read_file(handle, ctypes.c_void_p(buf_addr), 6)
    output = capfd.readouterr().out
    assert "Warning: file" in output
    assert "FIELD_1" in output
    assert "PROGRAM_SEG" in output
    assert "PREV_TIMER_VECTOR_OFF" in output
    assert lib.dos_close_file(handle) == 0

