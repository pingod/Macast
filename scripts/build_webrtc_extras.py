"""Build the optional WebRTC bundle for the machine running this script.

Nothing in a default artefact carries the two packages the WebRTC mirror target
needs (see `.github/workflows/build.yml`): each of the four platform jobs runs
this script, which installs them into a throwaway tree with `pip --target`,
hashes every file that landed, and zips the tree under the ABI-named release
asset the app asks for. The user's machine downloads that asset later, from the
settings page, and `screen_mirror.install_webrtc_extras()` verifies it file by
file before anything goes on `sys.path`.

Two rules this file exists to obey:

* **The names it installs and the shape of the manifest belong to the code that
  reads them.** `plugin_repo.EXTRAS_PACKAGES` says which packages, and
  `screen_mirror.EXTRAS_MANIFEST_FIELDS` / `EXTRAS_FILE_FIELDS` /
  `extras_manifest()` say what a bundle looks like -- the entries are built by
  position against the second tuple, and the manifest the first one produces is
  re-read against it before anything is zipped. Nothing here re-lists either, so
  adding a field is a one-file change on the plugin side -- and a builder that
  invents its own key produces a bundle *every* machine refuses, on four
  platforms, at the same moment. Part 56/D has a case that reads this file's
  string constants to prove it.
* **One ABI per run.** The tree is built by the interpreter that will load it,
  so there is no `--platform` / `--python-version` cross-compilation to get
  wrong; a job that wants a different bundle runs this script on that platform.
  The asset name comes from the same ABI the installer computes, so a macOS
  arm64 bundle cannot be served to an Intel Mac without `check_manifest` naming
  both halves of the mismatch.

Usage (from the repository root, on the platform being built for)::

    python scripts/build_webrtc_extras.py --version 0.20.0

Writes `Macast-WebRTC-extras-<os>-<arch>-<python>-v<version>.zip` in the current
directory and prints the file count and byte total. Exits non-zero if pip fails,
if the tree comes out empty, or if the zip does not pass the installer's own
verification.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PLUGIN = os.path.join(REPO, 'macast', 'plugins', 'renderer', 'screen_mirror.py')
# The plugin imports `macast`, so the checkout has to be on the path the way
# `scripts/run-from-source.sh` puts it there.
sys.path.insert(0, REPO)


def load_plugin():
    """The plugin module, by file -- the same loader the probes use.

    Going through `MacastPluginManager` would make a build script boot the
    plugin system, its settings, and its logging; the file is a plain module and
    this is a plain read of it. `screen_mirror` imports `macast.plugin_repo` at
    module scope, so one load gives the builder both tables it needs.
    """
    spec = importlib.util.spec_from_file_location('screen_mirror_extras_build',
                                                  PLUGIN)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def pip_install(module, target_dir):
    """Install the declared packages, and only them, into an empty tree.

    The transitive chain (the SRTP binding, the crypto backend, the CFFI
    runtime) is pip's business, not this script's: naming those here would be a
    second list to keep in sync with a wheel's metadata, and the half that is
    ours to name is already owned by `EXTRAS_PACKAGES`.
    """
    command = [sys.executable, '-m', 'pip', 'install',
               '--disable-pip-version-check', '--no-input', '--target',
               target_dir]
    command.extend(module.plugin_repo.EXTRAS_PACKAGES)
    print('  $ ' + ' '.join(command))
    subprocess.run(command, check=True)


def walk_tree(root):
    """[(relative posix path, absolute path)] for every file under `root`.

    Relative paths are `/`-joined because the manifest is read on Windows too,
    and its reader refuses a backslash -- the separator that would otherwise
    appear in every entry a macOS or Linux build wrote for it.
    """
    found = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        for name in sorted(filenames):
            absolute = os.path.join(dirpath, name)
            relative = os.path.relpath(absolute, root).replace(os.sep, '/')
            found.append((relative, absolute))
    return found


def build_entries(module, files):
    """One manifest entry per file, keys taken from the installer's own table.

    Built by position against `EXTRAS_FILE_FIELDS` rather than by spelling the
    keys: this is the single place the two sides could disagree, and the
    disagreement is invisible until a download is refused for a field name
    nobody wrote.
    """
    path_field, digest_field, size_field = module.EXTRAS_FILE_FIELDS
    entries = []
    for relative, absolute in files:
        with open(absolute, 'rb') as handle:
            data = handle.read()
        entries.append({path_field: relative,
                        digest_field: hashlib.sha256(data).hexdigest(),
                        size_field: len(data)})
    return entries


def write_zip(module, zip_path, entries, absolute_for, manifest):
    """The listed files plus the manifest at the root -- and nothing else.

    No directory entries: the installer compares the zip's member list against
    the manifest in both directions, so a `foo/` entry no file asked for is a
    refused bundle.
    """
    path_field = module.EXTRAS_FILE_FIELDS[0]
    with zipfile.ZipFile(zip_path, 'w', zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(module.EXTRAS_MANIFEST_NAME,
                         json.dumps(manifest).encode('utf-8'))
        for entry in entries:
            archive.write(absolute_for[entry[path_field]],
                          arcname=entry[path_field])


def main():
    parser = argparse.ArgumentParser(
        description='Zip the optional WebRTC packages for this machine\'s ABI.')
    parser.add_argument('--version', required=True,
                        help='the Macast version this asset belongs to')
    args = parser.parse_args()

    module = load_plugin()
    abi = module.extras_abi()
    name = module.extras_asset_name(abi, args.version)
    if not name:
        sys.stderr.write('no bundle is published for this machine: %r\n'
                         % (abi,))
        return 1
    print('building %s' % name)

    target_dir = os.path.abspath(os.path.join(os.getcwd(),
                                              'webrtc-extras-tree'))
    if os.path.exists(target_dir):
        sys.stderr.write('the tree directory is already there: %s\n'
                         % target_dir)
        return 1
    zip_path = os.path.join(os.getcwd(), name)
    os.makedirs(target_dir)
    try:
        pip_install(module, target_dir)
        files = walk_tree(target_dir)
        if not files:
            sys.stderr.write('pip finished but the tree is empty. An empty '
                             'manifest is refused at the other end by design, '
                             'so nothing worth shipping was built.\n')
            return 1
        entries = build_entries(module, files)
        manifest = module.extras_manifest(abi, args.version, entries)
        # The manifest's key set is read back against the installer's table
        # before a single byte is zipped. `extras_manifest()` is its only
        # writer, so this fires only when that function and the table it
        # describes have drifted apart -- which is the one mistake that would
        # ship four unusable bundles instead of one.
        expected = set(module.EXTRAS_MANIFEST_FIELDS)
        if set(manifest) != expected:
            sys.stderr.write('the manifest this build writes is not the shape '
                             'the installer reads: got %s, expected %s\n'
                             % (sorted(manifest), sorted(expected)))
            return 1
        write_zip(module, zip_path, entries, dict(files), manifest)
        # The installer's own reader, on the bytes we just wrote. A bundle that
        # fails here fails on one platform in front of a developer instead of on
        # four of them in front of users.
        verified, refusal = module._verify_extras_zip(zip_path, abi,
                                                      args.version)
        if verified is None:
            sys.stderr.write('the zip we built does not pass the installer\'s '
                             'check: %s\n' % refusal)
            return 1
        size_field = module.EXTRAS_FILE_FIELDS[2]
        print('wrote %s: %d files, %d bytes'
              % (zip_path, len(entries), sum(e[size_field] for e in entries)))
    finally:
        shutil.rmtree(target_dir, ignore_errors=True)
    return 0


if __name__ == '__main__':
    sys.exit(main())
