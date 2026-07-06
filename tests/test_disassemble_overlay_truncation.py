from pathlib import Path
import struct

from compiler.disassemble import stage1


def build_mz(module: bytes, overlay: bytes) -> bytes:
    header = bytearray(64)
    header[:2] = b"MZ"
    header_size = len(header)
    e_cblp = header_size + len(module)
    e_cp = 1
    struct.pack_into("<HHHH", header, 2, e_cblp, e_cp, 0, header_size // 16)
    return bytes(header) + module + overlay


def test_stage1_treats_bytes_beyond_module_as_overlay(tmp_path: Path) -> None:
    module = b"\x90\x90\x90"
    overlay = b"\xCC\xCC"
    data = build_mz(module, overlay)
    meta = stage1(data, tmp_path, None)
    assert meta["load_module"] == module
    overlay_seg = [s for s in meta["segment_map"] if s["name"] == "overlay"][0]
    assert overlay_seg["size"] == len(overlay)
