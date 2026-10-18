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

## Native Opus decoder

Native Opus decoding uses `ruopus` 0.1.2 under the MIT license.
Upstream: <https://github.com/jmg049/ruopus>.

```text
Copyright (c) 2025- Jack Geraghty

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

## TLS certificate roots

The TLS certificate roots included through `webpki-roots` 1.0.9 are licensed
under CDLA-Permissive-2.0. The following license text accompanies the data.
Upstream: <https://github.com/rustls/webpki-roots>.

```text
# Community Data License Agreement - Permissive - Version 2.0

This is the Community Data License Agreement - Permissive, Version
2.0 (the "agreement"). Data Provider(s) and Data Recipient(s) agree
as follows:

## 1. Provision of the Data

1.1. A Data Recipient may use, modify, and share the Data made
available by Data Provider(s) under this agreement if that Data
Recipient follows the terms of this agreement.

1.2. This agreement does not impose any restriction on a Data
Recipient's use, modification, or sharing of any portions of the
Data that are in the public domain or that may be used, modified,
or shared under any other legal exception or limitation.

## 2. Conditions for Sharing Data

2.1. A Data Recipient may share Data, with or without modifications, so
long as the Data Recipient makes available the text of this agreement
with the shared Data.

## 3. No Restrictions on Results

3.1. This agreement does not impose any restriction or obligations
with respect to the use, modification, or sharing of Results.

## 4. No Warranty; Limitation of Liability

4.1. All Data Recipients receive the Data subject to the following
terms:

THE DATA IS PROVIDED ON AN "AS IS" BASIS, WITHOUT REPRESENTATIONS,
WARRANTIES OR CONDITIONS OF ANY KIND, EITHER EXPRESS OR IMPLIED
INCLUDING, WITHOUT LIMITATION, ANY WARRANTIES OR CONDITIONS OF TITLE,
NON-INFRINGEMENT, MERCHANTABILITY OR FITNESS FOR A PARTICULAR PURPOSE.

NO DATA PROVIDER SHALL HAVE ANY LIABILITY FOR ANY DIRECT, INDIRECT,
INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING
WITHOUT LIMITATION LOST PROFITS), HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE DATA OR RESULTS,
EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGES.

## 5. Definitions

5.1. "Data" means the material received by a Data Recipient under
this agreement.

5.2. "Data Provider" means any person who is the source of Data
provided under this agreement and in reliance on a Data Recipient's
agreement to its terms.

5.3. "Data Recipient" means any person who receives Data directly
or indirectly from a Data Provider and agrees to the terms of this
agreement.

5.4. "Results" means any outcome obtained by computational analysis
of Data, including for example machine learning models and models'
insights.
```
