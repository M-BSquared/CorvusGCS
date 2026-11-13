# Licences of the third party components in this build

Corvus GCS is one native Rust program. The libraries it links are under the
MIT, Apache 2.0, ISC, BSD, Zlib and Unicode licences, which ask for the notice
and nothing else, plus the one licence below that asks for more. No library
under the GNU General Public License or Lesser General Public License ships in
this build: Qt, PySide6, paramiko, pyserial and the rest of the Python stack are
gone.

The list of every component, with its licence and what it is used for, is in the
application under Settings, About, Credits.

## Mozilla Public License 2.0

serialport-rs lists the serial ports and drives the serial link on Windows. It
is under the Mozilla Public License 2.0 and is linked unmodified. The licence
text is at <https://mozilla.org/MPL/2.0/>, and the source code of the version in
this build is published by its authors at
<https://github.com/serialport/serialport-rs>. You can build Corvus GCS against
a copy of your own.

serialport-rs depends on unescaper, which is offered under the MIT licence or
the GNU General Public License 3.0. Corvus GCS uses it under the MIT licence.

## Typefaces and icons

Inter and JetBrains Mono are under the SIL Open Font License 1.1, with the
copyright lines shown in Settings, About, Credits. Lucide is under the ISC
licence.

## Firmware and data that are not shipped

Corvus GCS downloads PX4 and ArduPilot firmware, map tiles, elevation data and
OpenStreetMap buildings only when you ask it to. None of that is part of the
program and none of it is bundled, so its licences (BSD 3-Clause, GPL v3, ODbL
and others) do not apply to the program itself.
