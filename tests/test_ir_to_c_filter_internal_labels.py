import json
import subprocess
import sys
from pathlib import Path

# flake8: noqa


def test_ir_to_c_filters_internal_extern_labels(tmp_path):
    repo_root = Path(__file__).resolve().parents[1]
    ir = {
        "functions": [
            {
                "start": 0x0100,
                "instructions": [
                    {"address": 0x0100, "mnemonic": "mov", "op_str": "al, 1", "bytes": "B001"},
                    {"address": 0x0102, "mnemonic": "jmp", "op_str": "0x200", "bytes": "E9FB00"},
                    {"address": 0x0200, "mnemonic": "ret", "op_str": "", "bytes": "C3"},
                ],
            }
        ],
        "extern_labels": [0x0200],
    }
    ir_path = tmp_path / "program.ir.json"
    ir_path.write_text(json.dumps(ir))
    exe_path = tmp_path / "program.bin"
    exe_path.write_bytes(b"\x00" * 8)
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
    assert "case 0x0200:" in src
