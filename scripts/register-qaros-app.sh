#!/usr/bin/env bash
# Registers a hand-built qaros-rerun viewer with QAROS, for this user (Linux).
#
# Writes one record to the per-user QAROS app registry, apps.json in the QAROS user config
# folder ($XDG_CONFIG_HOME/quaternar, else ~/.config/quaternar). The QAROS Visualizer's Sources
# catalog then shows the Rerun tile as Detected and launches this executable. The record has
# source "local": it wins over a viewer QAROS downloaded for the same platform. Other records
# in the file are kept. No root needed. Needs python3 (and cargo, unless
# --capi-header-sha256 is given).
#
# Usage:
#   cargo build --release -p qaros-rerun
#   scripts/register-qaros-app.sh [--executable PATH] [--app-version V]
#                                 [--capi-header-sha256 HEX] [--platform P]
#   scripts/register-qaros-app.sh --unregister [--platform P]
#
# The Windows counterpart is register-qaros-app.ps1, with the same record.
set -euo pipefail

app="qaros-rerun"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

executable=""
app_version=""
capi_header_sha256=""
platform=""
unregister=0

usage() { sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --executable) executable="${2:?--executable needs a path}"; shift 2 ;;
        --app-version) app_version="${2:?--app-version needs a value}"; shift 2 ;;
        --capi-header-sha256) capi_header_sha256="${2:?--capi-header-sha256 needs a value}"; shift 2 ;;
        --platform) platform="${2:?--platform needs a value}"; shift 2 ;;
        --unregister) unregister=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

command -v python3 >/dev/null 2>&1 || { echo "python3 is required." >&2; exit 1; }

if [ -z "$platform" ]; then
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64) platform="linux-x64" ;;
        Linux-aarch64|Linux-arm64) platform="linux-arm64" ;;
        Darwin-arm64) platform="macos-arm64" ;;
        Darwin-x86_64) platform="macos-x64" ;;
        *) echo "Unsupported platform: $(uname -s) $(uname -m)" >&2; exit 1 ;;
    esac
fi

if [ -n "${XDG_CONFIG_HOME:-}" ]; then
    config_dir="$XDG_CONFIG_HOME/quaternar"
else
    config_dir="${HOME:?HOME is not set}/.config/quaternar"
fi
registry="$config_dir/apps.json"

if [ "$unregister" -eq 0 ]; then
    [ -n "$executable" ] || executable="$repo_root/target/release/$app"
    if [ ! -f "$executable" ]; then
        echo "$executable does not exist. Build the viewer first (cargo build --release -p qaros-rerun) or pass --executable." >&2
        exit 1
    fi
    if [ ! -x "$executable" ]; then
        echo "$executable is not executable (chmod +x it)." >&2
        exit 1
    fi
    executable="$(cd "$(dirname "$executable")" && pwd)/$(basename "$executable")"

    if [ -z "$app_version" ]; then
        # The full version, by the rules of crates/build_version.rs and scripts/version.ps1.
        planned="$(tr -d '[:space:]' < "$repo_root/VERSION")"
        tag="$(git -C "$repo_root" tag --points-at HEAD --list 'v[0-9]*' 2>/dev/null | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$' | head -n 1 || true)"
        if [ -n "$tag" ]; then
            app_version="${tag#v}"
        elif commit="$(git -C "$repo_root" rev-parse --short=8 HEAD 2>/dev/null)"; then
            if last_tag="$(git -C "$repo_root" describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null)"; then
                count="$(git -C "$repo_root" rev-list --count "$last_tag..HEAD")"
            else
                count="$(git -C "$repo_root" rev-list --count HEAD)"
            fi
            app_version="$planned-dev.$count+$commit"
        else
            app_version="$planned-dev.0+unknown"
        fi
        case "$app_version" in
            "$planned"|"$planned"-*) ;;
            *) echo "Version $app_version does not match VERSION ($planned)." >&2; exit 1 ;;
        esac
    fi

    if [ -z "$capi_header_sha256" ]; then
        command -v cargo >/dev/null 2>&1 || { echo "cargo is required to compute the C API header hash (or pass --capi-header-sha256)." >&2; exit 1; }
        # qar_streaming.h from the qaros revision pinned in Cargo.toml, hashed with CRLF read as
        # LF (the same rule as package-release.ps1).
        case "$platform" in
            linux-x64) triple="x86_64-unknown-linux-gnu" ;;
            linux-arm64) triple="aarch64-unknown-linux-gnu" ;;
            macos-x64) triple="x86_64-apple-darwin" ;;
            macos-arm64) triple="aarch64-apple-darwin" ;;
            *) echo "Unknown platform: $platform" >&2; exit 1 ;;
        esac
        capi_header_sha256="$(cargo metadata --format-version 1 --locked --filter-platform "$triple" --manifest-path "$repo_root/Cargo.toml" | python3 -c '
