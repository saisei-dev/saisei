# Thin convenience wrapper around the JIT-only pipeline (tools/game.py).
# There is no ahead-of-time decode step: `build` emits the per-game config and
# links the runtime; the program image's entry cs:ip comes from its MZ header
# and every reached code segment is JIT-compiled on first execution.
#
# No games ship with the repo — bootstrap one with `make new-game` (see README),
# then pass its name as GAME=<name>. Inject extra compiler flags with CFLAGS=...
# (e.g. CFLAGS=-DFORCE_EXIT_AFTER_10S for a self-terminating smoke run).
PYTHON ?= python3
GAME   ?=

export PYTHONPATH := $(CURDIR)

.PHONY: build run run-silent play new-game lint hooks clean

build:
	$(PYTHON) tools/game.py build $(GAME)

run:
	$(PYTHON) tools/game.py run $(GAME) --headless

run-silent:
	$(PYTHON) tools/game.py run $(GAME) --headless --silent

# Interactive SDL window.
play:
	$(PYTHON) tools/game.py play $(GAME)

# Bootstrap a new game bundle from an archive (URL / .zip / dir).
#   make new-game ARGS="<url> --exe FOO.EXE"
new-game:
	$(PYTHON) tools/game.py new-game $(ARGS)

# Lint the whole tree with flake8 (rules in .flake8). Same check CI runs.
lint:
	$(PYTHON) -m flake8

# Enable the git pre-commit hook so flake8 runs before every commit.
hooks:
	git config core.hooksPath .githooks
	@echo "Git hooks enabled: flake8 now runs on every commit (.githooks/pre-commit)."

clean:
	rm -rf build
