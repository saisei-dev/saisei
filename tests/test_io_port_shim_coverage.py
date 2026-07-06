from __future__ import annotations

import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


def _extract_shim_ports() -> dict[str, set[int]]:
    # IO-port handling is split across shims.c and the per-device hardware
    # layers (runtime/hw/*.c), and a device's port function serves
    # both reads and writes. So collect every port mentioned (if/||/case)
    # across all of them and treat it as handled in all directions; the
    # check below still catches ports the game touches that NOTHING handles.
    sources = [REPO_ROOT / "runtime" / "core" / "shims.c"]
    sources += sorted((REPO_ROOT / "runtime").rglob("*.c"))
    text = "\n".join(p.read_text(encoding="utf-8") for p in sources)

    handled = {
        int(m, 16) for m in re.findall(r"port == 0x([0-9A-Fa-f]+)", text)
    }
    handled |= {
        int(m, 16) for m in re.findall(r"case 0x([0-9A-Fa-f]+):", text)
    }
    # Ports a device claims on the IO bus: `const uint16_t x_ports[] = {
    # 0x388, 0x389, 0xFFFF };` -- the authoritative declaration.
    for body in re.findall(r"_ports\[\]\s*=\s*\{([^}]*)\}", text):
        for m in re.findall(r"0x([0-9A-Fa-f]+)", body):
            if int(m, 16) != 0xFFFF:
                handled.add(int(m, 16))

    return {"inb": handled, "outb": handled, "outw": handled, "inw": handled}


def _resolve_port(arg: str, lines: list[str], line_no: int) -> int | None:
    if re.fullmatch(r"0x[0-9A-Fa-f]+|\d+", arg):
        return int(arg, 0)

    if arg != "dx":
        return None

    # Local backwards scan handles the patterns used by translated artifacts,
    # such as `dx = 0x3c8` followed by `dl = 0xc9` before an in/out.
    dx_value: int | None = None
    for back in range(line_no - 1, max(-1, line_no - 24), -1):
        text = lines[back]
        assign_dx = re.search(r"\bdx\s*=\s*(0x[0-9A-Fa-f]+|\d+)\s*;", text)
        if assign_dx:
            dx_value = int(assign_dx.group(1), 0)
            continue

        assign_dl = re.search(r"\bdl\s*=\s*(0x[0-9A-Fa-f]+|\d+)\s*;", text)
        if assign_dl and dx_value is not None:
            return (dx_value & 0xFF00) | int(assign_dl.group(1), 0)

    return dx_value


def _artifact_io_calls() -> list[tuple[str, int, str, int]]:
    calls: list[tuple[str, int, str, int]] = []
    for path in sorted((REPO_ROOT / "build").glob("*.c")):
        lines = path.read_text(encoding="utf-8").splitlines()
        for idx, line in enumerate(lines):
            match = re.search(r"\b(inb|inw|outb|outw)\(([^,\)]+)", line)
            if not match:
                continue
            op, arg = match.group(1), match.group(2).strip()
            port = _resolve_port(arg, lines, idx)
            if port is not None:
                calls.append((path.name, idx + 1, op, port))
    return calls


def test_resolved_artifact_io_ports_are_implemented_in_shims() -> None:
    shim_ports = _extract_shim_ports()
    missing: list[tuple[str, int, str, int]] = []

    for filename, line_no, op, port in _artifact_io_calls():
        if port not in shim_ports[op]:
            missing.append((filename, line_no, op, port))

    assert not missing, (
        "Found translated inb/outb/inw/outw ports without shim handling: "
        + ", ".join(f"{name}:{line} {op}(0x{port:04X})" for name, line, op, port in missing)
    )
