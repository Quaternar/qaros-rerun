# Architecture decisions

Decisions behind qaros-rerun. The numbers are not contiguous: only the decisions about this repository are published here. Decisions about the Rust binding itself belong to the binding in [Quaternar/qaros](https://github.com/Quaternar/qaros). Superseded decisions stay, with a pointer to their successor. Each ADR cites repository paths and public sources (the QAROS header and docs at [github.com/Quaternar/qaros](https://github.com/Quaternar/qaros), Rerun's source, and the Khronos, Microsoft and Rust documentation), and states measurements in place.

| ID | Title | Status | Date | Summary |
| --- | --- | --- | --- | --- |
| [ADR-0011](<ADR-0011 Integrate Inside A Rerun Viewer Build Pinned To 0.40.0.md>) | Inside a Rerun viewer build pinned to 0.40.0 | accepted | 2026-10-05 | Separate viewer binary reusing Rerun's store, time and 3D view |
| [ADR-0012](<ADR-0012 Patched re_view_spatial Observer Hook.md>) | Patched `re_view_spatial` observer hook | accepted | 2026-10-05 | Vendored crate hands 3D draw data to an observer |
| [ADR-0013](<ADR-0013 CPU Readback Frame Path First.md>) | CPU readback frame path first | superseded by ADR-0026 | 2026-10-05 | Screenshot color plus picking depth readback; cannot reach headset-resolution rates |
| [ADR-0014](<ADR-0014 Off Axis Eye Projection Through Rerun Symmetric Perspective.md>) | Off-axis projection via symmetric perspective | accepted | 2026-10-05 | `xr-math` decomposition proven equal to the off-axis matrix |
| [ADR-0015](<ADR-0015 Forward Normalized Depth And sRGB Color Conventions.md>) | Forward normalized depth, sRGB color | accepted | 2026-10-05 | Finite far plane, `depth_scale` 1, BGRA8 premultiplied alpha (ADR-0050); switchable |
| [ADR-0016](<ADR-0016 FrameSink Seam With A PNG Image Sink.md>) | FrameSink seam with a PNG image sink | accepted | 2026-10-05 | One seam for every output, with a GPU variant |
| [ADR-0017](<ADR-0017 QAROS Sink Owned On The UI Thread.md>) | QAROS sink owned on the UI thread | accepted | 2026-10-05 | Sink lent to the UI pass; eyes submitted after it, window-independent |
| [ADR-0019](<ADR-0019 2D Views As URI Panels Through A Local Panel Server.md>) | 2D views as URI panels | accepted | 2026-10-05 | Views cropped from the UI, served as PNG pages to GUI panels |
| [ADR-0020](<ADR-0020 One Render Sender Per User With A Render Policy.md>) | One render sender per user | accepted | 2026-10-06 | `UserRoster`, `RenderPolicy` (`--max-users 2`), both paths |
| [ADR-0022](<ADR-0022 Frame Keys And A Frame Stamp For Pose Pairing.md>) | Frame keys and a frame stamp | accepted | 2026-10-06 | `FrameKey` per user and frame; stamp decoded by the stub |
| [ADR-0025](<ADR-0025 Application Created Vulkan Device With Win32 Interop Extensions.md>) | App-created Vulkan device with Win32 extensions | accepted | 2026-10-06 | `vulkan-device` crate: picker, extra extensions, raw handles, image wrap |
| [ADR-0026](<ADR-0026 QAROS Native Vulkan Render Sender As The GPU Path.md>) | QAROS native Vulkan sender as the GPU path | accepted | 2026-10-06 | Eyes rendered into sender images |
| [ADR-0029](<ADR-0029 Vulkan Frame Hand Off Through Raw Batches Chained To wgpu.md>) | Vulkan hand-off through raw batches | accepted | 2026-10-08 | Acquire, wgpu render, release batches chained by semaphores; show without fence wait |
| [ADR-0031](<ADR-0031 Patched re_renderer Keeps Picking Depth On The GPU.md>) | Patched `re_renderer` keeps depth on the GPU | accepted | 2026-10-08 | Picking depth sampleable, no readback; second vendored crate |
| [ADR-0032](<ADR-0032 Lower Resolution Depth Follows The Sender.md>) | Lower-resolution depth follows the sender | accepted | 2026-10-08 | Nearest depth per footprint in a separate pass |
| [ADR-0035](<ADR-0035 Launched By QAROS Contract.md>) | Launched-by-QAROS contract of `b1338df` | accepted | 2026-10-13 | JSON invite on stdin, never stored, stop on EOF inside QAROS's 1.3 s, exit without DLL unload code |
| [ADR-0036](<ADR-0036 Target Discovery From The Peer List.md>) | Targets from the peer list | accepted | 2026-10-13 | Running target apps, full list re-read after updates and every 2 s |
| [ADR-0037](<ADR-0037 Vulkan Device After Onboarding On The Session Adapter.md>) | Device after onboarding on the session's adapter | accepted | 2026-10-13 | Discrete-GPU fallback; `--vulkan-device` override |
| [ADR-0038](<ADR-0038 App Content Pose Space By Default.md>) | App content pose space by default | accepted | 2026-10-13 | Near and far handed back in app units |
| [ADR-0039](<ADR-0039 Polled try_begin_frame By Default.md>) | Polled `try_begin_frame` by default | accepted | 2026-10-13 | One begin mechanism on both paths; a sender goes once its begin-frame wait ends, at most 500 ms at Stop |
| [ADR-0040](<ADR-0040 Render Sender Creation Timeout With Retry.md>) | Sender creation timeout with retry | accepted | 2026-10-13 | Background creations with a 10 s timeout, hedged across candidates; unanswered targets held back for 10 min and retried once a minute |
| [ADR-0044](<ADR-0044 Headless Loop Without Sleep While Streaming.md>) | Headless loop without sleep while streaming | accepted | 2026-10-13 | Next frame at once while a headset streams; idle passes wait for egui's schedule |
| [ADR-0045](<ADR-0045 re_renderer Pool Grace Of Four Frames.md>) | `re_renderer` pool grace of four frames | accepted | 2026-10-13 | Chosen from creation counts; widens the vendored patch |
| [ADR-0046](<ADR-0046 Headless Viewer Hides Its Panels.md>) | Headless viewer hides its panels | accepted | 2026-10-13 | Top, blueprint, selection and time panels hidden headless only |
| [ADR-0047](<ADR-0047 QAROS Graphics Default Per Platform.md>) | QAROS graphics default per platform | accepted | 2026-10-13 | Vulkan on Windows, CPU elsewhere, for `--sink qaros` only |
| [ADR-0049](<ADR-0049 Panels Posed By Their Top Edge Midpoint Facing The Viewer.md>) | Panels by their top-edge midpoint, facing the viewer | accepted | 2026-10-14 | Half a turn about the volume's up axis; panel URL named by the LAN address |
| [ADR-0050](<ADR-0050 Premultiplied Alpha On Both Paths.md>) | Premultiplied alpha on both paths | accepted | 2026-10-14 | OpenXR's default; the documentation leaves the mixer's convention open |
| [ADR-0057](<ADR-0057 Eye Output Resources Kept Across Frames.md>) | Eye output resources kept across frames | accepted | 2026-10-19 | Uniforms by content and one composite target per user and eye; `QAROS_RERUN_EYE_REUSE=0` only for diagnosis |
| [ADR-0058](<ADR-0058 Decoupled Eye Rendering On A Render Thread With A Second re_renderer Context.md>) | Decoupled eye rendering on a render thread | proposed | 2026-10-19 | `--decoupled-render`: second `re_renderer` context on the shared device draws the last scene |
| [ADR-0059](<ADR-0059 Retained Scenes As Reference Counted Draw Data Published After before_submit.md>) | Retained scenes as reference-counted draw data | proposed | 2026-10-19 | `Arc` copies published after `before_submit`; clones own their drawables |
| [ADR-0060](<ADR-0060 With Decoupled Rendering The Sink And The QAROS Session Live On The Render Thread.md>) | Sink and QAROS session on the render thread | proposed | 2026-10-19 | Whole sink setup and panel hook built and dropped there |
| [ADR-0061](<ADR-0061 A Process Wide Queue Submission Lock.md>) | Process-wide queue submission lock | proposed | 2026-10-19 | Raw Vulkan submits never interleave with another thread's |
| [ADR-0065](<ADR-0065 Frames That Keep Failing For A Second Release The User.md>) | Frames failing for a second release the user | proposed | 2026-10-20 | 1 s and 10 failures in a row; CPU shows count; an unresolved app volume is not ready, not failed |
