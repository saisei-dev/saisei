from pathlib import Path

from tools import game


def _dummy_game() -> game.GameDefinition:
    return game.GameDefinition(
        name="cat",
        key="cat",
        config_path=Path("games/cat/cat.json"),
        runtime=[],
        program_path="CAT.EXE",
        program="main",
        program_key="main",
    )


def test_run_accepts_speedup_after_game(monkeypatch):
    captured: dict[str, float | bool] = {}

    monkeypatch.setattr(game, "load_game_definition", lambda _, program=None: _dummy_game())

    def fake_run_game(*_args, **kwargs):
        captured.update(kwargs)

    monkeypatch.setattr(game, "run_game", fake_run_game)
    monkeypatch.setattr(
        "sys.argv",
        ["game.py", "run", "cat", "--headless", "--speedup", "4"],
    )

    game.main()

    assert captured["headless"] is True
    assert captured["speedup"] == 4.0


def test_run_rejects_non_positive_speedup(monkeypatch):
    monkeypatch.setattr(game, "load_game_definition", lambda _, program=None: _dummy_game())
    monkeypatch.setattr("sys.argv", ["game.py", "run", "cat", "--speedup", "0"])

    try:
        game.main()
    except SystemExit as exc:
        assert str(exc) == "--speedup must be a positive number"
    else:
        raise AssertionError("Expected SystemExit for invalid speedup")
