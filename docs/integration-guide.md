---
title: Integration guide
description: From a build to your own Rerun data in a QAROS headset, with options and troubleshooting.
sidebar_position: 2
---

# Integration guide: your Rerun data in a QAROS headset

You already log data with the Rerun SDK. `qaros-rerun` is a Rerun 0.40.0 viewer that also renders the 3D view for every headset in a QAROS session, one camera per eye, and opens your 2D views as QAROS GUI panels next to it. Your logging code does not change: point it at this viewer instead of the stock one.

This guide takes you from a build to your own data in the headset. To see it first with real data, run one of the [demos](demos/README.md).

## What runs where

Your program can run on any machine and any OS. The viewer runs on Windows, on the machine with the QAROS Hub, because it streams through QAROS's Windows runtime.

| Machine | Runs | Connections |
| --- | --- | --- |
| Any (Linux, macOS, Windows) | Your program with the Rerun SDK 0.40.0 | To the viewer: gRPC, TCP 9876 |
| Windows, the Hub machine | The QAROS Hub and its visualizer; `qaros-rerun` | `qaros-rerun` to the Hub through QAROS's library `qar-streaming-c.dll`; panel pages served on TCP 9877 |
| Headset | The QAROS player app | To the Hub (QAROS's own traffic); panel pages from TCP 9877 |

```text
your program ──gRPC :9876──▶ qaros-rerun ──qar-streaming-c──▶ QAROS Hub ──▶ headset
                             (Windows, the Hub machine)                      │
                             panel pages :9877 ◀──────── HTTP ───────────────┘
```

QAROS terms used below, with links to the [QAROS documentation][QAROS-DOCS]:

- **Hub**: the Windows machine that runs the QAROS runtime, the session and the mixer ([glossary][GLOSSARY]).
- **Visualizer**: the Hub's desktop window into the shared room; it renders the mixed scene a headset user would see ([Running the Hub][VISUALIZER]).
- **Target app**: anything that receives streams, a headset's player app or a visualizer ([Concepts][CONCEPTS]). The viewer renders one stream per running target app.
- **Peer**: a participant of the session, a device or an application ([Peers][PEERS]).
- **App volume**: a named box in the room where one application renders; QAROS masks the stream to it ([App volumes][APP-VOLUMES]).
- **GUI panel**: a flat web page placed in the room ([GUI panels][GUI-PANELS]).
- **Pairing code**: the Hub's short-lived, single-use code for a first join ([glossary][GLOSSARY]).

## What you get

- **The 3D view in stereo.** The first 3D view the viewer draws is rendered twice per headset frame, once per eye, with the eye poses and fields of view QAROS hands out for that frame. Color and depth go to QAROS, so the headset can reproject. Pixels where nothing was drawn are transparent, so only your data appears in the room.
- **One stream per user.** Every running target app in the QAROS session is a user with a render stream of its own. By default the viewer renders for up to two users at a time; a third waits for a free slot. Users who leave are released.
- **2D views as panels.** With `--panels`, every other view (images, time series, maps) is cropped from the viewer's UI, served as a small web page, and opened as a QAROS GUI panel within reach of the first headset user near the app volume. The Hub keeps each panel where it is, or where a user moved it, across runs.
- **A timeline you touch.** A timeline panel with a slider and a Play button opens 0.5 m in front of that user and 0.2 m below the eyes. Drag the slider with a finger to scrub the recording; Play resumes. It follows the desktop time, and the desktop follows it.
- **The desktop viewer.** In a window, the viewer behaves as Rerun's: timeline, blueprint and selection work as usual, and the headset shows the same time point.

What does not reach the headset: Rerun's labels and other overlays drawn after the 3D render. From the glasses, the timeline panel is the way to control the viewer; the 2D view panels show images of the views and do not take input.

## What you need

| Machine | For | Needs |
| --- | --- | --- |
| Windows (the Hub machine) | Streaming to headsets | QAROS 0.9, a GPU with a Vulkan driver, Visual Studio 2022 Build Tools with the "Desktop development with C++" workload, Git, rustup |
| macOS | Trying the viewer without QAROS | rustup; the viewer writes headset frames as PNGs instead of streaming |

The Vulkan path is developed on NVIDIA GPUs, whose driver ships the Vulkan runtime; no Vulkan SDK is needed.

**Network.** The viewer listens for SDK connections on TCP 9876 (`--port`) on every network interface. With `--panels 0.0.0.0:9877` it also serves panel pages on TCP 9877 to the whole network. Neither port asks for authentication: anyone who reaches 9876 can log data into the viewer, and anyone who reaches 9877 can read the panel pages. Use them on a network you trust. If Windows Defender Firewall blocks inbound connections to `qaros-rerun.exe`, headsets cannot load the panels and SDKs on other machines cannot connect. Allow both ports for the viewer, in an elevated PowerShell:

```powershell
New-NetFirewallRule -DisplayName "qaros-rerun" -Direction Inbound -Action Allow -Protocol TCP -LocalPort 9876,9877 -Program "C:\path\to\qaros-rerun\target\release\qaros-rerun.exe"
```

QAROS's own ports are listed on [QAROS's installation page][INSTALLATION].

