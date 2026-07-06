#!/usr/bin/env python3
import os
import pty
import sys


def main() -> None:
    if len(sys.argv) < 2:
        print("Usage: run_with_pty.py CMD [ARGS...]", file=sys.stderr)
        sys.exit(1)

    cmd = " ".join(sys.argv[1:])
    status = pty.spawn(["/bin/sh", "-c", cmd])
    if os.WIFEXITED(status):
        sys.exit(os.WEXITSTATUS(status))
    if os.WIFSIGNALED(status):
        sys.exit(128 + os.WTERMSIG(status))
    sys.exit(status)


if __name__ == "__main__":
    main()
