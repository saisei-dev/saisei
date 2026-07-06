import sys
import json
import subprocess


def test_disassemble_retf(tmp_path):
    bin_path = tmp_path / "retf.bin"
    bin_path.write_bytes(b"\xCB")
    outdir = tmp_path / "disasm"
    subprocess.run(
        [
            sys.executable,
            "compiler/disassemble.py",
            str(bin_path),
            "--outdir",
            str(outdir),
        ],
        check=True,
    )
    insns = json.loads((outdir / "disasm.json").read_text())
    assert any(insn["mnemonic"] == "retf" for insn in insns)
