from pathlib import Path
import sys

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.cfg import _postdominates, nearest_common_postdom  # noqa: E402


def test_postdominates_cycle():
    ipdom = {1: 2, 2: 3, 3: 1}
    assert not _postdominates(ipdom, 1, 4)


def test_nearest_common_postdom_cycle():
    ipdom = {1: 2, 2: 3, 3: 1}
    assert nearest_common_postdom(ipdom, 1, 2) == 1
