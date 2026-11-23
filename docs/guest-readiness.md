# Guest readiness

Both platforms check readiness before runner registration.
Each guest must report the configured CPU count.
macOS also checks physical memory against the configured allocation.
Linux records `/proc/meminfo`, which excludes kernel reservations from usable memory.

macOS initializes the selected iOS simulators.
Ubuntu waits for `cloud-init status --wait` when cloud-init is installed.
It then waits for `systemctl is-system-running --wait`.

Each Linux boot check has a fifteen-minute limit.
A failed cloud-init run or a degraded systemd state stops registration.
Ubuntu checks boot completion before installing runner dependencies and repeats readiness before registration.

The startup log records failed services. A guest without cloud-init still requires systemd readiness.

Both platforms require three load samples at or below twice the guest CPU count.
Samples occur five seconds apart. The host recovery limit is fifteen minutes.
A timeout stops provisioning. The watcher can replace the failed clone on its next attempt.

`~/bakery-diagnostics/startup.log` records image identity, configured resources, actual resources, boot information, and resource diagnostics.
The log includes the image fingerprint and Bakery package version.
Ephemeral replacement archives this directory with runner diagnostics under the host's `logs/` directory.
A failed provisioning attempt retains its VM address so replacement can archive its startup log.

Install the updated Bakery binary and scripts on each host before recycling idle runners.
Rebuild the macOS golden to add bake initialization. Ubuntu needs no re-bake for these provisioning checks.
The macOS golden also retains `~/bakery-diagnostics/bake.log`.
App installation, simulator test preferences, and backend fixtures remain workflow responsibilities.
