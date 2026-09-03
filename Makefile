# Macast developer entry points.
#
# Targets:
#   make help        show this list
#   make run         run the GUI from the current Python (no build needed)
#   make cli         run the headless CLI
#   make build-arm   build dist/Macast.app for Apple Silicon
#   make clean       remove build artefacts (dist/, build/, .venv-build/)
#   make deep-clean  also remove caches and pyc files
#
.PHONY: help run cli build-arm clean deep-clean

PYTHON ?= python3
VENV   ?= .venv-build

help:
	@echo "Macast targets:"
	@echo "  make run         launch the GUI from the current Python"
	@echo "  make cli         launch the headless CLI"
	@echo "  make build-arm   build dist/Macast.app for Apple Silicon macOS"
	@echo "  make clean       remove dist/, build/, .venv-build/"
	@echo "  make deep-clean  also remove caches and pyc files"

run:
	$(PYTHON) Macast.py

cli:
	$(PYTHON) -c "from macast.macast import cli; cli()"

build-arm:
	@if [ "$(shell uname -s)" != "Darwin" ]; then \
		echo "ERROR: build-arm only runs on macOS." >&2; exit 1; \
	fi
	bash scripts/build_macos_arm.sh

clean:
	rm -rf dist build

deep-clean: clean
	rm -rf $(VENV)
	find . -type d -name __pycache__ -prune -exec rm -rf {} +
	find . -type f -name '*.pyc' -delete
