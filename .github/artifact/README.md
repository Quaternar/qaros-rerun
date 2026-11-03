# qaros-rerun for Windows x64

A release build of the `qaros-rerun` viewer from CI. `BUILD.txt` names the commit and the CI run it came from.

| Path | What it is |
| --- | --- |
| `target/release/qaros-rerun.exe` | The viewer |
| `target/release/log-test-scene.exe` | Logs a small test scene to a running viewer |
| `demos/` | The demo launchers, run with [uv](https://docs.astral.sh/uv/) |

The layout matches a source checkout, so the run commands in the repository's docs work unchanged from this directory.

## What the PC needs

- The Microsoft Visual C++ Redistributable (x64). Without it Windows reports `VCRUNTIME140.dll` missing; install it with `winget install Microsoft.VCRedist.2015+.x64`.
- QAROS 0.9.0-dev.3177 or later, for streaming to headsets. The viewer finds the installed `qar-streaming-c.dll` by itself; otherwise set `QAROS_DLL` to it.
- A GPU with a Vulkan driver.
- uv, for the demos: `winget install astral-sh.uv`.

## Run

In PowerShell, from this directory, start the viewer and log to it from your own program (`docs/quick-start.md`):

```powershell
.\target\release\qaros-rerun.exe --sink qaros --local-app --panels
```

Or start a demo, which starts the viewer itself:

```powershell
uv run demos/structure_from_motion/run.py -- --sink qaros --local-app --panels
```

Windows may block a downloaded executable: right-click it, Properties, Unblock, or run `Get-ChildItem -Recurse | Unblock-File` here once.

Documentation (quick start, integration guide, demos): `docs/` in [the repository](https://github.com/Quaternar/qaros-rerun), at the commit in `BUILD.txt`.
