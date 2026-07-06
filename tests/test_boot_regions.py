import ctypes
import subprocess
from pathlib import Path


def test_dos_boot_regions_initialised(tmp_path):
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
    mem_ptr = ctypes.c_void_p.in_dll(lib, "virtual_memory").value
    mem = (ctypes.c_uint8 * (1 << 20)).from_address(mem_ptr)

    # BIOS Data Area clock ticks
    ticks_off = (0x40 << 4) + 0x6C
    ticks = int.from_bytes(bytes(mem[ticks_off:ticks_off + 4]), "little")
    assert ticks == 0

    # PSP command tail
    psp_cmd = (0x1000 << 4) + 0x80
    assert mem[psp_cmd] == 0
    assert mem[psp_cmd + 1] == 0x0D

    # Environment block pointer
    env_ptr_off = (0x1000 << 4) + 0x2C
    env_seg = mem[env_ptr_off] | (mem[env_ptr_off + 1] << 8)
    assert env_seg == 0x0FF0
    env_off = env_seg << 4
    # A faithful DOS environment block is non-empty: COMMAND.COM always launches
    # a program with COMSPEC / PATH / PROMPT set (real DOS never passes an empty
    # environment), so it starts with the COMSPEC entry, not two zero bytes.
    assert mem[env_off] != 0
    assert bytes(mem[env_off:env_off + 8]) == b"COMSPEC="