## Install and first run

### Build

```sh
git clone https://github.com/Quaternar/qaros-rerun
cd qaros-rerun
```

The Rust binding to QAROS (`qaros`), a stub of the runtime for tests (`qaros-stub`) and a test extension come from [github.com/Quaternar/qaros][QAROS-REPO] as git dependencies, pinned by revision in `Cargo.toml`; Cargo fetches them on the first build, so that build needs network access.

`rust-toolchain.toml` pins Rust 1.96, and rustup fetches it on the first `cargo` call inside the repository. Call `cargo` from inside `qaros-rerun`: elsewhere rustup picks your default toolchain, which the workspace refuses if it is older than 1.96.

```sh
cargo build --release -p qaros-rerun
```

The first build compiles Rerun and takes a while: a clean release build on a Windows laptop took about 25 minutes, a rebuild after a small change 25 s.

### First run without a headset

The default sink writes PNGs with scripted head motion instead of streaming, so you can check the viewer on any machine before QAROS is involved. With `--quit-when-done` the viewer exits by itself: it waits for a 3D view, shows 240 frames once the test scene arrives, prints a summary and exits.

```sh
./target/release/qaros-rerun --headless 1600x1000 --out out/headset \
    --frames 240 --write-every 30 --panels 127.0.0.1:9877 --quit-when-done &
sleep 5
./target/release/log-test-scene 300
wait
ls out/headset/user-1
```

In PowerShell:

```powershell
$viewer = Start-Process .\target\release\qaros-rerun.exe -PassThru -NoNewWindow -ArgumentList "--headless 1600x1000 --out out\headset --frames 240 --write-every 30 --panels 127.0.0.1:9877 --quit-when-done"
Start-Sleep 5
.\target\release\log-test-scene.exe 300
$viewer.WaitForExit()
Get-ChildItem out\headset\user-1
```

To stop the viewer earlier: `kill %1` in the shell, `Stop-Process -Id $viewer.Id` in PowerShell.

`log-test-scene` logs a 1 m orange cube, a ring of light-blue points around it, axes, an image and a time series through the standard SDK connection. Among the viewer's log lines you see (timestamps removed):

```text
INFO qaros_rerun: listening for Rerun SDK connections on port 9876; headset sink: Images
INFO qaros_rerun::panel_capture: panels: "camera/image" at http://127.0.0.1:9877/views/camera_image
INFO qaros_rerun::panel_capture: panels: "plots/sine" at http://127.0.0.1:9877/views/plots_sine
INFO qaros_rerun::viewer_app: headset: frames started 240, shown 240, abandoned 0; draw data copied 2868 of 2868
```

While the viewer runs, `http://127.0.0.1:9877/views` lists the panels as JSON (`["camera_image","plots_sine"]`). `out/headset/user-1` then holds every 30th frame, `frame_00000` to `frame_00210`, each as `frame_NNNNN_color.png` (left and right eye side by side), `frame_NNNNN_depth.png` and `frame_NNNNN.txt` (poses, fields of view, near and far). A color frame looks like this:

![Left and right eye of the test scene: an orange cube inside a ring of light-blue points](media/test-scene-color.png)

### Where the QAROS library comes from

The viewer loads `qar-streaming-c.dll` from QAROS's runtime at run time. It takes the first of:

1. `--qaros-library PATH`;
2. the environment variable `QAROS_DLL`;
3. the binding's default lookup: a runtime already loaded in the process, one beside the viewer's executable, the folder named by `QAROS_RUNTIME_DIR`, the installed QAROS (from `HKLM\SOFTWARE\Quaternar\QAROS`), then each folder on `PATH`. With QAROS installed, the viewer needs no setting.

The runtime's other binaries are loaded from the same folder, unless `QAROS_RUNTIME_DIR` names another.

