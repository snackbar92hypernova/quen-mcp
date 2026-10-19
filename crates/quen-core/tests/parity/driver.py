"""Parity driver for tests/parity.rs: runs synthetic cases through the Python harness's own functions
and prints one JSON result per case.

The functions come from the harness folder named by QUEN_HARNESS_DIR, as source only: the top-level statements that
define them run in a namespace of the driver's own. None of the harness's import-time work happens (its chdir, its
folders, its numpy / cv2 / Win32 imports), nothing is written there, and the game's real paths are never set: a global
the driver doesn't supply is a NameError, not a fallback to the real game.

stdin: a JSON list of cases {"kind", "dir" (an empty scratch folder of the test's), ...};
stdout: a JSON list of {"ok": result} or {"err": "Type: message"}, one per case.
"""
import ast, contextlib, csv, glob, hashlib, importlib.util, io, json, math, os, re, shutil, struct, sys, time, types, warnings
import numpy as np

HARNESS = os.environ["QUEN_HARNESS_DIR"]
if os.name == "nt": import _winapi


def load(file, names, **env):
    """The top-level defs and assignments of HARNESS/file that bind any of `names`, run in a fresh namespace."""
    path = os.path.join(HARNESS, file)
    tree = ast.parse(open(path, encoding="utf-8").read(), path)

    def binds(node):
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)): return {node.name}
        if isinstance(node, ast.Assign): return {n.id for t in node.targets for n in ast.walk(t) if isinstance(n, ast.Name)}
        if isinstance(node, ast.AugAssign) and isinstance(node.target, ast.Name): return {node.target.id}  # SC |= {...}
        return set()
    body = [n for n in tree.body if binds(n) & names]
    missing = names - set().union(*map(binds, body))
    if missing: raise SystemExit(f"{file}: no top-level {sorted(missing)}")
    ns = {"os": os, "re": re, "csv": csv, "json": json, "sys": sys, "time": time, "np": np, **env}
    exec(compile(ast.Module(body, []), path, "exec"), ns)
    return ns


# ROOT only names the parking folder, which park_names never touches.
IS = load("ini_session.py", {"apply_settings", "park_home", "park_names", "PLAIN", "BRIDGE", "bridge_flags", "bridge_loaded",
                             "perf", "PRESENTMON", "SIG_BITS"}, ROOT="%GAME_DIR%")
B404 = load("boot404.py", {"medians"})
W3 = load("w3ctl.py", {"SC", "parse_tokens", "describe", "die"})
CT = load("console_type.py", {"US", "SHIFTED", "PROMPT_ON", "PROMPT_OFF", "console_open", "tap", "main"})
CP = load("cvar_poke.py", {"TYPES", "CHUNK", "sections", "read_section", "cstr", "find", "show", "encode", "run"},
          struct=struct)
PP = load("pool_probe.py", {"STAMP"})


def write(path, text):
    with open(path, "wb") as f: f.write(text.encode())


def settings(case):
    """apply_settings on a copy; the file's bytes travel as Latin-1 text, one char per byte."""
    p = os.path.join(case["dir"], "dx12user.settings")
    with open(p, "wb") as f: f.write(case["data"].encode("latin-1"))
    IS["apply_settings"](p, case["flat"], case["sections"])
    return open(p, "rb").read().decode("latin-1")


def layout(case):
    """The synthetic Mods and bin folders of a park case (the Rust side lists the same folders). Outside the case's
    try: a link that can't be made fails the run instead of passing for a refusal."""
    for folder, entries in (("Mods", case["mods"]), ("bin", case["bin"])):
        os.mkdir(os.path.join(case["dir"], folder))
        for name, kind in entries:
            p = os.path.join(case["dir"], folder, name)
            if kind == "dir": os.mkdir(p)
            elif kind == "file": open(p, "wb").close()
            else:  # a junction, dirlink or filelink to a target outside the listing
                target = os.path.join(case["dir"], "target-" + name)
                open(target, "wb").close() if kind == "filelink" else os.mkdir(target)
                if kind == "junction" and os.name == "nt": _winapi.CreateJunction(target, p)
                else: os.symlink(target, p, target_is_directory=kind != "filelink")


