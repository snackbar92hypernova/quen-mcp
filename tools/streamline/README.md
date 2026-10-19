# A Streamline interposer RenderDoc can see

Witcher 3 (5.01) imports D3D12 and DXGI from NVIDIA Streamline's `sl.interposer.dll`. The interposer finds the real `d3d12.dll` and `dxgi.dll` functions by walking their export tables, so RenderDoc's hooks never see those calls and RenderDoc can't capture the game.

`getprocaddress.patch` makes a non-production build resolve each export through `GetProcAddress` instead, which RenderDoc hooks. With `sl.interposer.json` (`{"enableInterposer": false}`, read only by non-production builds) beside it, Streamline's own proxies stay out of the way too. quen's `renderdoc:NAME` launch profile swaps this build in for the game's signed one, journaled, and puts NVIDIA's file back afterwards, checked by sha256 and Authenticode.

Get it one of two ways. The repo never holds the DLL.

**Download quen's build:** `quen-streamline-dev-interposer-2.14.1.zip` from quen-mcp's GitHub Releases. It holds `sl.interposer.dll` (Streamline v2.14.1 with `getprocaddress.patch`, built with `build.bat -develop`), Streamline's `license.txt` and its `3rd-party-licenses.md`. Unzip it outside the game folder. The DLL's sha256 is `a0483eb3de9063731f0e85e4e9b0f38e9884a33651a53c7b1e07f1d7822b6e24`: `quen doctor` checks it and says "quen's release build". The build is quen's, not NVIDIA's: NVIDIA neither made nor endorses it.

**Or build it yourself**, once:

1. Get NVIDIA Streamline 2.14.1, the version the game ships, into a folder outside your user profile (the build embeds its source paths, which would name you): `git clone --branch v2.14.1 https://github.com/NVIDIA-RTX/Streamline C:\src\Streamline`.
2. Apply the patch in the clone: `git apply <quen-mcp>\tools\streamline\getprocaddress.patch`. It changes only `source\core\sl.interposer\hook.cpp`, and only in non-production builds (`#ifndef SL_PRODUCTION`).
3. Run `setup.bat vs2022` (it fetches the build dependencies into `C:\packman-repo` and makes the Visual Studio project), then `build.bat -develop`. Streamline's README lists what the build needs.
4. The DLL is `_artifacts\sl.interposer\Develop_x64\sl.interposer.dll`. It's unsigned, which is expected. Your build's sha256 differs from the release's unless your folder and compiler match; the doctor then says "built locally".
5. In quen's config, set `streamline_dev_interposer` to that file (or to the downloaded one) and `renderdoc` to your `renderdoccmd.exe`. `quen doctor` checks both, and checks that the game's own interposer is NVIDIA's signed original.

Never copy the dev build into the game folder yourself: quen's launch does it, and its end (or `session_recover` after a crash) puts the signed original back.

Streamline is © NVIDIA Corporation under the MIT licence; its notice is in `LICENSE-Streamline.txt`, which covers the patch too. The notices for the third-party parts Streamline uses are its `3rd-party-licenses.md`, in every clone. The release zip ships that file as fetched from the v2.14.1 tag, checked by sha256; it isn't committed here, because an author's email in it trips quen's privacy scan.