The library ships in QAROS's runtime ZIP, in its `bin` folder next to every runtime DLL. [QAROS's installation page][INSTALLATION-SDK] says to request the ZIP from [quaternar.com](https://www.quaternar.com/). Unzip it, keep `bin` whole, and set `QAROS_DLL` to its `bin\qar-streaming-c.dll`. The viewer refuses a library that lacks any function of the QAROS 0.9 header the binding is built against, and names the missing functions.

```powershell
$env:QAROS_DLL = "C:\path\to\qaros\bin\qar-streaming-c.dll"
```

### First run into QAROS (Windows)

The viewer joins the QAROS session as an application, creates its app volume "rerun" and streams to every target app it finds. Start it on the Hub machine as a local app of the Hub. No code is needed:

```powershell
.\target\release\qaros-rerun.exe --sink qaros --local-app --panels 0.0.0.0:9877
```

On the first start of a viewer executable the Hub asks a person at the machine to allow it, and the viewer waits for that answer (up to 2 minutes by default). Start the viewer in the signed-in desktop session; the Hub refuses a viewer started outside it (`ONBOARDING_HUB_NOT_AUTHENTIC`).

In a second terminal, log the test scene:

```powershell
.\target\release\log-test-scene.exe 600
```

The viewer's log then shows, in order: the runtime version (`qaros: runtime ...`), `qaros: connected; app volume "rerun" ready, looking for target peers`, one `qaros: target <peer> (<name>) found by the peer list` per target app, and `qaros: rendering for user-1 (...)` once its stream runs.

To see the scene:

- **On the Hub machine:** open the visualizer (left-click the QAROS tray icon). It is a target app too, and shows the room as a headset would.
- **In a headset:** the headset needs the QAROS player app and a first join to this Hub with a pairing code ([Getting started][USER-START]). Start the player app; the viewer logs the headset as a new target and starts rendering for it.

Look toward `--origin`, by default 1.6 m along the room's forward axis from the room origin and 0.5 m up. The orange cube stands there with its base on that point and the ring of light-blue points around its middle, as in the frame above.

To stop: Ctrl+C in the viewer's console, closing its window, or Stop in QAROS. The viewer leaves QAROS in order (senders, panels, app volume, session) and exits; a second Ctrl+C ends it at once.

### Other ways to join

**With a pairing code.** Pass the Hub's current code on the first start. The viewer stores its onboarding id in `qaros-rerun.onboarding-id.txt` in the working directory and rejoins with it on later starts, without a code. A code is short-lived, and one failed attempt spends it.

```powershell
.\target\release\qaros-rerun.exe --sink qaros --pairing-code <PAIRING-CODE> --panels 0.0.0.0:9877
```

**Started by QAROS.** In the visualizer, switch to Developer mode and add `qaros-rerun.exe` in the Source Applications panel as a QAROS API app ([Source Applications][SOURCE-APPS]), with its arguments one per line: `--sink`, `qaros`, `--launched-by-qaros`. On Windows the viewer then joins as a local app (the Hub asks to allow it on its first start); elsewhere QAROS passes a launch invite on stdin. Stop in the panel closes stdin, which ends the viewer. Started this way the viewer runs headless at 1280x720 unless you add `--window` or `--headless WxH`. Its working directory is the executable's folder, so set `QAROS_DLL` for the system or add `--qaros-library` and the path as two more lines. The viewer's output lands in the Hub's log folder.

## Send your own data

The viewer listens for Rerun SDK connections on gRPC port 9876 (`--port` changes it) on every network interface. Connect to it from your program as you would to a running Rerun viewer. Use the SDK of the viewer's version, 0.40.0: `pip install rerun-sdk==0.40.0` in Python, `rerun = "=0.40.0"` in Rust. Other SDK versions are untested with this viewer.

Python:

```python
import rerun as rr

rr.init("my_app")
rr.connect_grpc("rerun+http://127.0.0.1:9876/proxy")
rr.log("world/points", rr.Points3D([[0, 0, 0], [0.5, 0, 0.5]]))
```

Rust:

```rust
let recording = rerun::RecordingStreamBuilder::new("my_app")
    .connect_grpc_opts("rerun+http://127.0.0.1:9876/proxy")?;
```

The URL is the one `log-test-scene` and the demo launcher use. From another machine, replace `127.0.0.1` with the Hub machine's address and allow port 9876 through its firewall ([What you need](#what-you-need)).

### Place your data in the app volume

QAROS cuts every stream to its app volume's box ([the mixer][MIXER]). By default that box is 2 m on each side, and the point `--center` of your data (in Rerun units) sits in its middle. Data that is meters to hundreds of meters across is therefore mostly outside the box: with the defaults, only a 2 m cube around Rerun's origin can show. Fit the data with `--center` and `--scale`:

1. Run the viewer with `RUST_LOG=qaros_rerun=debug`. Every headset frame logs the scene's bounding box in Rerun units and in the view's coordinates. The image sink logs it too, so you can do this before QAROS is involved.
2. `--center` is the middle of the box: for each axis, (min + max) / 2.
3. `--scale` is room meters per Rerun unit. For the data to fit, take the smallest of: volume width / extent along the view's right axis, volume length / extent along its forward axis, volume height / extent along its up axis (with `--yaw 0`; for `RFU` these are X, Y and Z).

For example, a recording in `RFU` coordinates (Z up) logged this line:

```text
headset: frame FrameKey { user: UserId(1), frame_id: 0 }, scene bounds Vec3(-40.0, -30.0, 0.0) to Vec3(40.0, 30.0, 10.0) in RFU
```

The middle is `0,0,5`. The extents are 80 (right), 60 (forward) and 10 (up). With the default 2 m volume the scale is the smallest of 2/80, 2/60 and 2/10, which is 0.025:

```powershell
.\target\release\qaros-rerun.exe --sink qaros --local-app --panels 0.0.0.0:9877 --center 0,0,5 --scale 0.025
```

The data then fills 2 m by 1.5 m by 0.25 m of the room. For a larger model, grow the volume and the scale together, for example `--volume-size 4,3,1 --scale 0.05` (4 m by 3 m by 0.5 m); the room must have that space.

The test scene logs `scene bounds Vec3(-0.99962366, -0.99962366, 0.0) to Vec3(0.99962366, 0.99962366, 1.4) in RFU`. Its middle is `0,0,0.7`: with the default `--center 0,0,0` the top 0.4 m of its up axis lies above the volume.

`--origin X,Y,Z` moves the volume, and the data with it, in the room (meters, OpenXR axes: Y up, -Z forward; default `0,0.5,-1.6`). `--yaw DEGREES` turns the data about the room's up axis.

### What the headset shows

- **One 3D view.** The first 3D view drawn keeps the stream for as long as it is drawn every frame. The log does not name the view being streamed, so with several 3D views on screen, send a blueprint with a single 3D view. A recording without a 3D view streams nothing and opens no panels.
- **Right-handed coordinates.** The view's coordinates (for example `RFU`, Z up) are rotated onto the room's Y-up axes. Left-handed view coordinates are not streamed; the log says `not streaming this view: left-handed (...)`. `--up CODE` overrides what the view declares.
- **Two viewers at once** need two `--volume-name`s. QAROS hands a viewer that asks for an existing name that volume and its one render stream. `--display-name` sets the label people see (default "Rerun"; the viewer's peer is "Rerun viewer").

### Recordings (`.rrd`)

The viewer takes no file arguments: anything on its command line that is not an option stops it with `unknown option`. Replay a recording through the SDK instead. First read its application id; `rerun rrd print` shows it as the second field of `store_id`:

```sh
rerun rrd print recording.rrd
```

```text
StoreInfo {
    store_id: StoreId(
        Recording,
        "robot_survey",
        ...
```

Then replay it under that id. The viewer applies a blueprint only to recordings of the same application id. This sequence, the one `demos/launcher.py` uses, sends the blueprint before the data so the layout is right while it loads, and again after it so it wins over a blueprint stored in the recording:

```python
import rerun as rr
import rerun.blueprint as rrb

blueprint = rrb.Blueprint(rrb.Spatial3DView(origin="/world"))

recording = rr.RecordingStream("robot_survey")
recording.connect_grpc("rerun+http://127.0.0.1:9876/proxy")
recording.send_blueprint(blueprint, make_active=True, make_default=True)
recording.log_file_from_path("recording.rrd")
recording.flush()
recording.send_blueprint(blueprint, make_active=True, make_default=True)
recording.flush()
```

Recordings with many small chunks load slowly. `rerun rrd optimize in.rrd -o out.rrd` compacts them.

## Choose the path: Vulkan or CPU

`--qaros-graphics` decides how frames reach QAROS. The default is `vulkan` on Windows and `cpu` elsewhere.

| | `vulkan` | `cpu` |
| --- | --- | --- |
| How | The viewer creates its own Vulkan device on the GPU of the QAROS session and renders each eye straight into the images of that user's Vulkan render sender. No pixel passes through the CPU. | Each eye is rendered, read back to the CPU, and copied into the textures of a CPU render sender. |
| When a frame goes out | In the viewer frame its eyes were drawn in | One viewer frame or more later, once the readback arrives |
| Rate | Set by the viewer's own frame time | Bound by the readback; two users share it, so each gets about half |
| Needs | A Vulkan driver with `VK_KHR_external_memory_win32` and `VK_KHR_external_semaphore_win32`; the Vulkan graphics backend | Any graphics backend |
| Depth | `--depth forward` or `reverse` | Also `--depth linear` |

Use the Vulkan path on the Hub machine. Use `--qaros-graphics cpu` when the Vulkan path does not start on your GPU, to compare against it when frames look wrong, or with `--depth linear`.

**Which GPU.** The viewer creates its device on the adapter the session reports. When the session names none, it falls back to the first discrete NVIDIA GPU, else the first discrete GPU, else the first device, and logs a warning. On a laptop with two GPUs, check the log: `qaros: the session's GPU is adapter <LUID>` and `qaros: creating the viewer's Vulkan device on ...` should name the same GPU. `--vulkan-device luid:<16 hex digits>` names a device yourself; the viewer then creates it first and joins the session on its adapter. Other selectors: `default`, `discrete`, `name:TEXT`, `uuid:HEX` (32 hex digits), `index:N`.

**When frames look wrong**, change one switch at a time:

| What you see | Try |
| --- | --- |
| Red and blue swapped | `--color-format rgba` (default `bgra`) |
| Content offset, at the wrong scale, or turning twice with the volume | `--pose-space room` (default `app`) |
| Content swims or tears as the head moves while placement is right | `--depth reverse`, then on the CPU path `--depth linear` |
| Black or garbage on the Vulkan path | `--qaros-graphics cpu` on the same machine, to tell the path from the scene |
| Wrong eye image for the pose | `--frame-stamp`, which draws `U<user> F<frame> L\|R` into every eye: both eyes of a stream must show the same frame number, counting up |

## Panels, users and the decoupled render loop

### Panels

`--panels ADDRESS` starts the panel server: `http://ADDRESS/views` lists the published 2D views as JSON, and `http://ADDRESS/views/<name>` is a view's page. Views are captured every 250 ms (`--panel-interval-ms`). With `--sink qaros`, each view gets a GUI panel once it has been published for 3 s: 0.45 m wide, created within reach of the first headset user near the app volume (any viewer near it once no headset streamed for 60 s): 0.8 m from the head, to the right of the gaze, top edges just below the eyes, facing the head. The Hub keeps a panel's place across runs, so a restart finds the panels where they were left; close them in the Hub Display for fresh placement.

- **Reachability.** Bind to `0.0.0.0:<port>` so headsets can load the pages, and allow the port through the firewall. The viewer then names the pages by this machine's LAN address and logs the choice (`panels: pages named by ...`). If that is the wrong network, pass `--panel-base-url http://<reachable address>:<port>`.
- **Across runs.** A view's panel name and URL stay the same on every run. At Stop the viewer hides its panels instead of closing them (`--panels-at-stop keep` leaves them showing), and the next run reattaches to them. A reattached panel whose page loads nothing within 10 s is loaded again once; `--dead-panels leave` turns that off.
- **Cost.** Each capture reads the whole viewer UI back from the GPU. A longer `--panel-interval-ms` gives the headset more frames.
- A taken panel port moves the server to a free port, with a warning in the log.

### Stream panels

`--panel-route stream` takes the panel server out of the path. Each 2D view becomes a GUI panel whose content is a video stream the viewer owns (a CPU `QarVideoSender` attached with `QarGuiPanelStreamIdExt`): the viewer copies the view's pixels into the stream, and the mixer draws them as a texture, with no page to load and no second renderer. The timeline panel is drawn by the viewer too, and a finger on it comes back as typed panel gestures (tap, drag with its start and end) that the viewer turns into the same time commands. No port is bound for panels, so `--panels` only turns them on, and `--panel-base-url` and `--dead-panels` do not apply.

- **QAROS build.** Use it with QAROS 0.9.0-dev.3177 or later. Earlier mixers give every panel its own Ultralight renderer, and a stream panel's restart crashes the mixer ([known issues](known-issues.md#mixer-crash-when-stream-panels-restart-before-3177)).
- **Names.** Stream panels have names of their own (`<view>.stream.<volume>.rerun`, `timeline.panel-stream.<volume>.rerun`), so the two routes never take each other's panels. A stream run leaves web panels from earlier runs alone and logs them; close them once in the Hub Display.
- **Going back.** `--panel-route web` on the same build works in the same Hub session. A build from before stream panels cannot list panels while a stream panel exists in the session, so it needs a Hub "New session" first.

### Two users

Users are the running target apps in the QAROS peer list. The viewer creates a render sender for each one it finds; the first that answer take the slots (`--max-users`, default 2), and the others wait until a slot frees. Expect these log lines:

- `qaros: rendering for user-<n> (<name>, peer ...)` when a user's stream starts;
- `qaros: released user-<n> (peer ...)` when a user leaves, stops running, or stalls;
- `qaros: target <peer> held back for 600s, also in later runs, ...` for a target that did not answer.

A target that does not answer sender creation costs no slot. It is retried about once a minute, so a headset switched on after the viewer started gets its stream within about a minute. The viewer remembers such targets for 10 minutes in `qaros-rerun.unanswered-peers.txt`, next to the onboarding id file.

Each user's two eyes are drawn from the same scene in every viewer frame.

### The decoupled render loop (`--decoupled-render`)

This mode is experimental, off by default, and may change in a later version. By default the eyes are drawn inside the viewer frame, after Rerun rebuilds the scene from your data, so a slow scene update holds the headset at the scene's rate and head motion stutters. `--decoupled-render` draws the eyes on a render thread of their own from the last finished scene, with fresh head poses: head motion stays smooth while the data itself updates at the scene's rate. Try it when your scene updates slowly (many entities, large point clouds, long time windows).

- With the Vulkan path it needs a headless viewer (`--headless WxH`, or `--launched-by-qaros` without `--window`); in a window, use `--qaros-graphics cpu`.
- Videos and other textures Rerun rewrites in place can show newer content than the rest of a held scene.
- A failure on the render thread ends the viewer with exit code 1.

## Troubleshooting

| Symptom | Cause | What to do |
| --- | --- | --- |
| The first build fails fetching `https://github.com/Quaternar/qaros` | No network access, or Git cannot reach GitHub | Build once with network access; later builds use Cargo's git cache |
| `no usable onboarding id in qaros-rerun.onboarding-id.txt; pass --pairing-code with the Hub's current code` | First start, or a different working directory | Start with `--pairing-code`, or use `--local-app` |
| `ONBOARDING_HUB_NOT_AUTHENTIC` | Started outside the signed-in desktop session (over SSH, as a service) | Start the viewer in the signed-in desktop session |
| The viewer waits at start with `--local-app` | The Hub asks a person to allow this executable | Allow it in the Hub; every viewer executable the Hub has not allowed before is asked about |
| The library does not load, or loads with missing symbols listed | `QAROS_DLL` points at an older runtime, or the binding's default lookup (already loaded, beside the executable, `QAROS_RUNTIME_DIR`, the installed QAROS from `HKLM\SOFTWARE\Quaternar\QAROS`, then `PATH`) finds none or an older one | Point `QAROS_DLL` or `--qaros-library` at the `qar-streaming-c.dll` of a current runtime ([Where the QAROS library comes from](#where-the-qaros-library-comes-from)) |
| Connected, but no `target ... found` line | No running target app in the session | Open the visualizer or start the player app on a headset; check the `qaros: peer ...` lines in the log |
| `QAROS handed back app volume "rerun" closed by an earlier run; content may not show` | QAROS returned a volume a previous run closed | Start with another `--volume-name` |
| Nothing in the headset, the log streams | Data outside the app volume, no 3D view, or left-handed coordinates | See [Place your data in the app volume](#place-your-data-in-the-app-volume) and [What the headset shows](#what-the-headset-shows) |
| An SDK on another machine cannot connect | Firewall, or the wrong address | Allow TCP 9876 for `qaros-rerun.exe` ([What you need](#what-you-need)); connect to the Hub machine's LAN address |
| Panels white or empty | The panel's browser cannot reach the panel server | `--panels 0.0.0.0:<port>`, allow the port through the firewall, and `--panel-base-url` with an address the headsets reach |
| `--decoupled-render with the QAROS Vulkan sink needs --headless ...` | Decoupled render in a window on the Vulkan path | Add `--headless WxH`, or use `--qaros-graphics cpu` |
| `--qaros-graphics vulkan (the default on Windows) and --image-targets gpu write depth into a depth image ...` | `--depth linear` on the Vulkan path | `--depth forward` or `reverse`, or `--qaros-graphics cpu` |
| Build fails with E0514 ("found crate compiled by an incompatible version of rustc") | Crates from two toolchains in `target` | Run `cargo` from inside the repository, then `cargo clean` |
| On a Mac, the Vulkan backend is missing | No Vulkan loader on the library path | `brew install molten-vk vulkan-loader` and `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib`, set on the viewer itself (Homebrew's `cargo` drops it) |

For a log with more detail, set `RUST_LOG`, for example `RUST_LOG=info,qaros_rerun=debug,qaros_sink=debug`.

## Reference

### Command-line options

`qaros-rerun --help` prints a shorter summary of these options. An unknown option or a bad value prints the error and the usage and exits with code 2.

**Headset stream**

| Option | Default | What it does |
| --- | --- | --- |
| `--sink image\|qaros\|off` | `image` | PNGs with scripted head motion, QAROS render streams, or a plain viewer; `images` is accepted too |
| `--frames N` | image: 120; qaros: unlimited | Image sink: each simulated user leaves after N shown frames. QAROS: stop after N shown frames across all users |
| `--quit-when-done` | off | Close the viewer once the sink stops streaming |
| `--opaque-background` | off | Keep Rerun's background instead of making empty pixels transparent |
| `--max-users N` | 2 | Render for at most N users at a time; later users wait for a slot |
| `--frame-stamp` | off | Draw user, frame id and eye into every eye image |
| `--decoupled-render` | off | Experimental: draw the eyes on a render thread from the last finished scene; needs a sink; with the QAROS Vulkan path only headless |

**QAROS**

| Option | Default | What it does |
| --- | --- | --- |
| `--local-app` | off | Join the Hub on this machine as a local app, with no code or invite; excludes `--pairing-code` and `--onboarding-id-file` |
| `--pairing-code CODE` | none | The Hub's current pairing code for the first start; also used when a stored id cannot be rejoined |
| `--onboarding-id-file PATH` | `qaros-rerun.onboarding-id.txt` | Where the onboarding id is kept; `qaros-rerun.unanswered-peers.txt` goes next to it |
| `--launched-by-qaros` | off | Join as QAROS expects of an app it launches (as a local app on Windows; elsewhere with the first line of stdin, the launch invite or a code typed by hand) and stop when stdin closes; runs headless at 1280x720 unless `--window` or `--headless` is given |
| `--qaros-library PATH` | `QAROS_DLL`, else the default lookup | The `qar-streaming-c` library |
| `--qaros-graphics cpu\|vulkan` | `vulkan` on Windows, `cpu` elsewhere | How frames reach QAROS ([Choose the path](#choose-the-path-vulkan-or-cpu)) |
| `--pose-space app\|room` | `app` | Space of the stream's eye poses: `app` maps them into the room with `--origin`, `--yaw` and `--scale` and reports near and far in app units; `room` takes them as they come |
| `--volume-size W,L,H` | `2,2,2` | App volume size in meters: width (room X), length (room Z), height (room Y) |
| `--volume-name NAME` | `rerun` | The app volume's name; viewers running at once need different names |
| `--display-name NAME` | `Rerun` | The volume's label for people; the viewer's peer is "NAME viewer" |
| `--color-format bgra\|rgba` | `bgra` | Color format requested from the sender |
| `--panel-route web\|stream` | `web` | How 2D views and the timeline reach their panels: `web` serves pages from the panel server, `stream` sends each one as a video stream of the viewer ([Stream panels](#stream-panels)); `stream` needs QAROS 0.9.0-dev.3177 or later |
| `--panel-base-url URL` | `http://` plus the `--panels` address, `0.0.0.0` replaced by the LAN address | How GUI panels reach the panel server |
| `--panels-at-stop hide\|keep` | `hide` | What the panels become at Stop; they are never closed |
| `--dead-panels reload\|leave` | `reload` | Whether a reattached panel whose page loads nothing within 10 s is loaded again once |

**Placement and depth**

| Option | Default | What it does |
| --- | --- | --- |
| `--origin X,Y,Z` | `0,0.5,-1.6` | Room position of the data's center (OpenXR: Y up, -Z forward, meters); also the app volume's center |
| `--center X,Y,Z` | `0,0,0` | Point of the data (Rerun units) placed at `--origin` |
| `--scale S` | 1 | Room meters per Rerun unit; must be positive |
| `--yaw DEGREES` | 0 | Turn the data about the room's up axis |
| `--up CODE` | the view's own | Override the view coordinates, for example `RFU` |
| `--near METERS` | 0.05 | Near plane in room meters |
| `--far METERS` | 20 | Finite far plane in room meters |
| `--depth forward\|reverse\|linear` | `forward` | Depth encoding; `linear` only on the CPU path |

**Panels**

| Option | Default | What it does |
| --- | --- | --- |
| `--panels ADDRESS` | off | Serve 2D views at `http://ADDRESS/views/<name>`, for example `0.0.0.0:9877` |
| `--panel-interval-ms MS` | 250 | Capture interval |

**Viewer and graphics device**

| Option | Default | What it does |
| --- | --- | --- |
| `--port PORT` | 9876 | gRPC port the SDKs connect to, on every network interface |
| `--headless WxH` | off (window) | Run without a window, the UI rendered off screen at WxH points; excludes `--window` |
| `--window` | off | Open the desktop window also with `--launched-by-qaros` |
| `--graphics-backend vulkan\|metal\|dx12\|gl` | Rerun's choice | wgpu backend; the Vulkan path and `--vulkan-device` need `vulkan` |
| `--vulkan-device SELECTOR` | off | Create the Vulkan device on this GPU: `default`, `discrete`, `name:TEXT`, `uuid:HEX`, `luid:HEX`, `index:N` |
| `-h`, `--help` | | Print the usage |

**Image sink** (no QAROS; for trying the viewer and comparing paths)

| Option | Default | What it does |
| --- | --- | --- |
| `--out DIR` | `out/headset` | Output directory, one `user-<n>/` per user |
| `--eye-resolution WxH` | `960x1000` | Per-eye resolution (QAROS streams take theirs from the negotiated layout) |
| `--write-every N` | 20 | Write every N-th shown frame of each user |
| `--raw-depth` | off | Also write depth as raw little-endian `f32` (`frame_N_depth.f32`) |
| `--motion circle\|orbit\|still` | `circle` | Scripted head motion: `circle` walks around the world center looking level, `orbit` walks the same circle looking at the center |
| `--radius METERS` | 1.6 | Radius of the circle walk |
| `--eye-height METERS` | 1.6 | Eye height of the simulated heads |
| `--period SECONDS` | 4 | Time for one circle, counted at 60 frames per second |
| `--fov DEGREES` | headset-like | A centered field of view this many degrees wide, with square pixels |
| `--users N` | 1 | Simulated users, spread around the circle |
| `--join-interval N` | 30 | User k joins N viewer frames after user k-1 |

### `log-test-scene`

`log-test-scene [steps] [url] [heavy entities]`: logs `steps` steps of the test scene (default 200) to `url` (default `rerun+http://127.0.0.1:9876/proxy`), with `heavy entities` extra small point clouds (default 0) for a scene that updates slowly.

### Environment variables

| Variable | Default | What it does |
| --- | --- | --- |
| `QAROS_DLL` | the default lookup (the installed QAROS) | The `qar-streaming-c` library; `--qaros-library` wins over it |
| `QAROS_RUNTIME_DIR` | the library's directory | Where the QAROS runtime binaries are |
| `QAROS_STORAGE_DIR` | the library's default (`%LOCALAPPDATA%\Quaternar\QarOS` on Windows) | Where the QAROS runtime keeps its state |
| `RUST_LOG` | Rerun's default | Log filter, for example `qaros_rerun=debug` |
| `QAROS_RERUN_PERF_LOG` | off | Path of a CSV with the timing of each phase of a viewer frame and each QAROS call |

Rerun's own environment variables apply as in the stock viewer.

### For development only

The viewer also accepts the following, for measuring and diagnosing the viewer itself. They are not user settings; the source describes each.

- Options: `--begin-frame poll` (the only accepted value), `--gpu-priority high|normal`, `--vulkan-extension NAME`, `--image-targets cpu|gpu`, `--depth-scale-down N`.
- Environment variables: `QAROS_RERUN_BEGIN_KICK`, `QAROS_RERUN_PERF_GPU_SYNC`, `QAROS_RERUN_SCENE_DELAY_MS`, `QAROS_RERUN_EYE_REUSE`, `QAROS_RERUN_MEMORY_BLOCK_MIB`, `QAROS_RERUN_POOL_GRACE`. An invalid value of `QAROS_RERUN_EYE_REUSE`, `QAROS_RERUN_MEMORY_BLOCK_MIB` or `QAROS_RERUN_SCENE_DELAY_MS` stops the viewer at start with an error naming the variable.

[QAROS-REPO]: https://github.com/Quaternar/qaros "QAROS: the C API, its Rust binding and the documentation"
[QAROS-DOCS]: https://github.com/Quaternar/qaros/tree/b1338df/docs/website/docs "QAROS documentation"
[GLOSSARY]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/glossary.md "QAROS glossary"
[VISUALIZER]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/operator-guide/running-the-hub.md#the-visualizer "QAROS operator guide, Running the Hub: the visualizer"
[CONCEPTS]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/concepts.md "QAROS developer guide, Concepts: target app, player, visualizer"
[PEERS]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/peers.mdx "QAROS developer guide, Working with peers"
[APP-VOLUMES]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/app-volumes.mdx "QAROS developer guide, App volumes"
[GUI-PANELS]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/gui-panels.mdx "QAROS developer guide, GUI panels"
[INSTALLATION]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/operator-guide/installation.md "QAROS operator guide, Installation: artifacts, firewall ports, data folders"
[INSTALLATION-SDK]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/operator-guide/installation.md#the-sdk-package-for-integrators-this-repository "QAROS operator guide, Installation: the runtime ZIP and its bin folder"
[USER-START]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/user-guide/getting-started.md "QAROS user guide, Getting started: joining with a pairing code"
[SOURCE-APPS]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/operator-guide/source-applications.md "QAROS operator guide, Source Applications: adding a QAROS API app"
[MIXER]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/operator-guide/the-mixer.md "QAROS operator guide, The mixer: every stream is spatially masked to its volume's oriented bounding box"