def park(case):
    """park_names over the case's layout."""
    IS["MODS"], IS["GAME"] = (os.path.join(case["dir"], d) for d in ("Mods", "bin"))
    os.environ.update(W3_PARK_MODS=case["park"], W3_PARK_KEEP=case["keep"])
    os.environ.pop("W3_PARK_RENODX", None)
    return sorted(IS["park_names"]())


def bridge(case):
    """bridge_loaded is called with the `before` log on disk, its check after `after` was appended."""
    IS["GAME"] = case["dir"]
    p = os.path.join(case["dir"], "W3GameStateBridge.log")
    write(p, case["before"])
    in_game = IS["bridge_loaded"](0)
    write(p, case["before"] + case["after"])
    return {"flags": IS["bridge_flags"](0), "loaded": bool(in_game(0))}


def presentmon(case):
    """perf with PresentMon stubbed to copy the synthetic CSV, boot404's medians, and numpy's p95 (no harness function
    has one) over the rows and cells perf keeps."""
    src = os.path.join(case["dir"], "in.csv")
    write(src, case["csv"])
    IS["OUT"] = case["dir"]
    IS["subprocess"] = types.SimpleNamespace(run=lambda argv, **_: shutil.copyfile(src, argv[argv.index("--output_file") + 1]))
    IS["W"] = types.SimpleNamespace(procs=dict)
    with contextlib.redirect_stdout(io.StringIO()): perf = IS["perf"]("parity")
    del perf["tag"]
    rows = list(csv.DictReader(open(src, encoding="utf-8-sig")))[5:]
    cells = lambda k: [float(r[k]) for r in rows if r.get(k) not in (None, "", "NA")]
    p95 = [float(np.percentile(v, 95)) if v else math.nan for v in (cells("MsBetweenPresents"), cells("MsGPUBusy"))]
    return {"perf": perf, "medians": B404["medians"]([src]), "p95": p95}


def cvar(case):
    """cvar_poke's run (find, show, then --set / --restore) on the case's image, its prints swallowed: the object's
    offset and the image after."""
    base, img = case["base"], bytearray.fromhex(case["image"])

    def rd(addr, fmt):
        o = addr - base
        return struct.unpack_from(fmt, img, o)[0] if 0 <= o <= len(img) - struct.calcsize(fmt) else None

    def wr(addr, data): img[addr - base:addr - base + len(data)] = data
    with contextlib.redirect_stdout(io.StringIO()):
        obj = CP["run"](rd, base, case["name"], case["action"], case["value"], lambda: wr)
    return {"obj": obj - base, "image": img.hex()}


def cvar_build(case):
    """The build cvar_poke's layout is checked for (pool_probe's STAMP), as the KB writes it."""
    return "0x%08X" % PP["STAMP"]


def presentmon_argv(case):
    """perf's PresentMon command line, the exe left out, with W3_PM_FLAGS = case["flags"]; PresentMon stubbed to record it
    and write a small CSV."""
    IS["OUT"], seen = case["dir"], []
    def run(argv, **_):
        seen.append(argv[1:])
        write(argv[argv.index("--output_file") + 1], "MsBetweenPresents\n" + "1\n" * 8)
    IS["subprocess"], IS["W"] = types.SimpleNamespace(run=run), types.SimpleNamespace(procs=dict)
    os.environ["W3_PM_FLAGS"] = case["flags"]
    with contextlib.redirect_stdout(io.StringIO()): IS["perf"](case["tag"], case["secs"])
    return seen[0]


def tree(root, skip=()):
    """Every folder (a name ending in "/", no content) and file (its bytes as Latin-1) under root, by relative path."""
    out = {}
    for r, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if os.path.join(r, d) not in skip)
        for d in dirs: out[os.path.relpath(os.path.join(r, d), root).replace(os.sep, "/") + "/"] = None
        for f in files: out[os.path.relpath(os.path.join(r, f), root).replace(os.sep, "/")] = open(os.path.join(r, f), "rb").read().decode("latin-1")
    return out


