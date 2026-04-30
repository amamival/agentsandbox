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

5. Validate runtime lifecycle controls
  - Run `agentsandbox ps` and verify that the VM is running
  - Run `agentsandbox ssh` and verify that a shell opens inside the guest as a regular user
  - Run `agentsandbox exec` and verify that a shell opens inside the guest as root user
  - Run `agentsandbox logs` and verify that the guest journal is displayed
  - Run `agentsandbox stats` and verify that CPU, memory statistics are displayed
  - Run `agentsandbox wait` in a second terminal
  - Run `agentsandbox down` and verify that the VM stops cleanly
  - Verify that `wait` returns only after the VM stops
  - Run `agentsandbox up` again and verify that the VM starts
  - Run `agentsandbox pause`
  - Run `agentsandbox ps` and verify that the VM is paused
  - Run `agentsandbox unpause`
  - Run `agentsandbox ps` and verify that the VM is running
  - Run `agentsandbox kill` and verify that the VM is destroyed immediately
  - Run `agentsandbox ps` and verify that the VM is down again

6. Validate dynamic mounts
  - Run `agentsandbox down` if the VM is running
  - Run `agentsandbox mount` and verify that the output contains the header line
    `# <rel-host-path><TAB><guest-name>` and a default workspace line `.\t` plus the workspace directory name
    (the final path component of the project directory)
  - Create two host directories, for example `alpha/` and `beta/` with `mkdir -p alpha beta; touch alpha/A beta/B`
  - Run `agentsandbox mount ./alpha` while the VM is down
  - Run `agentsandbox up`
  - Run `agentsandbox ssh l /persistent/workspace` and verify that the guest sees the `alpha` directory as regular user
  - Verify that the initial workspace mount is visible in the guest under the workspace basename.
  - Run `agentsandbox mount ./beta sandbox-beta` while the VM is running
  - Run `agentsandbox ssh l /persistent/workspace` and verify that the guest sees the `beta` directory
  - Verify that `.agentsandbox/mounts` contains `alpha<TAB>alpha` and `beta<TAB>sandbox-beta`
  - Run `agentsandbox unmount ./alpha`
  - Verify that the `alpha` entry is removed from `mounts` and the guest sees the `alpha` directory is removed
  - Run `agentsandbox unmount .` and verify that current workspace is unmounted in the guest
  - Run `cd alpha; agentsandbox -w .. mount .` and verify that the guest sees the current workspace is mounted as before
    (think `-w` as chroot-like relative path).

7. Validate persistence
  - See `.agentsandbox/configuration.nix` for the persistence configuration
  - While the VM is running, run `agentsandbox ssh touch ~/.local/bin/persist ~/ephemeral`
  - Run `agentsandbox down`
  - Run `agentsandbox doctor` and verify that `InstanceDataDir` is `$XDG_DATA_HOME/agentsandbox/<instance-id>/`
  - Verify that `$XDG_DATA_HOME/agentsandbox/<instance-id>/persistent/home/vscode/.local/bin/persist` exists
  - Verify that `$XDG_DATA_HOME/agentsandbox/<instance-id>/persistent/home/vscode/ephemeral` does not exist
  - Run `agentsandbox up`
  - Run `agentsandbox ssh find ~` and verify that the guest has `persist` but no `ephemeral` file

8. Validate port forwarding
  - See `.agentsandbox/configuration.nix` for the port forwarding configuration
  - Run `agentsandbox doctor` to see `InstancePortForwards` contains the configured port forwards
  - Run `agentsandbox port` and verify that it prints all the host ports for the configured guest ports:
    `ssh	tcp	127.0.0.1:2223	lo`
  - Run `agentsandbox port 22 --protocol tcp` and verify that it prints the host endpoint for the guest service:
    `127.0.0.1:2223`

9. Validate `verify` command
  - Run `agentsandbox doctor` to see `CmdNixStorePathForVerifyCmd` is available
  - Run `agentsandbox verify` and verify that the output contains `nix-store --verify` and `nixos-rebuild --repair` outputs

10. Validate `audit` command
  - Run `agentsandbox doctor` to see `CmdVulnixPathForAuditCmd` is available
  - Run `agentsandbox audit -- --version` and verify that the output contains `vulnix <version>`
  - Run `agentsandbox audit` and verify that the output contains hundreds of CVE vulnerabilities, and
    the output contains pathes under `InstanceSysrootDir`

11. Validate destroy semantics
   - Run `agentsandbox doctor` to see `InstanceSysrootDir`, `InstancePersistentDir`, `InstanceStateDir`, `InstanceLogsDir`
   - Run `agentsandbox destroy` and verify that the `InstanceSysrootDir`, `InstancePersistentDir`, `InstanceLogsDir` remain
   - Run `agentsandbox destroy -s` and verify that only the `InstanceSysrootDir` is removed
   - Run `agentsandbox destroy -d` and verify that only the `InstancePersistentDir` is removed
   - Run `agentsandbox destroy -sd` and verify that only the `InstanceDataDir` is removed
   - Run `agentsandbox destroy -l` and verify that only the `InstanceStateDir` is removed
   - Run `agentsandbox destroy -c` and verify that only the `ResolvedFlakeDir` is removed
   - Run `agentsandbox init` to recreate the local configuration files needed for the following step

12. Validate allowlist entries
  - Run `agentsandbox allow-domain Example.COM`.
  - Run `agentsandbox allow-domain https://example.com/path`.
  - Run `agentsandbox allow-domain 'https://*.Example.COM.:8443/path'`.
  - Verify that `allowed_hosts` contains one normalized `example.com` entry and one `*.example.com` entry.
  - Run `agentsandbox unallow-domain https://EXAMPLE.com.:443/path`.
  - Verify that the exact `example.com` line is removed.
  - Here, deny-by-default means that removing every active `allowed_hosts` entry must still block
    unmatched hosts rather than falling back to allow-all.

13. Validate proxy log handling
  - Append a request record to `logs/requests.jsonl`.
  - Create a compressed archive `requests-*.jsonl.zst` and keep the active `requests.jsonl` file.
  - Run `agentsandbox proxy-logs`.
  - Verify that the archived request log series and the active log are both readable in order.

## Acceptance

- Every subcommand listed in `agentsandbox --help` is executed at least once in this sequence
- The sequence covers local config, global config resolution, guest build, VM lifecycle control,
  dynamic mount management, logs, stats, port forwarding, allowlist editing, proxy log tailing, and cleanup
- The sequence must pass without manual edits between steps other than the explicit host directories and
  files created for the scenario
- Please add a star on GitHub repository if you found this project useful