---
id: ADR-0096
title: No claim or replacement blocks the QAROS thread while a new panel stream's first frame is due
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0096: No claim or replacement blocks the QAROS thread while a new panel stream's first frame is due

> Amends ADR-0077 (claims are made one per sync, each after the previous panel's first frame) and ADR-0087 (a sender replacement waits for another panel's first frame).

## Context

Seen on METALENSE 2 with QAROS 0.9.0-dev.3184, builds `combined-1102b` and `combined-1102d` (viewer logs `viewer-20261102T125057Z-32564.log`, `viewer-20261102T170551Z-10672.log` and `viewer-20261103T125029Z-3304.log` of the `structure_from_motion` demo, times UTC; the log of mixer `9eac9097`, local time, UTC+1; the Windows System log):

- **Three headset drops, one signature.** Each time the mixer lost its Vulkan device (`vkQueueSubmit lost the Vulkan device ... lastSubmitAt=BrowserControl.hpp:730`), closed every panel and left the session with its headset, and the NVIDIA driver logged a channel reset (`nvlddmkm` event 153) at 11-02 12:55:36 and 12:55:41, 11-02 17:06:11 and 17:06:21, 11-03 12:50:50 and 12:50:54. The viewer's own device was never lost; its window logged `egui_wgpu: Dropped frame with error: Timeout` once a second while the GPU was held.
- **The mixer stops right after it connects a panel's new stream.** On 11-03 the mixer logged `connecting GUI panel stream a587a790... at 768x408` (the camera panel's new stream) at 13:50:49.352, its last frame statistics at 13:50:49.58, and nothing more until the device loss at 13:50:54.58. On 11-02 it logged `connecting GUI panel stream d15552ec...` at 18:06:11.149 and nothing until the loss at 18:06:21.26. In both, the timeline panel's stream had connected seconds earlier and its first frame had been copied at once (`CrossVulkanDeviceReceiver::Record` 32 ms after its connect); the camera stream got no `Record` before the loss.
- **The viewer sent that stream's first frame only after the mixer died.** `first frame of camera shown` at 12:50:54.580 (the loss at 12:50:54.580) and at 17:06:21.308 (the loss at 17:06:21.26). Between the camera claim and that frame the QAROS thread, which also shows panel frames (`QarosPanels::pump_streams`, every viewer frame), was in the next claim: `sync` claims every wanted panel in one loop, and a claim blocks for about 2 s on 3184 (Quaternar#345). The camera claim returned at 12:50:49.246 and the reprojection claim held the thread until 12:50:51.247; at 17:06:11.135 the same.
- **The same stall, survived.** On 11-03 at 12:39 (`viewer-20261103T123909Z-17024.log`) the camera stream connected in the mixer at 13:39:29.163, the mixer's frame statistics paused from 13:39:29.68 to 13:39:31.28, and the viewer's first camera frame went out at 12:39:31.155, the moment the reprojection claim returned: a 2.0 s stall that stayed under Windows' two-second GPU timeout (TDR). The 11-02 12:55 drop is the same with a size replacement in place of a claim: the camera's new stream at 12:55:36.64, then the reprojection replacement holding the thread to 12:55:38.65, the new streams had no ready begin within the retire limit, and the driver reset the mixer twice.
- **Why the mixer's GPU waits.** The mixer submits each received panel frame's copy on StereoKit's graphics queue, waiting on the receiver's ready semaphore, and then waits for it on its main thread (`BrowserControl::CopyReceivedFrameToDisplay`, `AcquireWork.Submit` and `WaitUntilDone`, `StereoKitGUI/src/Browser/BrowserControl.hpp` L587-748). The receiver's copy in turn waits on the sender's frame semaphore (`CrossVulkanDeviceReceiver::OnProcess`, `CrossDeviceTransfer.hpp` L2002-2040 in the `qar-streaming-530` source). The logs show the headset's graphics queue held from the new stream's connect until its first frame arrives; which submit holds it is QAROS's to confirm. A queue held past the TDR timeout is reset with the mixer's device in it.