def launched(files):
    """A tree at launch without the parking folder, and the names parked there (its marker aside)."""
    park = "game/mods_parked_by_harness/"
    names = sorted({k[len(park):].split("/")[0] for k in files if k.startswith(park) and k != park} - {"parked.json"})
    return {"files": {k: v for k, v in files.items() if not k.startswith(park)}, "parked": names}


class Launched(BaseException):
    """Raised by the stubbed Popen: the boot stops where it would start the game."""


def boot(case):
    """ini_session.py's own `boot` on a synthetic game and documents folder (case["files"]), stopped where it would
    launch the game: the files as the game would find them, then as boot's own failure cleanup (kill_all, restore,
    unpark, unlock: what `end` runs) leaves them. Every global it reads is set here; its processes, window and keys are
    stubs, and its harness folder is the case's own."""
    root = case["dir"]
    for rel, data in case["files"].items():
        p = os.path.join(root, *rel.split("/"))
        if rel.endswith("/"): os.makedirs(p, exist_ok=True); continue
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f: f.write(data.encode("latin-1"))
    game, docs, harness = (os.path.join(root, d) for d in ("game", "docs", "harness"))
    bin_, ini = os.path.join(game, "bin", "x64_dx12"), os.path.join(harness, "cap", "ini")
    os.makedirs(os.path.join(ini, "orig"))
    seen = {}

    def popen(cmd, cwd=None):
        seen["at_launch"], seen["argv"] = tree(root, {harness}), [os.path.relpath(a, root).replace(os.sep, "/") for a in cmd]
        raise Launched()
    spec = importlib.util.spec_from_file_location("ini_writer", os.path.join(HARNESS, "ini_writer.py"))
    iw = importlib.util.module_from_spec(spec); spec.loader.exec_module(iw)
    files = {"ini": os.path.join(bin_, "ReShade.ini"), "preset": os.path.join(bin_, "ReShadePreset.ini"),
             "settings": os.path.join(docs, "dx12user.settings"), "general": os.path.join(game, "bin", "config", "base", "general.ini")}
    ns = load("ini_session.py", {"boot", "Laps", "need_501", "versions", "apply_settings", "unpark", "park_names", "park_home",
                                 "park", "PLAIN", "BRIDGE", "signals_plan", "save_path", "saves_newest_first", "write_p1", "parse_kv",
                                 "patch_bytes", "console_on", "launch_cmd", "kill_all", "ident", "restore", "remove_rdx",
                                 "remove_signals", "unlock", "sha", "tree_sha"},
              ROOT=game, GAME=bin_, MODS=os.path.join(game, "Mods"), INI=files["ini"], PRESET=files["preset"],
              SETTINGS=files["settings"], GENERAL=files["general"], SAVES=os.path.join(docs, "gamesaves"), FILES=files,
              OUT=ini, ORIG=os.path.join(ini, "orig"), STATE=os.path.join(ini, "state.json"), STARTED=os.path.join(ini, "started.json"),
              BOOTS=os.path.join(harness, "cap", "boots.jsonl"), ADDON=os.path.join(bin_, "renodx-thewitcher3remastered.addon64"),
              BUILD=os.path.join(harness, "no-build.addon64"), VAXIS=os.path.join(game, "Mods", "modGraphicsControl"),
              RDX_DST=os.path.join(game, "Mods", "modRenoDXTest"), RDX_MARK="rdx_mod", SIG_MARK="signals.json",
              MOD="renodx-thewitcher3remastered", P1="renodx-thewitcher3remastered-preset1", set_values=iw.set_values,
              W=types.SimpleNamespace(LOCK=os.path.join(ini, "session.lock"), procs=dict, chain=lambda _: [(1, "python.exe"), (2, "parent.exe")]),
              pids=list, own=lambda *a, **k: None, quen_live=lambda: None, sh=lambda *a: types.SimpleNamespace(stdout="", returncode=0),
              subprocess=types.SimpleNamespace(Popen=popen),
              ctypes=types.SimpleNamespace(windll=types.SimpleNamespace(user32=types.SimpleNamespace(GetForegroundWindow=lambda: 0))),
              shutil=shutil, glob=glob, hashlib=hashlib)
    for k in [k for k in os.environ if k.startswith("W3_")]: del os.environ[k]
    os.environ.update(W3_ADDON_ANY="1", **case["env"])
    cwd = os.getcwd(); os.chdir(harness)  # boot writes .w3ctl_prev into its working folder
    try:
        with contextlib.redirect_stdout(io.StringIO()): ns["boot"](*ns["parse_kv"](case["preset"]), case["save"])
        raise RuntimeError("boot returned without launching")
    except Launched:
        pass
    finally:
        os.chdir(cwd)
    return {"launch": launched(seen["at_launch"]), "argv": seen["argv"], "after": tree(root, {harness})}


