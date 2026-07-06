"""The new-game bootstrap derives the right seed config from an archive."""
from pathlib import Path

from tools.new_game import build_seed_config, is_executable

ROOT = Path(__file__).resolve().parent.parent
CAT = ROOT / "games" / "cat" / "CAT.EXE"


def test_is_executable_detects_mz_and_extension():
    assert is_executable(CAT)
    assert not is_executable(ROOT / "games" / "cat" / "cat.json")


def test_seed_config_is_jit_only():
    # JIT-only seed: just the program image + the data files to copy. No
    # binaries list, entry symbol, or call-target table -- the runtime takes the
    # entry cs:ip from the MZ header and JIT-compiles every reached segment.
    cfg = build_seed_config("cat", CAT, CAT.parent)
    assert cfg["program_path"] == "CAT.EXE"
    assert any(r["dest"] == "CAT.EXE" for r in cfg["runtime"])
    assert "binaries" not in cfg
    assert "entry_symbol" not in cfg
    assert "call_targets" not in cfg
