"""Shared launcher for the demos.

Each `demos/<name>/run.py` describes one demo and hands it to `run`. The first start fetches
the Rerun example at the pinned tag, lets it download its dataset and records it once into
`demos/.cache/<name>/recording.rrd`; every start after that only starts the viewer and plays
the cached recording in a loop, so a restart needs no network and takes seconds.
"""

from __future__ import annotations

import argparse
import contextlib
import os
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path

import rerun as rr
import rerun.blueprint as rrb

RERUN_TAG = "0.40.0"
RAW_URL = f"https://raw.githubusercontent.com/rerun-io/rerun/{RERUN_TAG}"
LFS_URL = f"https://media.githubusercontent.com/media/rerun-io/rerun/{RERUN_TAG}"
LFS_POINTER_PREFIX = b"version https://git-lfs.github.com/spec/v1"

DEMOS_DIRECTORY = Path(__file__).resolve().parent
REPOSITORY_ROOT = DEMOS_DIRECTORY.parent
CACHE_DIRECTORY = DEMOS_DIRECTORY / ".cache"
DEFAULT_PORT = 9876
VIEWER_START_TIMEOUT_SECONDS = 60.0
# A viewer stopped by Ctrl+C leaves QAROS within about a second, then ends itself.
VIEWER_STOP_GRACE_SECONDS = 5.0
PID_FILE_PREFIX = "viewer-"
LATEST_LOG_NAME = "viewer.log"
KEPT_INSTANCE_LOGS = 20
# Rerun's own chatter stays at warnings; the QAROS side (onboarding, targets found, senders,
# the Vulkan device, the session teardown) is what a viewer.log is read for, also when QAROS
# starts the demo and no environment can be set.
DEFAULT_VIEWER_LOG_FILTER = "warn"
VIEWER_LOG_DIRECTIVES = (
    "qaros_rerun=info",
    "qaros_sink=info",
    "qaros=info",
    "vulkan_device=info",
)


@dataclass(frozen=True)
class Placement:
    """Where the demo's data sits in the room: the viewer's --center, --origin, --scale, --yaw."""

    center: tuple[float, float, float]
    origin: tuple[float, float, float] = (0.0, 1.1, -1.2)
    scale: float = 1.0
    yaw_degrees: float = 0.0

    def viewer_arguments(self) -> list[str]:
        return [
            "--center",
            ",".join(f"{value:g}" for value in self.center),
            "--origin",
            ",".join(f"{value:g}" for value in self.origin),
            "--scale",
            f"{self.scale:g}",
            "--yaw",
            f"{self.yaw_degrees:g}",
        ]


@dataclass(frozen=True)
class Demo:
    name: str
    title: str
    application_id: str
    """The example's own application id: the replayed recording keeps it, and the viewer
    applies a blueprint only to recordings of the same application."""
    example_directory: str
    """Directory of the example inside the Rerun repository, e.g. `examples/python/rgbd`."""
    example_files: Sequence[str]
    """Files to fetch, relative to the Rerun repository root."""
    example_arguments: Sequence[str]
    """Arguments after the Python interpreter, run inside `example_directory`."""
    placement: Placement
    blueprint: Callable[[], rrb.Blueprint]


def demo_time_panel(
    timeline: str, playback_speed: float = 1.0, frames_per_second: float | None = None
) -> rrb.TimePanel:
    """Plays from the start and loops forever, with only the slim time bar visible.

    `frames_per_second` paces sequence timelines, which have no seconds of their own."""
    return rrb.TimePanel(
        state="collapsed",
        timeline=timeline,
        playback_speed=playback_speed,
        fps=frames_per_second,
        play_state="playing",
        loop_mode="all",
    )


def demo_blueprint(*parts: rrb.BlueprintPart) -> rrb.Blueprint:
    """A blueprint without the side panels, so only the views show."""
    return rrb.Blueprint(
        *parts,
        rrb.BlueprintPanel(state="hidden"),
        rrb.SelectionPanel(state="hidden"),
        auto_views=False,
    )


