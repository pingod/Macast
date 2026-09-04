# Single source of truth for the package version.
#
# Historically Macast stored the version in a dotfile (.version) at the package
# root. Both pip consumers and py2app builds used it via `open('.version')`.
# The pyproject.toml build backend (setuptools>=61) prefers a real Python
# attribute, so this tiny module is the new canonical source; `.version` is
# kept as a fallback for any third-party tooling that still greps for it.
__version__ = "0.7.5"