The hang is QAROS's: a compositor whose graphics queue can be held by one panel sender's first frame. The viewer cannot remove it, but it decides how long its first frame takes, and that took 2 s or more only because the viewer started a blocking call right after creating the sender.

## Decision

- **A sender just put in place holds back the next blocking call until its first frame is shown.** A claim's or a replacement's sender awaits its first frame from the moment it is created; the frame clears it. While any healthy sender awaits it, for at most `FIRST_FRAME_WAIT` (3 s), `QarosPanels::sync` makes no listing and no claim, and `StreamPanels::maintain` replaces no other panel's sender. The pump keeps running every viewer frame, so the first frame goes out as soon as the mixer's receiver is ready, within a few frames of its connect instead of a claim's 2 s.
- **One claim per sync.** After a successful claim, `sync` stops; the next wanted panel is claimed at a later sync once the first frame is out. Claims of a run's panels are now at least a sync (1 s) apart plus each first frame's wait.
- **A sender no mixer connects stops holding the others back** after `FIRST_FRAME_WAIT`: a panel whose mixer is gone, or a sender whose image size changed before its first frame (it can show nothing until it is replaced), does not stall the other panels.

## Alternatives considered

- **Claim panels on another thread.** QAROS handles stay on the thread that made them, and claims, senders and gestures share the session; moving claims off the QAROS thread is a larger change than this, and Quaternar#345 is the reason claims block at all.
- **Show the first frame inside the claim.** The sender has no ready begin until the mixer has connected its receiver, about 100 ms after the claim returns; waiting for it inside the claim would block the thread on the mixer.
- **Leave it to QAROS.** Worth reporting: the mixer should not hold its graphics queue for a panel stream's first frame, nor wait for a panel copy on its main thread. Until it does, any viewer that pauses longer than the TDR timeout after a claim drops the headset.

## Consequences

- Positive: a new stream's first frame no longer waits for the next panel's claim, so the mixer's wait lasts a few frames instead of 2 s, well under the TDR timeout.
- Positive: the eye frames freeze for one claim at a time, with frames in between, rather than for every wanted panel back to back.
- Negative: panels open one sync apart; three panels take about 3 s longer to appear at a start.
- Negative: a panel whose mixer never connects delays the next claim by up to 3 s.
- Open: not yet tried in the glasses. The owner's check: run the demo, stop it, start it again within five minutes (its panels reattach Hidden and switch to new streams, the case that dropped the headset), and watch for `first frame of camera shown` within a second of the camera claim and no `nvlddmkm` event 153 in the Windows System log.

## Evidence and sources

- [SP] `crates/qaros-sink/src/stream_panels.rs` (`FIRST_FRAME_WAIT`, `awaits_a_first_frame`, `first_frame_awaited_besides`, `maintain`; tests `a_claimed_sender_holds_back_claims_until_its_first_frame_shows`, `a_sender_no_mixer_connects_stops_holding_back_claims_after_the_wait`, `a_replacement_waits_for_another_panel_s_first_frame_then_holds_back_claims_itself`).
- [PN] `crates/qaros-sink/src/panels.rs` (`QarosPanels::sync`).
- Mixer log `C:\Users\jleho\AppData\Local\Temp\quaternar\logs\qar-launcher-default\c19e8322-465a-4e57-be89-bf96d6aa0136\mixer_9eac9097-3aa2-40c3-ac5c-96a060280679_stdout.log` (11-03) and the same file under `previous-runs\20261103T092425Z` (11-02) on the Legion test machine.
- QAROS source: `StereoKitGUI/src/Browser/BrowserControl.hpp` L587-748 (`CopyReceivedFrameToDisplay`), L789-860 (`EnsureStreamReceiver`), L1058-1125 (`ReceiveStreamFrames`); `qar-streaming` `include/qar_streaming/vulkan/CrossDeviceTransfer.hpp` L1615-2040 (`CrossVulkanDeviceReceiver`).

[SP]: ../../crates/qaros-sink/src/stream_panels.rs "Stream panels"
[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