def parse_arguments(demo: Demo) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=f"{demo.title}: start the headset viewer and play the demo in a loop.",
        epilog="Arguments after `--` go to the viewer, e.g. -- --sink qaros --pairing-code CODE",
    )
    parser.add_argument("--viewer", type=Path, help="viewer binary (default: target/release/qaros-rerun)")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help="viewer gRPC port (default 9876)")
    parser.add_argument("--record", action="store_true", help="record the demo again from the dataset")
    parser.add_argument("--record-only", action="store_true", help="prepare the recording and exit")
    parser.add_argument("viewer_arguments", nargs=argparse.REMAINDER)
    arguments = parser.parse_args()
    if arguments.viewer_arguments[:1] == ["--"]:
        arguments.viewer_arguments = arguments.viewer_arguments[1:]
    return arguments


class TerminateRequested(Exception):
    """A terminate request (task manager, kill) reached the launcher and not the viewer."""


def stop_on_terminate(_signal_number: int, _frame: object) -> None:
    raise TerminateRequested


def run(demo: Demo) -> None:
    arguments = parse_arguments(demo)
    # Progress lines must show up at once also when a wrapper script redirects them to a file.
    sys.stdout.reconfigure(line_buffering=True)
    # A terminate request (task manager, kill) stops the viewer, as Ctrl+C does.
    signal.signal(signal.SIGTERM, stop_on_terminate)
    demo_cache = CACHE_DIRECTORY / demo.name
    recording_path = demo_cache / "recording.rrd"
    # A blueprint mistake should stop the launcher before anything starts, not silently fall back
    # to Rerun's default layout.
    rr.set_strict_mode(True)
    blueprint = demo.blueprint()

    if arguments.record or not recording_path.exists():
        record_demo(demo, recording_path)
    if arguments.record_only:
        print(f"{demo.title}: recording ready at {recording_path}")
        return

    viewer_binary = find_viewer_binary(arguments.viewer)
    stop_orphaned_viewers(demo_cache)
    port = arguments.port if port_is_free(arguments.port) else free_port()
    viewer_command = [
        str(viewer_binary),
        "--port",
        str(port),
        *demo.placement.viewer_arguments(),
        # QAROS gives an app that asks for an existing volume name that volume and its one render
        # stream, so each demo has its own: two demos at once no longer fight over one stream.
        # A --volume-name after `--` comes later and wins.
        "--volume-name",
        f"rerun-{demo.name}",
        # The label people see on the volume; the common name above is the volume's identity.
        "--display-name",
        f"Rerun {demo.title.lower()}",
        *arguments.viewer_arguments,
    ]
    viewer_log_path = instance_log_path(demo_cache)
    print(f"{demo.title}: starting the viewer on port {port} (log: {viewer_log_path})")
    with viewer_log_path.open("w", encoding="utf-8") as viewer_log:
        point_latest_log_at(viewer_log_path, demo_cache / LATEST_LOG_NAME)
        viewer = subprocess.Popen(
            viewer_command,
            cwd=REPOSITORY_ROOT,
            stdout=viewer_log,
            stderr=subprocess.STDOUT,
            env={**os.environ, "RUST_LOG": viewer_log_filter(os.environ.get("RUST_LOG"))},
        )
        pid_path = demo_cache / f"{PID_FILE_PREFIX}{os.getpid()}.pid"
        pid_path.write_text(f"{viewer.pid}\n", encoding="utf-8")
        try:
            wait_for_viewer(viewer, port, viewer_log_path)
            play_recording(demo.application_id, recording_path, blueprint, port)
            print(f"{demo.title}: playing in a loop. Close the viewer or press Ctrl+C to stop.")
            viewer.wait()
        except KeyboardInterrupt:
            give_the_viewer_time_to_stop(viewer)
        except TerminateRequested:
            pass
        finally:
            stop_process(viewer)
            pid_path.unlink(missing_ok=True)


