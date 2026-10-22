# Builds quen's HUD line (hud\quen\QuenHud.as) into modQuen's bundle: w3\out\hud\content\blob0.bundle and
# metadata.store, for pack.py. Prints the sha256 of both. The same sources give the same bytes.
#   1. compc: the game's own module classes, from REDkit's gui_new\actionscript read in place, into a library that
#      stays external (w3\out\hud\game.swc), with hud\extern's four declarations of Adobe's easing classes, which
#      HudModuleBase imports and neither Flex nor REDkit has.
#   2. mxmlc: QuenHud against it. The movie holds QuenHud only (checked by mxmlc's link report); its build date
#      (Metadata, ProductInfo) is dropped.
#   3. REDkit's GFx exporter, with the arguments wcc_lite's import passes it, then the cooked CSwfResource the game's
#      bundles hold, written here (cooked()). wcc_lite's own import and cook are not used: this REDkit saves an
#      import into its own bin folder and cooks only files in its own depot.
#   4. wcc_lite pack and metadatastore, each with -agreetoterms and its log in w3\out\hud. wcc_lite rewrites four
#      settings files in Documents\The Witcher 3 on every run with the same bytes: their times are put back; one whose
#      bytes changed meanwhile (a running game) is left as it is.
# Needs JAVA_HOME (8 or later; 21 works), FLEX_HOME (Apache Flex SDK 4.16.1, with playerglobal 32.0 in
# frameworks\libs\player\32.0) and REDKIT (The Witcher 3 REDkit's folder). Usage: python -I w3\hud.py
import ctypes, hashlib, os, re, struct, subprocess, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "out", "hud")
GFX = os.path.join(OUT, "gfx")
COOKED = os.path.join(OUT, "cooked")
CONTENT = os.path.join(OUT, "content")
# The movie's depot path: the HUD loads modules from swf\hud\ by file name (Hud.as m_swfPath), the .swf read as .redswf.
REDSWF = r"gameplay\gui_new\swf\hud\hud_quen.redswf"
PREFIX = "hud-quen"  # the GFx exporter's name for the movie's exported images (none); the linkage name is it + .gfx
# What wcc_lite rewrites on every run (w3\README.md, Offline script compile).
SETTINGS = ["editoruser.settings", "editoruser.settings.bak", "profile.settings", "profile.settings.bak"]
CREATE_NO_WINDOW = 0x08000000


def cooked(gfx, linkage):
    """The cooked CSwfResource as the game's bundles hold it (startup.bundle's hud_watermark.redswf rebuilt this way
    byte for byte, 2026-10-22): a CR2W 164 header, string, name, property and export tables, then the one export: its
    linkage name, the GFx movie and no exported textures."""
    assert len(linkage) < 64
    names = ["", "CSwfResource", "linkageName", "String"]
    strings = b"".join(n.encode() + b"\0" for n in names)
    table, o = b"", 0
    for n in names:
        h = 0x811C9DC5  # FNV-1a 32 over the name and its terminating zero; the empty name's is 0
        for c in n.encode() + b"\0":
            h = (h ^ c) * 0x01000193 & 0xFFFFFFFF
        table += struct.pack("<II", o, h if n else 0)
        o += len(n) + 1
    props = bytes(16)
    data = (b"\0" + struct.pack("<HHI", 2, 3, 5 + len(linkage)) + bytes([0x80 | len(linkage)]) + linkage.encode()
            + b"\0\0" + struct.pack("<I", len(gfx)) + gfx + bytes(4))
    at = 160 + len(strings) + len(table) + len(props) + 24
    export = struct.pack("<HHIIIII", 1, 0x2000, 0, len(data), at, 0, zlib.crc32(data))
    head = struct.pack("<4sIIQIIIII", b"CR2W", 164, 0, 0, 0, at + len(data), at + len(data), 0xDEADBEEF, 6)
    off = 160
    for i, raw in enumerate((strings, table, b"", props, export)):
        count = (len(raw), len(names), 0, 1, 1)[i]
        head += struct.pack("<III", off if raw else 0, count, zlib.crc32(raw) if raw else 0)
        off += len(raw)
    head += bytes(160 - len(head))
    head = head[:32] + struct.pack("<I", zlib.crc32(head)) + head[36:]
    return head + strings + table + props + export + data


def need(var, rel):
    root = os.environ.get(var)
    if not root or not os.path.exists(os.path.join(root, rel)):
        raise SystemExit(f"hud.py: {var} must name a folder holding {rel}")
    return root


def run(args, cwd=None, env=None):
    # No console window (AGENTS.md); the output comes back here, whole on a failure.
    r = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, errors="replace",
                       creationflags=CREATE_NO_WINDOW)
    if r.returncode != 0:
        raise SystemExit(f"hud.py: {os.path.basename(args[0])} {args[1:3]} failed ({r.returncode}):\n{r.stdout}{r.stderr}")
    return r.stdout


def flex(tool, *args):
    env = dict(os.environ, PLAYERGLOBAL_HOME=os.path.join(FLEX, "frameworks", "libs", "player"))
    return run([JAVA, "-Xmx1g", "-Dsun.io.useCanonCaches=false", "-Djava.util.Arrays.useLegacyMergeSort=true", "-jar",
                os.path.join(FLEX, "lib", f"{tool}.jar"), f"+flexlib={os.path.join(FLEX, 'frameworks')}",
                "-target-player=32.0", "-swf-version=15", *args], env=env)


def tags(movie):
    """The movie's tags after its header: (code, the tag's bytes) each."""
    p = (5 + 4 * (movie[0] >> 3) + 7) // 8 + 4  # the stage RECT, frame rate and count
    while p < len(movie):
        code, n = movie[p] >> 6 | movie[p + 1] << 2, movie[p] & 0x3F
        head = 6 if n == 0x3F else 2
        if n == 0x3F:
            n = struct.unpack_from("<I", movie, p + 2)[0]
        yield code, movie[p:p + head + n]
        p += head + n


