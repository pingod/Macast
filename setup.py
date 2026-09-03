"""
Macast — a DLNA Media Renderer using mpv.

This setup.py is consumed by `pip install macast` and by older `python setup.py
install` flows. py2app builds live in scripts/setup_py2app.py.

For building a macOS .app, see BUILDING.md or run:
    bash scripts/build_macos_arm.sh
"""

import os
import sys
from setuptools import setup, find_packages

VERSION = "0.0.0"
with open(os.path.join('macast', '.version'), 'r') as f:
    VERSION = f.read().strip()

LONG_DESCRIPTION = ""
with open('README.md', 'r', encoding='utf-8') as f:
    LONG_DESCRIPTION = f.read()

INSTALL = ["requests", "appdirs", "cherrypy", "lxml", "netifaces"]
PACKAGES = find_packages()

# Per-platform deps. The Linux list pulls the original author's git forks
# of pystray/pyperclip because the upstream releases don't yet ship the
# Linux fixes; on macOS / Windows the upstream wheels are fine.
if sys.platform == 'darwin':
    INSTALL += ["rumps", "pyperclip"]
elif sys.platform == 'win32':
    INSTALL += ["pillow", "pyperclip", "pystray"]
else:
    INSTALL += [
        "pillow",
        "pystray @ git+https://github.com/xfangfang/pystray.git",
        "pyperclip @ git+https://github.com/xfangfang/pyperclip.git",
    ]

setup(
    name="macast",
    version=VERSION,
    author="xfangfang",
    author_email="xfangfang@126.com",
    description="a DLNA Media Renderer",
    license="GPL3",
    url="https://github.com/xfangfang/Macast",
    long_description=LONG_DESCRIPTION,
    long_description_content_type="text/markdown",
    classifiers=[
        "Topic :: Multimedia :: Sound/Audio",
        "Topic :: Multimedia :: Video",
        'Programming Language :: Python :: 3',
        'Programming Language :: Python :: 3.10',
        'Programming Language :: Python :: 3.11',
        'Programming Language :: Python :: 3.12',
        "Operating System :: MacOS :: MacOS X",
        "Operating System :: Microsoft :: Windows :: Windows NT/2000",
        "Operating System :: POSIX",
    ],
    platforms=["MacOS X", "Windows", "POSIX"],
    keywords=["mpv", "dlna", "renderer"],
    install_requires=INSTALL,
    packages=PACKAGES,
    include_package_data=True,
    entry_points={
        'console_scripts': [
            'macast-cli = macast.macast:cli',
            'macast-gui = macast.macast:gui'
        ]
    },
    # 3.6 / 3.7 are EOL and rumps/py2app no longer build wheels for them.
    # 3.8 was the original minimum, 3.10 is the practical floor today.
    python_requires=">=3.10",
)
