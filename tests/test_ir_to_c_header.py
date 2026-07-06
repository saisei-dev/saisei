# flake8: noqa
import json
import subprocess
import sys
from pathlib import Path


def test_ir_to_c_emits_header_and_includes(tmp_path):
    repo_root = Path(__file__).resolve().parents[1]
    ir = {
        "functions": [
            {
                "start": 0x0000,
                "instructions": [
                    {
                        "address": 0x0000,
                        "mnemonic": "ret",
                        "op_str": "",
                        "bytes": "C3",
                    }
                ],
            },
            {
                "start": 0x0003,
                "instructions": [
                    {
                        "address": 0x0003,
                        "mnemonic": "ret",
                        "op_str": "",
                        "bytes": "C3",
                    }
                ],
            },
        ]
    }

    ir_path = tmp_path / "program.ir.json"
    ir_path.write_text(json.dumps(ir))
    exe_path = tmp_path / "program.bin"
    exe_path.write_bytes(b"\xC3\xC3")
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

    header_path = out_c.with_suffix(".h")
    assert header_path.exists()
    header_src = header_path.read_text()
    assert (
        "void func_0000_impl(uint16_t expected_retip, "
        "const char *file, const char *func, int line);" in header_src
    )
    assert (
        "#define func_0000(retip) func_0000_impl((retip), "
        "__FILE__, __func__, __LINE__)" in header_src
    )
    assert (
        "void func_0003_impl(uint16_t expected_retip, "
        "const char *file, const char *func, int line);" in header_src
    )
    assert (
        "#define func_0003(retip) func_0003_impl((retip), "
        "__FILE__, __func__, __LINE__)" in header_src
    )

    c_src = out_c.read_text()
    assert '#include "program.h"' in c_src
    assert "Forward declarations" not in c_src