def undated(path):
    """The movie without mxmlc's Metadata (77) and ProductInfo (41) tags, which hold the build's date, and without
    FileAttributes' metadata bit."""
    b = open(path, "rb").read()
    body = zlib.decompress(b[8:])
    out = bytearray(body[:(5 + 4 * (body[0] >> 3) + 7) // 8 + 4])
    for code, tag in tags(body):
        if code == 69:
            tag = tag[:2] + bytes([tag[2] & ~0x10]) + tag[3:]
        if code not in (41, 77):
            out += tag
    open(path, "wb").write(b"CWS" + b[3:4] + struct.pack("<I", 8 + len(out)) + zlib.compress(bytes(out), 9))


def documents():
    buf = ctypes.create_unicode_buffer(260)
    ctypes.windll.shell32.SHGetFolderPathW(None, 5, None, 0, buf)  # CSIDL_PERSONAL
    return os.path.join(buf.value, "The Witcher 3")


def wcc(*args):
    # Its log defaults to REDkit's bin\wcc.log. Its -dir and -outdir can't be quoted (the build folder's check below).
    run([os.path.join(REDKIT, r"bin\x64_RedKit\wcc_lite.exe"), *args, "-agreetoterms",
         f"-wcclog={os.path.join(OUT, 'wcc.log')}"], cwd=os.path.join(REDKIT, r"bin\x64_RedKit"))


JAVA = os.path.join(need("JAVA_HOME", r"bin\java.exe"), "bin", "java.exe")
FLEX = need("FLEX_HOME", r"frameworks\libs\player\32.0\playerglobal.swc")
REDKIT = need("REDKIT", r"bin\x64_RedKit\wcc_lite.exe")
if " " in OUT:
    raise SystemExit(f"hud.py: wcc_lite takes no space in its folders: {OUT}")
for d in (OUT, GFX, os.path.dirname(os.path.join(COOKED, REDSWF)), CONTENT):
    os.makedirs(d, exist_ok=True)
stray = [os.path.join(r, f) for r, _, fs in os.walk(COOKED) for f in fs if os.path.join(r, f) != os.path.join(COOKED, REDSWF)]
if stray:
    raise SystemExit(f"hud.py: {COOKED} holds files besides the movie, which pack would bundle: {stray}")

swc = os.path.join(OUT, "game.swc")
swf = os.path.join(OUT, "hud_quen.swf")
report = os.path.join(OUT, "link-report.xml")
flex("compc", f"-source-path+={os.path.join(REDKIT, r'r4data\gameplay\gui_new\actionscript')}",
     f"-source-path+={os.path.join(HERE, 'hud', 'extern')}",
     "-include-classes=red.game.witcher3.hud.modules.HudModuleBase,red.core.events.GameEvent", f"-output={swc}")
flex("mxmlc", f"-external-library-path+={swc}", f"-source-path+={os.path.join(HERE, 'hud')}", "-debug=false",
     "-optimize=true", "-use-network=false", "-default-size=600,70", "-default-frame-rate=24",
     "-static-link-runtime-shared-libraries=true", f"-link-report={report}", f"-output={swf}",
     os.path.join(HERE, "hud", "quen", "QuenHud.as"))
linked = re.findall(r'<script name="([^"]+)"', open(report, encoding="utf-8").read())
if len(linked) != 1 or not linked[0].replace("\\", "/").endswith("quen/QuenHud.as"):
    raise SystemExit(f"hud.py: the movie links more than QuenHud: {linked}")
undated(swf)

# The exporter compresses a whole 4 KB buffer: past the movie's own length come leftovers of its memory, which differ
# from run to run. Cut to the length the header gives, and compressed again.
run([os.path.join(REDKIT, r"bin\tools\GFx4\gfxexport_mult4fix.exe"), swf, "-c", "-i", "DDS", "-d1c", "-d5", "-quick",
     "-pack", "-ptresize", "mult4", "-share_images", "-ne", "-list", "-lwr", "-d", GFX + "\\", "-o", GFX + "\\", "-p",
     PREFIX], cwd=GFX)
gfx = open(os.path.join(GFX, "hud_quen.gfx"), "rb").read()
if gfx[:3] != b"CFX":
    raise SystemExit("hud.py: the GFx exporter wrote no compressed GFx movie")
gfx = gfx[:8] + zlib.compress(zlib.decompress(gfx[8:])[:struct.unpack_from("<I", gfx, 4)[0] - 8], 9)
open(os.path.join(COOKED, REDSWF), "wb").write(cooked(gfx, PREFIX + ".gfx"))

docs = documents()
saved = {f: open(os.path.join(docs, f), "rb").read() for f in SETTINGS if os.path.isfile(os.path.join(docs, f))}
times = {f: os.stat(os.path.join(docs, f)) for f in saved}
try:
    wcc("pack", f"-dir={COOKED}\\", f"-outdir={CONTENT}\\")
    wcc("metadatastore", f"-path={CONTENT}\\")
finally:
    for f, data in saved.items():
        path = os.path.join(docs, f)
        if open(path, "rb").read() == data:
            os.utime(path, ns=(times[f].st_atime_ns, times[f].st_mtime_ns))
        else:
            print(f"hud.py: {path} changed during the build: left as it is")

sha = lambda p: hashlib.sha256(open(p, "rb").read()).hexdigest()
for name in ("blob0.bundle", "metadata.store"):
    print(sha(os.path.join(CONTENT, name)), f"Mods/modQuen/content/{name}")
