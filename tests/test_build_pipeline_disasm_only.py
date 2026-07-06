from pathlib import Path
import sys

from compiler import build_pipeline


def test_disasm_only(monkeypatch, tmp_path: Path) -> None:
    calls: list[tuple[list[str], str]] = []

    def fake_run(cmd: list[str], desc: str) -> None:
        calls.append((cmd, desc))

    monkeypatch.setattr(build_pipeline, "run", fake_run)

    bin_path = tmp_path / "foo.bin"
    bin_path.write_bytes(b"\x90")

    monkeypatch.setattr(
        sys,
        "argv",
        [
            "build_pipeline.py",
            "--disasm-only",
            "--artifacts-dir",
            str(tmp_path),
            str(bin_path),
        ],
    )

    build_pipeline.main()

    assert len(calls) == 1
    assert calls[0][1] == f"disassembling {bin_path.stem}"
    assert calls[0][0][0] == sys.executable
    assert not (tmp_path / f"{bin_path.stem}.c").exists()


def test_missing_capstone_dependency_exits(
    monkeypatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(
        build_pipeline.importlib.util, "find_spec", lambda _: None
    )
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "build_pipeline.py",
            "--artifacts-dir",
            str(tmp_path),
            str(tmp_path / "foo.bin"),
        ],
    )

    try:
        build_pipeline.main()
    except SystemExit as exc:
        assert "Missing Python dependency 'capstone'" in str(exc)
    else:  # pragma: no cover
        raise AssertionError("expected SystemExit")
