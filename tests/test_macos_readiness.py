import importlib.util
import io
from contextlib import redirect_stdout
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("readiness", Path(__file__).parents[1] / "scripts/macos-readiness.py")
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
        with patch.dict(readiness.os.environ, {"BAKERY_RUNNER_METADATA": '{"cpu":6,"memory_gb":12}'}), \
             patch.object(readiness.os, "cpu_count", return_value=4), \
             patch.object(readiness, "run", side_effect=[str(12 * 1024**3), "boot", "os", "xcode"]), \
             patch.object(readiness, "diagnostics"), \
             patch.object(readiness, "warm_simulators") as warm:
            with self.assertRaisesRegex(RuntimeError, "resources differ"):
                readiness.main()
        warm.assert_not_called()


if __name__ == "__main__":
    unittest.main()
