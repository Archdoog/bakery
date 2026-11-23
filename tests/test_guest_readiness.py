import importlib.util
import io
from contextlib import redirect_stdout
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("readiness", Path(__file__).parents[1] / "scripts/guest-readiness.py")
readiness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(readiness)


class ReadinessTests(unittest.TestCase):
    def setUp(self):
        self.output = redirect_stdout(io.StringIO())
        self.output.__enter__()
        self.addCleanup(self.output.__exit__, None, None, None)

    def wait(self, loads, timeout=900):
        clock = [0]

        def sleep(seconds):
            clock[0] += seconds

        with patch.object(readiness.os, "cpu_count", return_value=4), \
             patch.object(readiness.os, "getloadavg", side_effect=[(load, 0, 0) for load in loads]), \
             patch.object(readiness.time, "monotonic", side_effect=lambda: clock[0]), \
             patch.object(readiness.time, "sleep", side_effect=sleep):
            readiness.wait_for_host(timeout)
        return clock[0]

    def test_load_spike_restarts_stability_window(self):
        self.assertEqual(self.wait([450, 8, 7, 300, 8, 7, 6]), 30)

    def test_cold_clone_can_recover_after_ten_minutes(self):
        self.assertEqual(self.wait([450] * 121 + [8, 7, 6]), 615)

    def test_overload_fails_at_deadline(self):
        with self.assertRaisesRegex(TimeoutError, "12 seconds"):
            self.wait([450] * 4, timeout=12)

    def test_initializes_only_ios_devices_and_leaves_them_shutdown(self):
        devices = '{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-27-2":[{"name":"iPhone","udid":"phone","state":"Shutdown"}],"com.apple.CoreSimulator.SimRuntime.watchOS-27-2":[{"udid":"watch"}]}}'
        with patch.object(readiness, "run", side_effect=[devices, "", "", ""]) as run:
            readiness.warm_simulators()
        self.assertEqual([call.args for call in run.call_args_list[1:]], [
            ("xcrun", "simctl", "boot", "phone"),
            ("xcrun", "simctl", "bootstatus", "phone", "-b"),
            ("xcrun", "simctl", "shutdown", "phone"),
        ])

    def test_failed_boot_completion_still_shuts_down_simulator(self):
        devices = '{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-27-2":[{"name":"iPhone","udid":"phone","state":"Booted"}]}}'
        with patch.object(readiness, "run", side_effect=[devices, subprocess.TimeoutExpired("bootstatus", 180), ""]) as run:
            with self.assertRaises(subprocess.TimeoutExpired):
                readiness.warm_simulators()
        self.assertEqual(run.call_args.args, ("xcrun", "simctl", "shutdown", "phone"))

    def test_resource_drift_prevents_simulator_initialization(self):
        with patch.object(readiness.sys, "platform", "darwin"), \
             patch.dict(readiness.os.environ, {"BAKERY_RUNNER_METADATA": '{"cpu":6,"memory_gb":12}'}), \
             patch.object(readiness.os, "cpu_count", return_value=4), \
             patch.object(readiness, "run", side_effect=[str(12 * 1024**3), "boot", "xcode"]), \
             patch.object(readiness, "diagnostics"), \
             patch.object(readiness, "warm_simulators") as warm:
            with self.assertRaisesRegex(RuntimeError, "CPU count differs"):
                readiness.main()
        warm.assert_not_called()

    def test_linux_waits_for_cloud_init_and_systemd(self):
        with patch.object(readiness.shutil, "which", return_value="/usr/bin/cloud-init"), \
             patch.object(readiness, "run", side_effect=["done", "running"]) as run:
            readiness.wait_for_linux_boot()
        self.assertEqual([call.args for call in run.call_args_list], [
            ("cloud-init", "status", "--wait"), ("systemctl", "is-system-running", "--wait")])
        self.assertTrue(all(call.kwargs["timeout"] == 900 for call in run.call_args_list))

    def test_missing_cloud_init_still_checks_systemd(self):
        with patch.object(readiness.shutil, "which", return_value=None), \
             patch.object(readiness, "run", return_value="running") as run:
            readiness.wait_for_linux_boot()
        run.assert_called_once_with("systemctl", "is-system-running", "--wait", timeout=900)

    def test_cloud_init_failure_stops_before_systemd(self):
        with patch.object(readiness.shutil, "which", return_value="/usr/bin/cloud-init"), \
             patch.object(readiness, "run", side_effect=subprocess.CalledProcessError(1, "cloud-init")) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                readiness.wait_for_linux_boot()
        self.assertEqual(run.call_count, 1)

    def test_failed_systemd_startup_blocks_load_acceptance(self):
        with patch.object(readiness.sys, "platform", "linux"), \
             patch.dict(readiness.os.environ, {"BAKERY_RUNNER_METADATA": '{}'}), \
             patch.object(readiness.Path, "read_text", return_value="kernel metrics"), \
             patch.object(readiness, "diagnostics"), \
             patch.object(readiness, "wait_for_linux_boot", side_effect=subprocess.CalledProcessError(1, "systemctl")), \
             patch.object(readiness, "wait_for_host") as host:
            with self.assertRaises(subprocess.CalledProcessError):
                readiness.main()
        host.assert_not_called()

    def test_linux_boot_and_admission_phases(self):
        for boot_only in (True, False):
            with self.subTest(boot_only=boot_only), \
                 patch.object(readiness.sys, "platform", "linux"), \
                 patch.dict(readiness.os.environ, {"BAKERY_RUNNER_METADATA": '{"cpu":4,"memory_gb":12}'}), \
                 patch.object(readiness.os, "cpu_count", return_value=4), \
                 patch.object(readiness.Path, "read_text", return_value="kernel metrics"), \
                 patch.object(readiness, "run") as run, \
                 patch.object(readiness, "diagnostics"), \
                 patch.object(readiness, "wait_for_linux_boot") as boot, \
                 patch.object(readiness, "wait_for_host") as host:
                readiness.main(boot_only=boot_only)
            run.assert_not_called()
            boot.assert_called_once()
            self.assertEqual(host.call_count, 0 if boot_only else 1)


if __name__ == "__main__":
    unittest.main()
