import subprocess
import textwrap
from pathlib import Path


def test_parity8_matches_x86_even_parity(tmp_path):
    repo_root = Path(__file__).resolve().parents[1]
    source = tmp_path / "parity8_check.c"
    binary = tmp_path / "parity8_check"
    source.write_text(
        textwrap.dedent(
            """
            #include <stdint.h>
            #include "shims.h"

            int main(void) {
              /* PF should be 1 for even popcount in the low byte. */
              if (parity8(0x00) != 1) return 1;
              if (parity8(0x01) != 0) return 2;
              if (parity8(0x03) != 1) return 3;
              if (parity8(0xFF) != 1) return 4;
              return 0;
            }
            """
        )
    )

    subprocess.check_call(
        ["gcc", "-Iruntime/include", str(source), "-o", str(binary)],
        cwd=repo_root,
    )
    subprocess.check_call([str(binary)], cwd=repo_root)
