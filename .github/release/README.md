# qaros-rerun

The [Rerun](https://rerun.io) viewer with a second output to QAROS, QuaternAR's AR streaming runtime: a release build for one platform. `BUILD.txt` names the version, the commit and the QAROS C API header it was built against.

The QAROS Visualizer installs this package from its Sources catalog ("Rerun"). It also runs on its own, unpacked anywhere.

| File | What it is |
| --- | --- |
| `qaros-rerun` (`.exe` on Windows) | The viewer |
| `log-test-scene` (`.exe` on Windows) | Logs a small test scene to a running viewer |
| `LICENSE` | The viewer's license (MIT) |
| `THIRD-PARTY-LICENSES.txt` | The licenses of the crates compiled into both programs, Rerun's among them |

## What the PC needs

- QAROS, for streaming to headsets: the version the release's `manifest.json` names as `min_qaros_version`, or later. The viewer loads the installed QAROS runtime by itself; otherwise set `QAROS_DLL` to `qar-streaming-c.dll` (or `--qaros-library`).
- A GPU with a Vulkan driver.
- Windows: the Microsoft Visual C++ 2015-2022 Redistributable (x64). The QAROS setup installs it from QAROS 0.10.0; on a PC without it, Windows reports `VCRUNTIME140.dll` missing: install it from Microsoft (`winget install Microsoft.VCRedist.2015+.x64`).
- Linux: a desktop session (X11 or Wayland) with the usual GTK 3 and xkbcommon libraries.

## Run

From this directory, start the viewer and log to it from any Rerun SDK on port 9876:

```powershell
.\qaros-rerun.exe --sink qaros --local-app --panels
.\log-test-scene.exe 600    # second terminal: a 1 m cube, points, axes
```

`qaros-rerun --help` lists every option, `qaros-rerun --version` prints the version. Windows may block a downloaded executable: right-click it, Properties, Unblock, or run `Get-ChildItem | Unblock-File` here once.

Documentation (quick start, integration guide, demos): `docs/` in [the repository](https://github.com/Quaternar/qaros-rerun), at the commit in `BUILD.txt`.
