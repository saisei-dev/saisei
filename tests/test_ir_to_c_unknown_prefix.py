import sys
from pathlib import Path

import pytest

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import (  # noqa: E402
    CCodeRenderer,
    UnsupportedInstructionError,
)


def test_unknown_instruction_prefix_aborts_translation() -> None:
    # The translator used to emit a silent ``// TODO ASM: ...`` comment
    # for mnemonics it did not understand (here a ``lock``-prefixed inc).
    # That let partial/broken C through, so it now hard-aborts instead.
    func = {
        "start": 0x0000,
        "instructions": [
            {
                "address": 0x0000,
                "mnemonic": "lock inc",
                "op_str": "ax",
                "bytes": "",
            },
            {"address": 0x0001, "mnemonic": "ret", "op_str": "", "bytes": ""},
        ],
    }

    with pytest.raises(UnsupportedInstructionError) as exc:
        CCodeRenderer().render_function(func, {0x0000})

    assert "lock inc ax" in str(exc.value)
