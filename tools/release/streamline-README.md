# quen's Streamline dev interposer (2.14.1)

`sl.interposer.dll` is a build of the interposer from NVIDIA Streamline 2.14.1, made by quen's maintainers from NVIDIA's public source (https://github.com/NVIDIA-RTX/Streamline, tag `v2.14.1`) with one patch, `tools/streamline/getprocaddress.patch` in quen-mcp. It's a non-production (Develop) build, and unsigned.

**NVIDIA neither made nor endorses this build.** Streamline is © NVIDIA Corporation under the MIT licence (`license.txt`); the notices for the third-party parts it uses are in `3rd-party-licenses.md`.

## What it's for

The Witcher 3 (5.01) loads D3D12 and DXGI through Streamline's interposer, which finds the real functions in a way RenderDoc's hooks never see, so RenderDoc can't capture the game. The patch makes this build look them up through `GetProcAddress`, which RenderDoc hooks.

## How quen uses it

Only quen's `renderdoc:NAME` launch profile uses it:

1. Keep the DLL anywhere outside the game folder.
2. Set `streamline_dev_interposer` to its path in quen's config (`quen setup`, see INSTALL.md), and `renderdoc` to your `renderdoccmd.exe`. `quen doctor` checks both.
3. For a renderdoc launch, quen swaps this build in for the game's signed one, journaled, with an `sl.interposer.json` beside it that keeps Streamline's own proxies out of the way. The session's end (or `session_recover` after a crash) puts NVIDIA's original back, checked by sha256 and Authenticode.

Never copy it into the game folder yourself.

To build it yourself instead, follow `tools/streamline/README.md` in quen-mcp.
