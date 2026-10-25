# Licences of the LGPL libraries in this build

Corvus GCS runs its desktop window on Qt 6 through PySide6 (Qt for Python).
Both are used under the GNU Lesser General Public License, version 3:

- Qt 6, including Qt WebEngine: <https://www.qt.io/>
- PySide6 and Shiboken6: <https://pyside.org>

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
- paramiko: <https://github.com/paramiko/paramiko>

MAVLink is corvus_link, Corvus' own module written in Rust (rust/corvus_link
in the source). On Windows it links serialport-rs, which is under the Mozilla
Public License 2.0, unmodified. Its source and licence are at
<https://github.com/serialport/serialport-rs> and <https://mozilla.org/MPL/2.0/>.
The other Rust libraries it links are under MIT or Apache 2.0 and are listed
in Settings > About > Credits.

Qt WebEngine contains Chromium and the third party components Chromium
uses, each under its own licence. The full list is part of Chromium's own
credits and is linked from Settings > About > Credits.
