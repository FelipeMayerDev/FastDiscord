# Bundled Windows DJ tools

The portable Windows package includes these helpers under `tools/`. Downloads
happen only when building the package, with SHA-256 verification. FastDiscord
uses explicit paths relative to its executable, without requiring global PATH
changes or downloading helpers while playing music. Source/Linux builds still
use system-installed tools. Deno runs yt-dlp's bundled JavaScript components for
YouTube; remote component downloads and yt-dlp auto-updates are disabled.

* **yt-dlp 2026.08.19**: official [release](https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19).
  The standalone Windows executable includes Python and third-party components
  and is GPLv3+, although yt-dlp itself is Unlicense. Its original `LICENSE` and
  `THIRD_PARTY_LICENSES.txt` are included under `licenses/`. Corresponding
  [source release](https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/yt-dlp.tar.gz)
  and [build instructions](https://github.com/yt-dlp/yt-dlp/blob/2026.08.19/README.md#compile);
  component sources and the maintainer's source-request address are recorded
  in the included third-party notices.
* **FFmpeg n9.0.2-17-g2a571b6068**, BtbN `win64-lgpl-shared-9.0`:
  [exact binary release](https://github.com/BtbN/FFmpeg-Builds/releases/tag/autobuild-2026-09-30-13-08),
  [FFmpeg source commit](https://github.com/FFmpeg/FFmpeg/tree/2a571b6068), and
  [build recipes and dependency sources](https://github.com/BtbN/FFmpeg-Builds/tree/autobuild-2026-09-30-13-08).
  The original LGPLv3 `LICENSE.txt` is included as `licenses/ffmpeg-LICENSE.txt`.
  The FFmpeg executables and DLLs remain separate and replaceable. To rebuild,
  follow the pinned build repository's prerequisites, then run
  `./makeimage.sh win64 lgpl-shared 9.0` and
  `./build.sh win64 lgpl-shared 9.0`; use the source commit above when reproducing
  this version. Build configuration is also available with `tools/ffmpeg.exe
  -buildconf`. This monthly binary release is retained upstream for two years;
  refresh the URL and SHA-256 pin before that retention period ends.
* **Deno 2.9.7**: official [release](https://github.com/denoland/deno/releases/tag/v2.9.7),
  [source](https://github.com/denoland/deno/tree/v2.9.7), and MIT license, included
  as `licenses/deno-LICENSE.md`.

For redistribution, preserve these notices and provide the corresponding
third-party sources under their respective license terms. These helper licenses
do not replace FastDiscord's own license. No bundled helper is modified.
