# Tests

This file defines a end-to-end scenario sequence that exercises every subcommand.

## Requirements

- Archtecture: x86_64
- Kernel: Linux 6.1 or later
- Volume: writable home directory

## Scenario

0. Check installation
   - Run `nix shell <repo>` where `<repo>` is the path to the clone of this repository
   - Run `agentsandbox --help` and verify that the help message is printed
   - Run `agentsandbox help` and verify that the help message is printed
   - Run `agentsandbox --version` and verify that the version string is printed
   - Run `agentsandbox version` and verify that the version string is printed
   - Run `agentsandbox doctor` and verify that `Cmd*` dependencies are available
     (we'll see other fields in the output later)

1. Using the project workspace
   - Create a new empty directory and `cd` into it
   - Confirm that no `.agentsandbox/` directory exists yet
   - Run `agentsandbox init`
   - Verify that `.agentsandbox/{flake.nix,configuration.nix,allowed_hosts,mounts}` were created
   - Run `agentsandbox doctor` and verify that `ResolvedFlakeDir` is the local `.agentsandbox/` directory
   - Run `agentsandbox doctor` and verify that `InstanceId` (starts with `<dirname>-`),
     `Instance{Data,State,Runtime}Dir` appear in the output
   - Run `agentsandbox init` again and verify that the message "init: $PWD/.agentsandbox already exists" is printed
   - Run `agentsandbox init -f` and verify that the message "init: wrote template files to $PWD/.agentsandbox" is printed
     and timestamps of the files are updated
   - Temporarily `cd` to a different directory and verify that `agentsandbox doctor` reports *different* `ResolvedFlakeDir`
   - In the temporary directory, verify that `agentsandbox doctor -w <original-dir>` reports *same* `ResolvedFlakeDir` as before
   - Run ` agentsandbox destroy -c` and verify that the local `.agentsandbox/` directory is removed

2. Using the global workspace
   - Create a new empty directory and `cd` into it
   - Confirm that no `.agentsandbox/` directory exists yet
   - Run `agentsandbox init --global`
   - Verify that `$XDG_CONFIG_HOME/agentsandbox/{flake.nix,configuration.nix,allowed_hosts,mounts}`
     were created (`$XDG_CONFIG_HOME` is typically `~/.config/`)
   - Run `agentsandbox doctor` (note that without `-g` flag) and verify that `ResolvedFlakeDir` is the
     global `$XDG_CONFIG_HOME/agentsandbox/` directory
   - Run `agentsandbox doctor` and verify that `InstanceId` (starts with `agentsandbox-`), and
     `Instance{Data,State,Runtime}Dir` appear in the output
   - Run `agentsandbox init --global` again and verify that the message "init: $XDG_CONFIG_HOME/agentsandbox already exists" is printed
   - Run `agentsandbox init --global -f` and verify that the message "init: wrote template files to $XDG_CONFIG_HOME/agentsandbox" is printed
     and timestamps of the files are updated
   - `cd` to a different directory without `.agentsandbox/` and verify that `agentsandbox doctor` reports *same* `ResolvedFlakeDir` as before
   - Run ` agentsandbox destroy -gc` and verify that the global `$XDG_CONFIG_HOME/agentsandbox/` directory is removed

3. Build the initial guest system
   - Create a new empty directory and `cd` into it
   - Run `agentsandbox init`
   - Run `agentsandbox build` and be patient as it takes a while
   - Optionally examine generated `domain.xml`, `agentsandbox ps`, and `virsh console <domain>` in other terminals
   - Finally it destroys the new domain. Example output:
```
Domain 'test-default-3dfcf2a48071b66ed848db7937a8eec1' created from /run/user/1000/agentsandbox/test-default-3dfcf2a48071b66ed848db7937a8eec1/domain.xml

Connection timed out during banner exchange
Connection timed out during banner exchange
Connection timed out during banner exchange
Connection timed out during banner exchange
building the system configuration...
Done. The new configuration is /nix/store/kapg52qccy514hm0rqwaaxj8xcghvk7z-nixos-system-agentsandbox-25.11.20260429.755f5aa
/home/user/.local/share/agentsandbox/test-default-3dfcf2a48071b66ed848db7937a8eec1/sysroot/nix/store/kapg52qccy514hm0rqwaaxj8xcghvk7z-nixos-system-agentsandbox-25.11.20260429.755f5aa
Domain 'test-default-3dfcf2a48071b66ed848db7937a8eec1' destroyed
```

4. Start the VM
   - Run `agentsandbox up` and be patient as it takes a little while
   - Verify that the VM starts, four virtiofs processes start
   - Verify that `runtime.log` exists and that `requests.jsonl` exists.
   - Verify that the SSH port reported by `agentsandbox port 22 --protocol tcp` matches the forwarded port used by `up`.
   - Verify that the instance `/etc/machine-id` file is stable across repeated `build` runs.



3. Inspect launcher metadata.
   - Verify that `allowed_hosts` is populated and `mounts` contains the header line
     `# <rel-host-path><TAB><guest-name>` and a default workspace line `.\t` plus the
     workspace directory name (the final path component of the project directory).
   - Run `agentsandbox verify`.
   - Verify that the current `verify` stub announces the planned repair commands `nixos-rebuild --repair` and `nix store verify --repair`.
   - Verify that the command list, version string, and dependency diagnostics are printed.

3.5 Validate `audit` command wiring (`run_audit`).
   - Ensure host `vulnix` is available in the current shell (recommended: run under this repo's `nix develop`).
   - Run `agentsandbox audit -- --version`.
   - Verify that the command invokes host `vulnix` and version output is visible via inherited stdout/stderr.
   - Run `agentsandbox audit -- -j` and stop after confirming scan output starts.
   - Verify argument contract: launcher prepends `-g <instance-sysroot>`, then forwards user args unchanged.
   - Verify exit-code contract: launcher process exit status matches vulnix exit status.
   - If host `vulnix` is missing, verify that the error message points to the patched host binary requirement in this repository environment.

4. Confirm config and instance resolution from `doctor`.
   - Run `agentsandbox doctor` (or use the output from step 3).
   - Verify that `ResolvedFlakeDir` is the local `.agentsandbox/` directory for this workspace.
   - Verify that `InstanceId`, `InstanceDataDir`, `InstanceStateDir`, and `InstanceRuntimeDir`
     appear in the output.

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

6.5. Validate `-w` and mount path resolution.
   - Under the project directory, create two directories `ws-a/alpha` and `ws-b/alpha`.
   - Run `agentsandbox -w "$PWD/ws-a" init` and `agentsandbox -w "$PWD/ws-a" mount ./alpha`.
   - Run `agentsandbox -w "$PWD/ws-b" init` and `agentsandbox -w "$PWD/ws-b" mount ./alpha`.
   - Verify that `ws-a/.agentsandbox/mounts` contains `.\tws-a` and a tab-separated line whose guest name is `alpha`.
   - Verify that `ws-b/.agentsandbox/mounts` contains `.\tws-b` and a tab-separated line whose guest name is `alpha`.

7. Build the guest system.
   - Run `agentsandbox build`.
   - Verify that the sysroot is created under `$XDG_DATA_HOME/agentsandbox/<instance-id>/sysroot`.
   - Verify that the guest top-level build exists inside the sysroot.

8. Start the VM.
   - Run `agentsandbox up`.
   - Verify that the VM starts, the proxy sidecar starts, both virtiofs daemons start, and the transient libvirt domain is created.
   - Verify that `runtime.log` exists and that `requests.jsonl` exists.
   - Verify that the SSH port reported by `agentsandbox port 22 --protocol tcp` matches the forwarded port used by `up`.

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
    - Verify that `persistent/` is removed (the `sysroot/` directory is already absent from `destroy -s`).
    - Run `agentsandbox destroy -sd`.
    - Verify that the whole data dir is removed.
    - Verify that a later `agentsandbox up` does not reuse prior guest persistent data from before `-sd`.
    - Run `agentsandbox destroy -l`.
    - Verify that the instance state directory (as reported by `InstanceStateDir` from `doctor`) is removed.
    - Run `agentsandbox destroy -c`.
    - Verify that the resolved config directory (`.agentsandbox/`) is removed.
    - Run `agentsandbox init` to recreate the local configuration files needed for the following step.

16. Validate the remaining utility commands in the same session.
   - Run `agentsandbox port`.
   - Verify that it prints the host port for the configured guest port.
   - Run `agentsandbox port 22 --protocol tcp`.
   - Verify that the OpenSnitch port or other forwarded port can be resolved when configured.
   - Run `agentsandbox wait` again after stopping the VM.
   - Verify that it exits cleanly when nothing is running.

   - Here, deny-by-default means that removing every active `allowed_hosts` entry must still block unmatched hosts rather than falling back to allow-all.


## Acceptance

- Every subcommand listed in `agentsandbox --help` is executed at least once in this sequence.
- The sequence covers local config, global config resolution, guest build, VM startup, guest attachment, logs, stats, mount management, allowlist editing, proxy log tailing, lifecycle control, and cleanup.
- The sequence must pass without manual edits between steps other than the explicit host directories and log records created for the scenario.
- Config and instance resolution are checked via `agentsandbox doctor`.
