# Tests

This file defines a single end-to-end scenario sequence that exercises every `agentsandbox` subcommand in one run. It is written from the user perspective and assumes a local Linux host with a writable workspace.

## Scenario

1. Start in a clean project directory.
   - Create a new empty directory and `cd` into it.
   - Confirm that no `.agentsandbox/` directory exists yet.

2. Initialize the workspace.
   - Run `agentsandbox init`.
   - Verify that `.agentsandbox/flake.nix`, `.agentsandbox/configuration.nix`, `.agentsandbox/allowed_hosts`, and `.agentsandbox/mounts` were created.
   - Verify that `allowed_hosts` is deny-by-default and `mounts` contains only the template header.
   - Here, deny-by-default means that removing every active `allowed_hosts` entry must still block unmatched hosts rather than falling back to allow-all.

3. Inspect launcher metadata.
   - Run `agentsandbox --help`.
   - Run `agentsandbox version`.
   - Run `agentsandbox doctor`.
   - Run `agentsandbox verify`.
   - Verify that the current `verify` stub announces the planned repair commands `nixos-rebuild --repair` and `nix store verify --repair`.
   - Verify that the command list, version string, and dependency diagnostics are printed.

4. Confirm config resolution.
   - Run `agentsandbox __resolve-active-config "$PWD"`.
   - Verify that the active config dir resolves to the local `.agentsandbox/` directory.
   - Run `agentsandbox __resolve-instance "$PWD" "$PWD/.agentsandbox" default`.
   - Verify that the instance id, machine id, data dir, state dir, runtime dir, and domain name are all reported.

5. Prepare allowlist entries.
   - Run `agentsandbox allow-domain Example.COM`.
   - Run `agentsandbox allow-domain https://example.com/path`.
   - Run `agentsandbox allow-domain 'https://*.Example.COM.:8443/path'`.
   - Verify that `allowed_hosts` contains one normalized `example.com` entry and one `*.example.com` entry.
   - Run `agentsandbox unallow-domain https://EXAMPLE.com.:443/path`.
   - Verify that the exact `example.com` line is removed.

6. Prepare mounts.
   - Create two host directories, for example `alpha/` and `beta/`.
   - Run `agentsandbox mount ./alpha`.
   - Run `agentsandbox mount ./beta sandbox-beta`.
   - Verify that `mounts` contains `<host-path><TAB>alpha` and `<host-path><TAB>sandbox-beta`.
   - Run `agentsandbox mount` with no arguments.
   - Verify that the current mount list is printed.
   - Run `agentsandbox unmount ./alpha`.
   - Verify that the `alpha` entry is removed from `mounts`.

7. Build the guest system.
   - Run `agentsandbox build`.
   - Verify that the sysroot is created under `$XDG_DATA_HOME/agentsandbox/<instance-id>/sysroot`.
   - Verify that the guest top-level build exists inside the sysroot.
   - Verify that the instance `machine-id` is stable across repeated `build` runs.

8. Start the VM.
   - Run `agentsandbox up`.
   - Verify that the VM starts, the proxy sidecar starts, both virtiofs daemons start, and the transient libvirt domain is created.
   - Verify that `runtime.log` exists and that `requests.jsonl` exists.
   - Verify that the SSH port reported by `agentsandbox port 22 tcp` matches the forwarded port used by `up`.

9. Inspect the running VM.
   - Run `agentsandbox ps`.
   - Verify that the status is `running`.
   - Run `agentsandbox ssh`.
   - Verify that a shell opens inside the guest as `vscode`.
   - Run `agentsandbox exec -- uname -a`.
   - Verify that the command executes inside the guest and returns guest kernel information.
   - Run `agentsandbox logs`.
   - Verify that the guest journal is displayed.
   - Run `agentsandbox stats`.
   - Verify that CPU, memory, network I/O, block I/O, and PID columns are displayed.

10. Validate persistence and dynamic mounts inside the guest.
   - In the guest shell, verify that `/persistent/home/vscode` exists.
   - Verify that `/persistent/workspace/<workspace-name>` is present.
   - Verify that the initial workspace mount is visible in the guest under the workspace basename.
   - Verify that the `beta` mount appears under `/persistent/workspace/sandbox-beta`.

11. Validate proxy log handling.
   - Append a request record to `logs/requests.jsonl`.
   - Create a compressed archive `requests-*.jsonl.zst` and keep the active `requests.jsonl` file.
   - Run `agentsandbox proxy-logs`.
   - Verify that the archived request log series and the active log are both readable in order.

12. Validate runtime lifecycle controls.
   - Run `agentsandbox pause`.
   - Verify that `ps` shows the VM as paused.
   - Run `agentsandbox unpause`.
   - Verify that `ps` shows the VM as running again.
   - Run `agentsandbox wait`.
   - In a second terminal, stop the VM with `agentsandbox down` or `agentsandbox kill`.
   - Verify that `wait` returns only after the VM stops.

13. Validate graceful shutdown.
   - Run `agentsandbox down`.
   - Verify that the domain stops cleanly.
   - Verify that the helper PID files are cleaned up.

14. Validate forced shutdown.
   - Start the VM again with `agentsandbox up`.
   - Run `agentsandbox kill`.
   - Verify that the domain is destroyed immediately.
   - Verify that helper sockets and PID files are cleaned up.

15. Validate destroy semantics.
    - Run `agentsandbox destroy`.
    - Verify that the sysroot remains.
    - Verify that the `persistent/` tree remains.
    - Run `agentsandbox destroy -s`.
    - Verify that the sysroot is removed.
    - Verify that the `persistent/` tree remains.
    - Run `agentsandbox destroy -d`.
    - Verify that `persistent/` is removed and `sysroot/` remains.
    - Run `agentsandbox destroy -sd`.
    - Verify that the whole data dir is removed.
    - Verify that a later `agentsandbox up` reuses the persistent state.

16. Validate the remaining utility commands in the same session.
   - Run `agentsandbox port`.
   - Verify that it prints the host port for the configured guest port.
   - Run `agentsandbox port 50052 tcp`.
   - Verify that the OpenSnitch port or other forwarded port can be resolved when configured.
   - Run `agentsandbox wait` again after stopping the VM.
   - Verify that it exits cleanly when nothing is running.

## Acceptance

- Every subcommand listed in `agentsandbox --help` is executed at least once in this sequence.
- The sequence covers local config, global config resolution, guest build, VM startup, guest attachment, logs, stats, mount management, allowlist editing, proxy log tailing, lifecycle control, and cleanup.
- The sequence must pass without manual edits between steps other than the explicit host directories and log records created for the scenario.