import hashlib, json, os, sys
metadata = json.loads(sys.stdin.buffer.read().decode("utf-8"))
for package in metadata["packages"]:
    source = package.get("source") or ""
    if package["name"] == "qaros" and source.startswith("git+https://github.com/Quaternar/qaros"):
        checkout = package["manifest_path"]
        for _ in range(4):
            checkout = os.path.dirname(checkout)
        header = os.path.join(checkout, "qar-streaming-c", "include", "qar_streaming.h")
        if not os.path.isfile(header):
            sys.exit("The QAROS C API header is not at %s; run cargo fetch first." % header)
        with open(header, "rb") as f:
            print(hashlib.sha256(f.read().replace(b"\r\n", b"\n")).hexdigest())
        sys.exit(0)
sys.exit("The qaros package is not in cargo metadata; is it still a git dependency in Cargo.toml?")
')"
    fi
fi

APP="$app" PLATFORM="$platform" REGISTRY="$registry" UNREGISTER="$unregister" \
EXECUTABLE="$executable" APP_VERSION="$app_version" CAPI_HEADER_SHA256="$capi_header_sha256" \
python3 - <<'PYTHON'
import datetime, json, os, re, sys, tempfile

FORMAT_VERSION = 1
app, platform, path = os.environ["APP"], os.environ["PLATFORM"], os.environ["REGISTRY"]
unregister = os.environ["UNREGISTER"] == "1"

registry = {"version": FORMAT_VERSION, "apps": []}
if os.path.exists(path):
    with open(path, encoding="utf-8-sig") as f:
        text = f.read()
    if text.strip():
        try:
            existing = json.loads(text)
        except ValueError as error:
            sys.exit("%s is not valid JSON; fix or remove it. %s" % (path, error))
        if isinstance(existing.get("version"), int) and existing["version"] > FORMAT_VERSION:
            sys.exit("%s has format version %s; this script writes version %d. Update the repository."
                     % (path, existing["version"], FORMAT_VERSION))
        registry.update(existing)
        registry["version"] = FORMAT_VERSION
        registry["apps"] = registry.get("apps") or []

def is_this(record):
    return record.get("app") == app and record.get("platform") == platform and record.get("source") == "local"

others = [r for r in registry["apps"] if not is_this(r)]
removed = len(registry["apps"]) - len(others)

if unregister:
    if removed == 0:
        print("No local %s registration for %s in %s." % (app, platform, path))
        sys.exit(0)
    registry["apps"] = others
else:
    sha = os.environ["CAPI_HEADER_SHA256"].lower()
    if not re.fullmatch(r"[0-9a-f]{64}", sha):
        sys.exit("'%s' is not a SHA-256 in hex." % sha)
    record = {
        "app": app,
        "platform": platform,
        "source": "local",
        "executable": os.environ["EXECUTABLE"],
        "app_version": os.environ["APP_VERSION"],
        "c_api_header_sha256": sha,
        "registered_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }
    registry["apps"] = others + [record]

# Atomic: a temporary file in the same folder, then rename over the registry.
directory = os.path.dirname(path)
os.makedirs(directory, exist_ok=True)
fd, temporary = tempfile.mkstemp(prefix=".apps.json.", suffix=".tmp", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
        json.dump(registry, f, indent=2)
        f.write("\n")
        f.flush()
        os.fsync(f.fileno())
    os.replace(temporary, path)
except BaseException:
    if os.path.exists(temporary):
        os.unlink(temporary)
    raise

if unregister:
    print("Unregistered the local %s for %s from %s." % (app, platform, path))
else:
    print("%s %s %s (%s, local) in %s" % ("Updated" if removed else "Registered", app, record["app_version"], platform, path))
    print("  executable: %s" % record["executable"])
    print("  C API header sha256: %s" % record["c_api_header_sha256"])
PYTHON