def record_demo(demo: Demo, recording_path: Path) -> None:
    example_root = CACHE_DIRECTORY / f"rerun-{RERUN_TAG}"
    for relative_path in demo.example_files:
        fetch_example_file(relative_path, example_root / relative_path)

    recording_path.parent.mkdir(parents=True, exist_ok=True)
    raw_path = recording_path.with_suffix(".raw.rrd")
    print(f"{demo.title}: recording once from the dataset (the first start downloads it)")
    example_command = [sys.executable, *demo.example_arguments, "--save", str(raw_path)]
    completed = subprocess.run(example_command, cwd=example_root / demo.example_directory, check=False)
    if completed.returncode != 0 or not raw_path.exists():
        sys.exit(f"{demo.title}: the example failed with exit code {completed.returncode}")

    # Examples log many small chunks; compacted, a replay loads in seconds instead of minutes.
    optimized_path = recording_path.with_suffix(".optimized.rrd")
    optimize_command = [sys.executable, "-m", "rerun", "rrd", "optimize", str(raw_path), "-o", str(optimized_path)]
    completed = subprocess.run(optimize_command, check=False, capture_output=True, text=True)
    if completed.returncode != 0:
        sys.exit(f"{demo.title}: compacting the recording failed:\n{completed.stderr[-2000:]}")
    optimized_path.replace(recording_path)
    raw_path.unlink()


def fetch_example_file(relative_path: str, destination: Path) -> None:
    if destination.exists() and not destination.read_bytes().startswith(LFS_POINTER_PREFIX):
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    content = download(f"{RAW_URL}/{relative_path}")
    if content.startswith(LFS_POINTER_PREFIX):
        content = download(f"{LFS_URL}/{relative_path}")
    destination.write_bytes(content)


