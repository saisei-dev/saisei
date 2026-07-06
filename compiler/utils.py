"""Shared utility helpers for build scripts."""
from __future__ import annotations

import re


_identifier_re = re.compile(r"[^0-9A-Za-z_]")


def sanitize_identifier(value: str) -> str:
    """Return a lowercase identifier safe for filenames and prefixes.

    The sanitisation mirrors the behaviour expected by the build pipeline and
    generated configuration files. Characters outside of ``[0-9A-Za-z_]`` are
    replaced with underscores, leading underscores are stripped and identifiers
    that would otherwise be empty or start with a digit fall back to sensible
    defaults. The result is always lowercase so case-insensitive filesystems
    behave consistently."""

    cleaned = _identifier_re.sub("_", value)
    cleaned = cleaned.lstrip("_")
    if not cleaned:
        cleaned = "bin"
    if cleaned[0].isdigit():
        cleaned = f"_{cleaned}"
    return cleaned.lower()
