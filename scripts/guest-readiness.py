"""Wait for guest boot completion and host recovery before runner registration."""

import json
import os
import platform
from pathlib import Path
import shutil
import subprocess
import sys
import time


def run(*args, timeout=180):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=timeout).stdout.strip()


def warm_simulators():
    devices = json.loads(run("xcrun", "simctl", "list", "devices", "available", "-j"))["devices"]
    for runtime, entries in devices.items():
        if not runtime.startswith("com.apple.CoreSimulator.SimRuntime.iOS-"):
            continue
        for device in entries:
            udid = device["udid"]
            print(f"Initialize simulator: {device['name']} ({udid})", flush=True)
            if device["state"] != "Booted":
                run("xcrun", "simctl", "boot", udid)
            try:
                run("xcrun", "simctl", "bootstatus", udid, "-b")
            finally:
                run("xcrun", "simctl", "shutdown", udid)


def wait_for_host(timeout=900):
    limit = 2 * (os.cpu_count() or 1)
    deadline = time.monotonic() + timeout
    stable = 0
    while True:
        load = os.getloadavg()[0]
        stable = stable + 1 if load <= limit else 0
        print(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} Host load: {load:.2f}, limit {limit}, stable samples {stable}/3", flush=True)
        if stable == 3:
            return
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"Host load did not settle within {timeout} seconds.")
        time.sleep(min(5, remaining))


def wait_for_linux_boot():
    if shutil.which("cloud-init"):
        print(run("cloud-init", "status", "--wait", timeout=900), flush=True)
    else:
        print("cloud-init is not installed.", flush=True)
    print(run("systemctl", "is-system-running", "--wait", timeout=900), flush=True)


def diagnostics():
    commands = (
        (("sysctl", "vm.loadavg", "vm.swapusage"), ("memory_pressure", "-Q"),
         ("ps", "-Ao", "pcpu,pmem,rss,comm", "-r"))
        if sys.platform == "darwin" else
        (("uptime",), ("free", "-b"), ("systemctl", "--failed", "--no-pager"),
         ("ps", "-eo", "pcpu,pmem,rss,comm", "--sort=-pcpu"))
    )
    for args in commands:
        try:
            result = subprocess.run(args, capture_output=True, text=True, timeout=10)
            print(result.stdout if args[0] != "ps" else "\n".join(result.stdout.splitlines()[:9]), flush=True)
        except (OSError, subprocess.TimeoutExpired) as error:
            print(f"Cannot collect {args[0]} diagnostics: {error}", flush=True)


def main(boot_only=False):
    metadata = json.loads(os.environ.get("BAKERY_RUNNER_METADATA", "{}"))
    cpu = os.cpu_count()
    details = {"phase": "boot" if boot_only else "admission", "configured": metadata, "actual_cpu": cpu, "os": platform.platform()}
    if sys.platform == "darwin":
        memory = int(run("sysctl", "-n", "hw.memsize"))
        details.update(actual_memory_bytes=memory, boot=run("sysctl", "-n", "kern.boottime"),
                       xcode=run("xcodebuild", "-version"))
    else:
        details.update(meminfo=Path("/proc/meminfo").read_text(), uptime=Path("/proc/uptime").read_text())
    print(json.dumps(details), flush=True)
    diagnostics()
    try:
        if metadata and cpu != metadata["cpu"]:
            raise RuntimeError("Guest CPU count differs from the runner configuration. Recycle this runner.")
        if sys.platform == "darwin":
            if metadata and memory != metadata["memory_gb"] * 1024**3:
                raise RuntimeError("Guest memory differs from the runner configuration. Recycle this runner.")
            warm_simulators()
        else:
            wait_for_linux_boot()
        if not boot_only:
            wait_for_host()
    finally:
        diagnostics()


if __name__ == "__main__":
    try:
        main(boot_only=sys.argv[1:] == ["--boot-only"])
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Runner readiness failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError):
            for output in (error.stdout, error.stderr):
                if output:
                    print(output, file=sys.stderr)
        sys.exit(1)
