import sys
from pathlib import Path

# flake8: noqa
sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import CCodeRenderer  # noqa: E402


def test_function_name_metadata():
    func = {
        "start": 0x0385,
        "instructions": [
            {
                "address": 0x0385,
                "mnemonic": "call",
                "op_str": "04EF",
                "bytes": "E80000",
            },
            {
                "address": 0x0388,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0385, 0x04EF}
    renderer = CCodeRenderer(
        func_names={0x0385: "clearPendingKeys", 0x04EF: "loadFile"},
        name_prefix="game_",
    )
    lines = renderer.render_function(func, known)
    src = "\n".join(lines)
    assert lines[0] == "// func_0385"
    assert "void game_clearPendingKeys_impl(const char *file, const char *func, int line)" in src
    # In-binary direct call routes through the in-loop dispatch: push the
    # return IP and set pc to the target (loadFile @ 0x04EF), then continue.
    assert "pc = 0x04EF;" in src
    assert "continue;" in src


def test_instruction_comment_metadata():
    func = {
        "start": 0x0385,
        "instructions": [
            {
                "address": 0x0385,
                "mnemonic": "call",
                "op_str": "04EF",
                "bytes": "E80000",
            },
            {
                "address": 0x0388,
                "mnemonic": "ret",
                "op_str": "",
                "bytes": "C3",
            },
        ],
    }
    known = {0x0385, 0x04EF}
    comments = {
        0x0385: "Call loadFile",
        0x0388: {"text": "Multi line\ncomment", "multiline": True},
    }
    renderer = CCodeRenderer(
        func_names={0x0385: "clearPendingKeys", 0x04EF: "loadFile"},
        comments=comments,
        name_prefix="game_",
    )
    lines = renderer.render_function(func, known)
    assert any(line.strip() == "// Call loadFile" for line in lines)
    block = next(i for i, line in enumerate(lines) if line.strip() == "/*")
    assert lines[block + 1].strip() == "Multi line"
    assert lines[block + 2].strip() == "comment"
    assert lines[block + 3].strip() == "*/"

