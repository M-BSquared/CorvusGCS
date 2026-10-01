# Licences of the LGPL libraries in this build

Corvus GCS runs its desktop window on Qt 6 through PySide6 (Qt for Python).
Both are used under the GNU Lesser General Public License, version 3:

- Qt 6, including Qt WebEngine: <https://www.qt.io/>
- PySide6 and Shiboken6: <https://pyside.org>

pymavlink, the MAVLink library, is under the same LGPL v3:
<https://github.com/ArduPilot/pymavlink>

The LGPL v3 is a set of additional permissions on top of the GNU General
Public License, version 3, so both texts are here:

- [`LGPL-3.0.txt`](LGPL-3.0.txt)
- [`GPL-3.0.txt`](GPL-3.0.txt)

paramiko, the SSH library, is under the GNU Lesser General Public License,
version 2.1, and so is the FFmpeg build inside Qt WebEngine. That text is here
too, as paramiko ships it:

- [`LGPL-2.1.txt`](LGPL-2.1.txt)

They ship as separate shared libraries or Python packages next to the
application, unmodified, and can be replaced with builds of your own. Their
source code is published by their authors:

- Qt: <https://download.qt.io/official_releases/qt/> and <https://code.qt.io/>
- PySide6 and Shiboken6: <https://code.qt.io/cgit/pyside/pyside-setup.git/>
- pymavlink: <https://github.com/ArduPilot/pymavlink>
- paramiko: <https://github.com/paramiko/paramiko>

Qt WebEngine contains Chromium and the third party components Chromium
uses, each under its own licence. The full list is part of Chromium's own
credits and is linked from Settings > About > Credits.
