# Third-party notices

The muscli source code is licensed under the MIT license. Release installers
also contain the following separate programs. They are not relicensed under
MIT and remain governed by their own licenses.

## mpv and FFmpeg (Windows bundle)

The Windows installer includes unmodified executables from the reproducible
`mpv-winbuild-cmake` project:

- Build release: `20260903`
- mpv: `69e63f425a`, archive SHA-256
  `418dbfb5feb851cbed33d6c05d8481ba71802621bfd6efe8974522b28d42ac97`
- FFmpeg: `9fc8c785e`, archive SHA-256
  `03bc01eff87973fd757ac0ef5ead32796223b4f8435c46965e3238ab11d5b685`
- Build scripts and corresponding source:
  <https://github.com/shinchiro/mpv-winbuild-cmake/tree/cd1edc1>
- Exact binary release:
  <https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20260903>

mpv is primarily GPL-2.0-or-later; its exact license depends on enabled build
options. FFmpeg is LGPL-2.1-or-later or GPL-2.0-or-later depending on enabled
components. The upstream archives include their license information. Source
for every bundled component and the build recipe are available from the links
above.

The release process copies these tools as separate executables. muscli invokes
them as child processes and does not statically or dynamically link against
their libraries.
