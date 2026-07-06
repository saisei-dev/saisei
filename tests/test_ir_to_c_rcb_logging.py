# flake8: noqa
import json
import subprocess
import sys
from pathlib import Path


def test_ir_to_c_rcb_logging(tmp_path):
    repo_root = Path(__file__).resolve().parents[1]
    ir = {
        "functions": [
            {
                "start": 0x0000,
                "instructions": [
                    {
                        "address": 0x0000,
                        "mnemonic": "mov",
                        "op_str": "word ptr es:[0xFF2C], ax",
                        "bytes": "66",
                    },
                    {
                        "address": 0x0003,
                        "mnemonic": "add",
                        "op_str": "ax, word ptr es:[0xFF2C]",
                        "bytes": "66",
                    },
                    {
                        "address": 0x0006,
                        "mnemonic": "add",
                        "op_str": "word ptr es:[0xFF2C], ax",
                        "bytes": "66",
                    },
                    {
                        "address": 0x0009,
                        "mnemonic": "mov",
                        "op_str": "ax, word ptr es:[0xFF2C]",
                        "bytes": "66",
                    },
                    {
                        "address": 0x000C,
                        "mnemonic": "ret",
                        "op_str": "",
                        "bytes": "C3",
                    },
                ],
            }
        ]
    }
    ir_path = tmp_path / "program.ir.json"
    ir_path.write_text(json.dumps(ir))
    exe_path = tmp_path / "program.bin"
    exe_path.write_bytes(b"\x00" * 2)
    out_c = tmp_path / "program.c"
    subprocess.check_call(
        [
            sys.executable,
            str(repo_root / "compiler" / "ir_to_c.py"),
            str(ir_path),
            str(exe_path),
            "--out",
            str(out_c),
        ]
    )
    src = out_c.read_text()
    assert "rcb_write16(DATA_BASE_SEG, ax);" in src
    assert "uint32_t old = ax;" in src
    assert "uint32_t src = rcb_read16(DATA_BASE_SEG);" in src
    assert "uint32_t tmp = old + src;" in src
    assert "CF = tmp > 0xFFFF;" in src
    assert "ax = tmp & 0xFFFF;" in src
    assert "OF = (~(old ^ src) & (old ^ tmp) & 0x8000) != 0;" in src
    assert "uint32_t old = rcb_read16(DATA_BASE_SEG);" in src
    assert "uint32_t src = ax;" in src
    assert "uint32_t tmp = old + src;" in src
    assert "rcb_write16(DATA_BASE_SEG, tmp & 0xFFFF);" in src
    assert "OF = (~(old ^ src) & (old ^ tmp) & 0x8000) != 0;" in src
    assert "ax = rcb_read16(DATA_BASE_SEG);" in src
