# Third-party code in quen's Witcher 3 package

Compiled into `W3Quen.asi` (`asi\build.bat`). Each folder keeps its licence file.

| Folder | What | Version | Licence |
|---|---|---|---|
| `tracy\public` | the Tracy Profiler client (`TracyClient.cpp` and the headers it includes) | 0.14.1 (`public\common\TracyVersion.hpp`; protocol version 82, `public\common\TracyProtocol.hpp`) | 3-clause BSD, `tracy\LICENSE` (not in git: below) |
| `detours` | Microsoft Detours: `detours.cpp`, `disasm.cpp`, `modules.cpp`, `detours.h`, `detver.h` from its `src` folder | 4.0.1 (`DETOURS_VERSION 0x4c0c1`), commit 9764cebcb1a75940e68fa83d6730ffaf0f669401 of github.com/microsoft/Detours | MIT, `detours\LICENSE.md` |

- **Tracy:** the `public` folder of the Tracy 0.14.1 source archive `tracy-0.14.1.zip` (sha256 `908f3a2917fa86a247abfcf85dcf04bad1db6986a4d40f94b70512f3e9e98d5b`), unchanged, and its `LICENSE`.
- **Two of those files are not in git:** an author's email in each copyright line trips the repo's privacy scan, so they are copied in from that archive (`.gitignore` lists them). `asi\build.bat` stops without them, and `pack.py` checks their sha256:
  | File | From the archive's | sha256 |
  |---|---|---|
  | `tracy\LICENSE` | `tracy-0.14.1\LICENSE` | `27ab6b51e57f7e263a6e953d9c8984cd26e69d2f7606bde81e4b3048acf30732` |
  | `tracy\public\client\tracy_SPSCQueue.h` | `tracy-0.14.1\public\client\tracy_SPSCQueue.h` | `8cb74f10b233693d83da294dc48aa8ba8bb2a860a1d4dafb19dfbe3a76620a70` |
- **Other code inside Tracy's client,** each under its own licence in its header: moodycamel::ConcurrentQueue (simplified BSD, `client\tracy_concurrentqueue.h`), SPSCQueue (MIT, `client\tracy_SPSCQueue.h`), LZ4 (2-clause BSD, `common\tracy_lz4.cpp`), rpmalloc (public domain, `client\tracy_rpmalloc.cpp`). `pack.py` puts every notice into the release's `W3Quen-licences.txt`. It is the client the W3Trace ASI was built with. Tracy's capture tool must be the same version: `tracy-capture` 0.14.1 prints `tracy-capture 0.14.1 / 30997d5` when run without arguments, and quen's doctor checks that line.
- **Detours:** the five files unchanged, from the Detours checkout whose `lib.X64\detours.lib` the W3Trace ASI's build linked.
