import sys
import subprocess
from pathlib import Path


def test_lookup_call_target_logs_file_and_offset(tmp_path):
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
lib.long_jump_impl.argtypes = [
    ctypes.c_uint16,
    ctypes.c_uint16,
    ctypes.c_char_p,
    ctypes.c_char_p,
    ctypes.c_int,
]
lib.long_jump_impl.restype = None

class RegisterBytes(ctypes.Structure):
    _fields_ = [('l', ctypes.c_uint8), ('h', ctypes.c_uint8)]
class Register16(ctypes.Union):
    _fields_ = [('x', ctypes.c_uint16), ('byte', RegisterBytes)]
class Flags(ctypes.Structure):
    _fields_ = [('CF', ctypes.c_uint8), ('PF', ctypes.c_uint8),
                ('ZF', ctypes.c_uint8), ('SF', ctypes.c_uint8),
                ('OF', ctypes.c_uint8), ('IF', ctypes.c_uint8),
                ('DF', ctypes.c_uint8)]
class CPUState(ctypes.Structure):
    _fields_ = [('r_ax', Register16), ('r_bx', Register16),
                ('r_cx', Register16), ('r_dx', Register16),
                ('si', ctypes.c_uint16), ('di', ctypes.c_uint16),
                ('bp', ctypes.c_uint16), ('sp', ctypes.c_uint16),
                ('r_ip', ctypes.c_uint16), ('r_cs', ctypes.c_uint16),
                ('r_ds', ctypes.c_uint16), ('r_es', ctypes.c_uint16),
                ('r_ss', ctypes.c_uint16), ('flags', Flags)]
cpu = CPUState.in_dll(lib, 'cpu')
lib.long_jump_impl(0x100, 0x2, b'test.c', b'test_func', 1)
assert cpu.r_cs == 0x100, hex(cpu.r_cs)
assert cpu.r_ip == 0x2, hex(cpu.r_ip)
"""

    # Run in tmp_path so any bundle (written under ./crashes) does not
    # pollute the repo.
    proc = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        cwd=tmp_path,
    )
    # Faithful far jmp: long_jump_impl just sets cpu.r_cs:cpu.r_ip and returns
    # to the top-level loop -- it no longer resolves/dispatches the target, so
    # there is no lookup_call_target file+offset trace and no exit(1). The
    # unmapped-target detection moved to run_machine's resolve step.
    assert proc.returncode == 0, proc.stderr
    assert "Trace: long_jump to 0100:0002" in proc.stdout