def keys(case):
    """parse_tokens + describe: the lines `w3ctl.py keys TOKENS --dry-run` prints after its header."""
    return [W3["describe"](ev) for ev in W3["parse_tokens"](case["tokens"], case["hold"], case["gap"])]


def console(case):
    """console_type's main with the game stubbed in front, every key and sleep recorded (its two w3ctl calls through
    parse_tokens with w3ctl's defaults, the layout's console key left at 0x29); the closing check sees a black frame."""
    seen = []
    def event(ev):
        seen.append(f"sleep {round(ev[1])}" if ev[0] == "sleep" else f"key {'up' if ev[3] else 'down'} 0x{ev[2]:03X}")
    def w3ctl(argv, check):
        for ev in W3["parse_tokens"](argv[argv.index("keys") + 1:], 200, 400): event(ev)
    CT.update(W=types.SimpleNamespace(session_guard=lambda: None, find_game=lambda: 1, is_fg=lambda hwnd: True, send=event),
              subprocess=types.SimpleNamespace(run=w3ctl), time=types.SimpleNamespace(sleep=lambda s: event(("sleep", s * 1000))),
              grab=lambda gdi: np.zeros((720, 1280, 3), np.uint8), cv2=__import__("cv2"))
    CT["main"](case["line"])
    return seen


def prompt(case):
    """console_open on a synthetic w x h frame: `bg` (or `noise`: random pixels from that seed), the prompt's lit pixels
    scaled from 720p (edges rounded) at `fg` when `prompt`. The frame goes to frame.bgra for the Rust side."""
    w, h = case["w"], case["h"]
    img = (np.random.default_rng(case["noise"]).integers(0, 256, (h, w, 3), np.uint8) if "noise" in case
           else np.full((h, w, 3), case["bg"], np.uint8))
    if case["prompt"]:
        for r, c in zip(*CT["PROMPT_ON"]):
            img[round(r * h / 720):round((r + 1) * h / 720), round(c * w / 1280):round((c + 1) * w / 1280)] = case["fg"]
    np.dstack([img, np.full((h, w), 255, np.uint8)]).tofile(os.path.join(case["dir"], "frame.bgra"))
    CT["cv2"] = __import__("cv2")
    return bool(CT["console_open"](img))


def bits(case):
    return list(IS["SIG_BITS"])


def clean(x):
    """JSON has no NaN: a non-finite float goes out as its name."""
    if isinstance(x, float) and not math.isfinite(x): return str(x)
    if isinstance(x, (list, tuple)): return [clean(v) for v in x]
    if isinstance(x, dict): return {k: clean(v) for k, v in x.items()}
    return x


warnings.simplefilter("ignore")  # numpy's mean-of-nothing warning: the NaN it returns is the result
KINDS = {f.__name__: f for f in (settings, park, bridge, presentmon, presentmon_argv, bits, cvar, cvar_build, boot,
                                  keys, console, prompt)}
out = []
for case in json.loads(sys.stdin.buffer.read()):
    os.makedirs(case["dir"])
    if case["kind"] == "park": layout(case)
    try: out.append({"ok": clean(KINDS[case["kind"]](case))})
    except (Exception, SystemExit) as e: out.append({"err": f"{type(e).__name__}: {e}"})
json.dump(out, sys.stdout)
