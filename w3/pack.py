# Builds and tests W3Quen.asi (asi\build.bat test, which needs WNB_SDK and MSVC) and modQuen's HUD bundle (hud.py,
# which needs JAVA_HOME, FLEX_HOME and REDKIT), then packs quen's Witcher 3 package into w3\out\quen-w3-<VERSION>.zip,
# whose root mirrors the game folder. Prints the sha256 of each file and of the zip, for the release notes and quen's
# doctor pins. The zip is reproducible: same inputs, same bytes.
# Usage: python -I w3\pack.py
import hashlib, os, re, subprocess, sys, zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
# The release is the one W3Quen.cpp names (kVersion), so the zip and the ASI's log line never disagree.
VERSION = re.search(r'kVersion\[\] = "([0-9.]+)"', open(os.path.join(HERE, "asi", "W3Quen.cpp")).read()).group(1)
OUT = os.path.join(HERE, "out")
TRACY = os.path.join(HERE, "third_party", "tracy")
# Tracy 0.14.1's two files that aren't in git (an author's email in each trips the privacy scan): their sha256s.
UNTRACKED = {
    os.path.join(TRACY, "LICENSE"): "27ab6b51e57f7e263a6e953d9c8984cd26e69d2f7606bde81e4b3048acf30732",
    os.path.join(TRACY, "public", "client", "tracy_SPSCQueue.h"): "8cb74f10b233693d83da294dc48aa8ba8bb2a860a1d4dafb19dfbe3a76620a70",
}
# The code compiled into W3Quen.asi besides ours, each licence asking for its notice to come with the binary: the file,
# and the first and last line of the notice in it (None: the whole file). rpmalloc is in the public domain.
LICENCES = [
    ("Tracy Profiler 0.14.1 (its client)", os.path.join(TRACY, "LICENSE"), None),
    ("moodycamel::ConcurrentQueue, in Tracy's client", os.path.join(TRACY, "public", "client", "tracy_concurrentqueue.h"),
     ("// Simplified BSD license:", "EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.")),
    ("SPSCQueue, in Tracy's client", os.path.join(TRACY, "public", "client", "tracy_SPSCQueue.h"),
     ("Copyright (c) 2020 Erik Rigtorp", "SOFTWARE.")),
    ("LZ4, in Tracy's client", os.path.join(TRACY, "public", "common", "tracy_lz4.cpp"),
     ("LZ4 - Fast LZ compression algorithm", "EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.")),
    ("Microsoft Detours 4.0.1", os.path.join(HERE, "third_party", "detours", "LICENSE.md"), None),
]
FILES = {  # path in the zip (= in the game folder): source
    "bin/x64_dx12/W3Quen.asi": os.path.join(OUT, "W3Quen.asi"),
    "bin/x64_dx12/W3Quen-licences.txt": os.path.join(OUT, "W3Quen-licences.txt"),
    "Mods/modQuen/content/scripts/local/quen.ws": os.path.join(HERE, "mod", "modQuen", "content", "scripts", "local", "quen.ws"),
    # From 0.1.16: the HUD line's movie (hud.py), and the notice REDkit's licence asks of everything made with it.
    "Mods/modQuen/content/blob0.bundle": os.path.join(OUT, "hud", "content", "blob0.bundle"),
    "Mods/modQuen/content/metadata.store": os.path.join(OUT, "hud", "content", "metadata.store"),
    "Mods/modQuen/README.txt": os.path.join(HERE, "mod", "modQuen", "README.txt"),
}

sha = lambda p: hashlib.sha256(open(p, "rb").read()).hexdigest()
for path, want in UNTRACKED.items():
    if not os.path.isfile(path) or sha(path) != want:
        raise SystemExit(f"{path}: not Tracy 0.14.1's (sha256 {want}): see w3/third_party/README.md")


def notice(path, lines):
    text = open(path, encoding="utf-8-sig").read()
    if lines is None:
        return text
    body = text.splitlines()
    first = next(i for i, l in enumerate(body) if lines[0] in l)
    last = next(i for i, l in enumerate(body) if i >= first and lines[1] in l)
    return "\n".join(body[first:last + 1]) + "\n"


subprocess.run(["cmd", "/c", os.path.join(HERE, "asi", "build.bat"), "test"], check=True)
subprocess.run([sys.executable, "-I", os.path.join(HERE, "hud.py")], check=True)
with open(FILES["bin/x64_dx12/W3Quen-licences.txt"], "w", encoding="utf-8", newline="\n") as f:
    f.write("W3Quen.asi contains code of the projects below, under these licences.\n")
    for name, path, lines in LICENCES:
        f.write(f"\n---- {name} ----\n\n" + notice(path, lines))
zip_path = os.path.join(OUT, f"quen-w3-{VERSION}.zip")
# ponytail: stored, not deflated, with a fixed date: the bytes depend on the files only, not on zlib or the clock.
with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_STORED) as z:
    for name, src in FILES.items():
        info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
        info.create_system = 0
        info.external_attr = 0
        z.writestr(info, open(src, "rb").read())
for name, src in FILES.items():
    print(sha(src), name)
print(sha(zip_path), os.path.relpath(zip_path, os.path.dirname(HERE)))
