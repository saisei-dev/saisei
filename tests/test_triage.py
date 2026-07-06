"""triage classifies crash bundles into the right problem class."""
import json

from tools.triage import triage, _CLASS


def _bundle(tmp_path, name, manifest=None, crash="", trace=""):
    b = tmp_path / name
    b.mkdir()
    if manifest is not None:
        (b / "manifest.json").write_text(json.dumps(manifest))
    (b / "crash.txt").write_text(crash)
    (b / "trace.tail.log").write_text(trace)
    return b


def test_stack_drift_is_operator_class(tmp_path, capsys):
    b = _bundle(tmp_path, "crash_1_1_stack_drift_0x10",
                manifest={"kind": "stack_drift", "runtime_version": "abc123",
                          "active_binary": "app", "fault_addr": "0x10"},
                crash="[FATAL] stack drift at lcall boundary\n")
    assert triage(b) == 0
    out = capsys.readouterr().out
    assert "operator" in out and "abc123" in out


def test_kind_from_dirname_without_manifest(tmp_path, capsys):
    b = _bundle(tmp_path, "crash_1_1_unhandled_pc_0xABCD",
                crash="[BUG] Unhandled pc=ABCD\n")
    assert triage(b) == 0
    out = capsys.readouterr().out
    assert "bad-transfer" in out  # unhandled_pc class


def test_missing_file_detected_from_logs(tmp_path, capsys):
    b = _bundle(tmp_path, "crash_1_1_unhandled_pc_0x0",
                crash="Error: dos_open_file: failed to open LEVELS.DAT\n")
    assert triage(b) == 0
    out = capsys.readouterr().out
    assert "missing-file" in out


def test_class_map_has_known_kinds():
    assert "stack_drift" in _CLASS and "unhandled_pc" in _CLASS
