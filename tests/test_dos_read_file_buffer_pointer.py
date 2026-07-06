import ctypes
import subprocess
from pathlib import Path


class Register16(ctypes.Structure):
    _fields_ = [("x", ctypes.c_uint16)]


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
    ]


def build_shims(tmp_path: Path) -> ctypes.CDLL:
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


def test_dos_read_file_uses_buffer_pointer_not_ds_dx(tmp_path, monkeypatch):
    lib = build_shims(tmp_path)
    # dos_open resolves DOS paths within the process working dir (and strips any
    # leading drive/root separators, per DOS semantics), so run from tmp_path and
    # open the file by its relative name.
    monkeypatch.chdir(tmp_path)

    cpu = CPUState.in_dll(lib, "cpu")
    cpu.r_ds = 0
    cpu.r_dx.x = 0

    dos_open_file = lib.dos_open_file
    dos_open_file.argtypes = [ctypes.c_char_p]
    dos_open_file.restype = ctypes.c_uint8

    dos_read_file = lib.dos_read_file
    dos_read_file.argtypes = [ctypes.c_uint16, ctypes.c_void_p, ctypes.c_uint16]
    dos_read_file.restype = ctypes.c_uint8

    dos_close_file = lib.dos_close_file
    dos_close_file.argtypes = [ctypes.c_uint16]
    dos_close_file.restype = ctypes.c_uint8

    payload = b"ABCD"
    sample = tmp_path / "sample.bin"
    sample.write_bytes(payload)

    assert dos_open_file(b"sample.bin") == 0
    handle = cpu.r_ax.x

    virtual_memory = ctypes.c_void_p.in_dll(lib, "virtual_memory")
    target_addr = virtual_memory.value + 0x4000
    target_buf = (ctypes.c_ubyte * len(payload)).from_address(target_addr)
    for i in range(len(payload)):
        target_buf[i] = 0

    ivt_before = bytes((ctypes.c_ubyte * len(payload)).from_address(virtual_memory.value))

    assert dos_read_file(handle, ctypes.c_void_p(target_addr), len(payload)) == 0
    assert bytes(target_buf) == payload

    ivt_after = bytes((ctypes.c_ubyte * len(payload)).from_address(virtual_memory.value))
    assert ivt_after == ivt_before

    assert dos_close_file(handle) == 0