def download(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def find_viewer_binary(explicit_path: Path | None) -> Path:
    executable_name = "qaros-rerun.exe" if os.name == "nt" else "qaros-rerun"
    candidates = [explicit_path] if explicit_path else []
    if environment_path := os.environ.get("QAROS_RERUN_VIEWER"):
        candidates.append(Path(environment_path))
    candidates += [
        REPOSITORY_ROOT / "target" / "release" / executable_name,
        REPOSITORY_ROOT / "target" / "debug" / executable_name,
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    sys.exit("No viewer binary found. Build it first: cargo build --release -p qaros-rerun")


def viewer_log_filter(inherited_filter: str | None) -> str:
    """The inherited RUST_LOG (the QAROS Hub sets its own) with our crates' startup milestones
    added, unless the inherited filter already names a level for them."""
    base_filter = inherited_filter or DEFAULT_VIEWER_LOG_FILTER
    directives = [directive for directive in base_filter.split(",") if directive]
    named_targets = {directive.split("=", 1)[0] for directive in directives if "=" in directive}
    for directive in VIEWER_LOG_DIRECTIVES:
        if directive.split("=", 1)[0] not in named_targets:
            directives.append(directive)
    return ",".join(directives)


def instance_log_path(demo_cache: Path) -> Path:
    """A log per start, so a restart or a second instance keeps the evidence of the one before."""
    log_directory = demo_cache / "logs"
    log_directory.mkdir(parents=True, exist_ok=True)
    started = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    instance_logs = sorted(log_directory.glob("viewer-*.log"))
    for old_log in instance_logs[: max(0, len(instance_logs) - (KEPT_INSTANCE_LOGS - 1))]:
        with contextlib.suppress(OSError):
            old_log.unlink()
    return log_directory / f"viewer-{started}-{os.getpid()}.log"


def point_latest_log_at(instance_log: Path, latest_log: Path) -> None:
    """`viewer.log` names the newest start's log (a hard link, so it fills as the viewer writes)."""
    try:
        latest_log.unlink(missing_ok=True)
        os.link(instance_log, latest_log)
    except OSError as error:
        # On Windows a viewer that is still running holds the old link open.
        print(f"{latest_log.name} still names an earlier, running instance ({error.strerror})")


def stop_orphaned_viewers(demo_cache: Path) -> None:
    """Stops viewers whose launcher crashed or was killed, so a restart always works.

    Each launcher records its viewer in `viewer-<launcher pid>.pid`. A viewer whose launcher still
    runs is another instance of this demo and is left alone."""
    for pid_path in demo_cache.glob(f"{PID_FILE_PREFIX}*.pid"):
        launcher_pid_text = pid_path.stem.removeprefix(PID_FILE_PREFIX)
        try:
            launcher_pid = int(launcher_pid_text)
            viewer_pid = int(pid_path.read_text(encoding="utf-8").strip())
        except (OSError, ValueError) as error:
            print(f"ignoring the pid file {pid_path.name} ({error})")
            continue
        if launcher_pid != os.getpid() and process_is_launcher(launcher_pid):
            continue
        pid_path.unlink(missing_ok=True)
        stop_orphaned_viewer(viewer_pid)


def stop_orphaned_viewer(viewer_pid: int) -> None:
    # The pid may belong to another program by now; only a viewer is stopped.
    if not process_is_viewer(viewer_pid):
        return
    try:
        os.kill(viewer_pid, signal.SIGTERM)
    except OSError as error:
        print(f"could not stop the orphaned viewer (pid {viewer_pid}): {error}")
        return
    print(f"stopped a viewer whose launcher is gone (pid {viewer_pid})")
    deadline = time.monotonic() + 5.0
    while process_is_viewer(viewer_pid) and time.monotonic() < deadline:
        time.sleep(0.2)


def process_is_viewer(pid: int) -> bool:
    return "qaros-rerun" in process_name(pid)


def process_is_launcher(pid: int) -> bool:
    return "python" in process_name(pid).lower()


def process_name(pid: int) -> str:
    if os.name == "nt":
        listing_command = ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"]
    else:
        listing_command = ["ps", "-p", str(pid), "-o", "comm="]
    listing = subprocess.run(listing_command, capture_output=True, text=True, check=False)
    return listing.stdout


def port_is_free(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        return probe.connect_ex(("127.0.0.1", port)) != 0


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


def wait_for_viewer(viewer: subprocess.Popen[bytes], port: int, viewer_log_path: Path) -> None:
    deadline = time.monotonic() + VIEWER_START_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if viewer.poll() is not None:
            log_tail = viewer_log_path.read_text(encoding="utf-8", errors="replace")[-2000:]
            sys.exit(f"The viewer exited with code {viewer.returncode}:\n{log_tail}")
        if not port_is_free(port):
            return
        time.sleep(0.2)
    stop_process(viewer)
    sys.exit(f"The viewer did not open port {port} within {VIEWER_START_TIMEOUT_SECONDS:g} s")


def play_recording(application_id: str, recording_path: Path, blueprint: rrb.Blueprint, port: int) -> None:
    recording = rr.RecordingStream(application_id)
    recording.connect_grpc(f"rerun+http://127.0.0.1:{port}/proxy")
    # The recorded example carries its own blueprint. Ours goes first so the layout is right while
    # the data loads, and again last so it wins over the recorded one.
    recording.send_blueprint(blueprint, make_active=True, make_default=True)
    recording.log_file_from_path(recording_path)
    recording.flush()
    recording.send_blueprint(blueprint, make_active=True, make_default=True)
    recording.flush()


def give_the_viewer_time_to_stop(viewer: subprocess.Popen[bytes]) -> None:
    """The console delivers Ctrl+C to the viewer as well: it leaves QAROS and ends itself, and
    terminating it first would cut that teardown short. A second Ctrl+C stops the wait."""
    with contextlib.suppress(subprocess.TimeoutExpired, KeyboardInterrupt):
        viewer.wait(timeout=VIEWER_STOP_GRACE_SECONDS)


def stop_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=10)
        return
    process.kill()
