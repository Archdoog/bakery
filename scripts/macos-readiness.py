"""Initialize iOS simulators and wait for macOS host recovery before registration."""

import json
import os
import subprocess
import sys
import time


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=180).stdout.strip()


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


def diagnostics():
    for args in (("sysctl", "vm.loadavg", "vm.swapusage"), ("memory_pressure", "-Q"),
                 ("ps", "-Ao", "pcpu,pmem,rss,comm", "-r")):
        try:
            result = subprocess.run(args, capture_output=True, text=True, timeout=10)
            print(result.stdout if args[0] != "ps" else "\n".join(result.stdout.splitlines()[:9]), flush=True)
        except (OSError, subprocess.TimeoutExpired) as error:
            print(f"Cannot collect {args[0]} diagnostics: {error}", flush=True)


def main():
    metadata = json.loads(os.environ.get("BAKERY_RUNNER_METADATA", "{}"))
    cpu = os.cpu_count()
    memory = int(run("sysctl", "-n", "hw.memsize"))
    print(json.dumps({"configured": metadata, "actual_cpu": cpu, "actual_memory_bytes": memory,
                      "boot": run("sysctl", "-n", "kern.boottime"),
                      "os": run("sw_vers"), "xcode": run("xcodebuild", "-version")}), flush=True)
    diagnostics()
    try:
        if metadata and (cpu != metadata["cpu"] or memory != metadata["memory_gb"] * 1024**3):
            raise RuntimeError("Guest resources differ from the runner configuration. Recycle this runner.")
        warm_simulators()
        wait_for_host()
    finally:
        diagnostics()


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Runner readiness failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        sys.exit(1)
