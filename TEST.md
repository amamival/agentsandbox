# Tests

This file defines a end-to-end scenario sequence that exercises every subcommand.

## Requirements

- Archtecture: x86_64
- Kernel: Linux 6.1 or later
- Volume: writable home directory

## Scenario

0. Check installation
  - Run `nix shell <repo>` where `<repo>` is the path to the clone of this repository
  - Run `devvm --help` and verify that the help message is printed
  - Run `devvm help` and verify that the help message is printed
  - Run `devvm --version` and verify that the version string is printed
  - Run `devvm version` and verify that the version string is printed
  - Run `devvm doctor` and verify that `Cmd*` dependencies are available
    (we'll see other fields in the output later)

1. Using the project workspace
  - Create a new empty directory and `cd` into it
  - Confirm that no `.devvm/` directory exists yet
  - Run `devvm init`
  - Verify that `.devvm/{flake.nix,configuration.nix,devvm.toml}` and
    `.devvm/devvm/{flake.nix,claude-nixos.nix}` were created
  - Run `devvm doctor` and verify that `ResolvedFlakeDir` is the local `.devvm/` directory
  - Run `devvm doctor` and verify that `InstanceId` is `<dirname>[default]`,
    `Instance{Data,State,Runtime}Dir` appear in the output
  - Run `devvm init` again and verify that the message "init: $PWD/.devvm already exists" is printed
  - Run `devvm init -f` and verify that the message "init: wrote template files to $PWD/.devvm" is printed
    and timestamps of the files are updated
  - Temporarily `cd` to a different directory and verify that `devvm doctor` reports *different* `ResolvedFlakeDir`
  - In the temporary directory, verify that `devvm doctor -w <original-dir>` reports *same* `ResolvedFlakeDir` as before
  - Run ` devvm destroy -c` and verify that the local `.devvm/` directory is removed

2. Using the global workspace
  - Create a new empty directory and `cd` into it
  - Confirm that no `.devvm/` directory exists yet
  - Run `devvm init --global`
  - Verify that `$XDG_CONFIG_HOME/devvm/devvm/{flake.nix,configuration.nix,devvm.toml}`
    was created (`$XDG_CONFIG_HOME` is typically `~/.config/`)
  - Run `devvm --global doctor` and verify that `ResolvedFlakeDir` is the
    global `$XDG_CONFIG_HOME/devvm/devvm/` directory
  - Run `devvm --global doctor` and verify that `InstanceId` is `devvm[default]`, and
    `Instance{Data,State,Runtime}Dir` appear in the output
  - Run `devvm init --global` again and verify that the message "init: $XDG_CONFIG_HOME/devvm/devvm already exists" is printed
  - Run `devvm init --global -f` and verify that the message "init: wrote template files to $XDG_CONFIG_HOME/devvm/devvm" is printed
    and timestamps of the files are updated
  - `cd` to a different directory without `.devvm/` and verify that `devvm --global doctor` reports *same* `ResolvedFlakeDir` as before
  - Run `devvm --global destroy -c` and verify that the global project directory is removed

3. Build the initial guest system
  - Create a new empty directory and `cd` into it
  - Run `devvm init`
  - Run `devvm build` and be patient as it takes a while
  - Optionally examine generated `domain.xml`, `devvm ps`, and `virsh console <domain>` in other terminals
  - Finally it destroys the new domain. Example output:
```
Domain 'test-default-3dfcf2a48071b66ed848db7937a8eec1' created from /run/user/1000/devvm/test-default-3dfcf2a48071b66ed848db7937a8eec1/domain.xml

Connection timed out during banner exchange
Connection timed out during banner exchange
Connection timed out during banner exchange
Connection timed out during banner exchange
building the system configuration...
Done. The new configuration is /nix/store/<hash>-nixos-system-devvm-26.05.<revision>
/home/user/.local/share/devvm/test[default]/system/nix/store/<hash>-nixos-system-devvm-26.05.<revision>
Domain 'test[default]' destroyed
```

4. Start the VM
  - Run `devvm up` and be patient as it takes a little while
  - Verify that the VM starts and the `/nix` and `/persistent` virtiofs exports are active

5. Validate runtime lifecycle controls
  - Run `devvm ls` and verify that the current instance is listed
  - Run `devvm ps` and verify that the VM is running
  - Run `devvm ssh` and verify that a shell opens inside the guest as a regular user
  - Run `devvm exec` and verify that a shell opens inside the guest as root user
  - Run `devvm logs` and verify that the guest journal is displayed
  - Run `devvm stats` and verify that CPU, memory statistics are displayed
  - Run `devvm wait` in a second terminal
  - Run `devvm down` and verify that the VM stops cleanly
  - Verify that `wait` returns only after the VM stops
  - Run `devvm up` again and verify that the VM starts
  - Run `devvm pause`
  - Run `devvm ps` and verify that the VM is paused
  - Run `devvm unpause`
  - Run `devvm ps` and verify that the VM is running
  - Run `devvm kill` and verify that the VM is destroyed immediately
  - Run `devvm ps` and verify that the VM is down again

6. Validate dynamic mounts
  - Run `devvm down` if the VM is running
  - Run `devvm mount` and verify that the output contains a default workspace row
    `.<TAB><workspace-name><TAB>rw`
    (the final path component of the project directory)
  - Create two host directories, for example `alpha/` and `beta/` with `mkdir -p alpha beta; touch alpha/A beta/B`
  - Run `devvm mount ./alpha` while the VM is down
  - Run `devvm up`
  - Run `devvm ssh l /persistent/home/workspace` and verify that the guest sees the `alpha` directory as regular user
  - Verify that the initial workspace mount is visible in the guest under the workspace basename.
  - Run `devvm mount ./beta sandbox-beta` while the VM is running
  - Run `devvm ssh l /persistent/home/workspace` and verify that the guest sees the `beta` directory
  - Verify that `.devvm/devvm.local.toml` contains the `alpha` and `sandbox-beta` mount entries
  - Run `devvm unmount ./alpha`
  - Verify that the `alpha` entry is removed from the effective TOML config and the guest no longer sees it
  - Run `devvm unmount .` and verify that current workspace is unmounted in the guest
  - Run `cd alpha; devvm -w .. mount .` and verify that the guest sees the current workspace is mounted as before
    (think `-w` as chroot-like relative path).

7. Validate persistence
  - See `.devvm/configuration.nix` for the persistence configuration
  - While the VM is running, run `devvm ssh touch ~/.local/bin/persist ~/ephemeral`
  - Run `devvm down`
  - Run `devvm doctor` and verify that `InstanceDataDir` is `$XDG_DATA_HOME/devvm/<instance-id>/`
  - Verify that `$XDG_DATA_HOME/devvm/<instance-id>/persistent/home/vscode/.local/bin/persist` exists
  - Verify that `$XDG_DATA_HOME/devvm/<instance-id>/persistent/home/vscode/ephemeral` does not exist
  - Run `devvm up`
  - Run `devvm ssh find ~` and verify that the guest has `persist` but no `ephemeral` file

8. Validate port forwarding
  - See `.devvm/configuration.nix` for the port forwarding configuration
  - Run `devvm doctor` to see `InstancePortForwards` contains the configured port forwards
  - Run `devvm port` and verify that it prints all the host ports for the configured guest ports:
    `ssh	tcp	127.0.0.1:2223	lo`
  - Run `devvm port 22 --protocol tcp` and verify that it prints the host endpoint for the guest service:
    `127.0.0.1:2223`

9. Validate `verify` command
  - Run `devvm doctor` to see `CmdNixStorePathForVerifyCmd` is available
  - Run `devvm verify` and verify that the output contains `nix-store --verify` and `nixos-rebuild --repair` outputs

10. Validate `audit` command
  - Run `devvm doctor` to see `CmdVulnixPathForAuditCmd` is available
  - Run `devvm audit -- --version` and verify that the output contains `vulnix <version>`
  - Run `devvm audit` and verify that the output contains hundreds of CVE vulnerabilities, and
    the output contains paths under `InstanceSystemDir`

11. Validate destroy semantics
   - Run `devvm doctor` to see `InstanceRootfsDir`, `InstanceSystemDir`, `InstanceUserDir`, `InstanceStateDir`, `InstanceLogsDir`
   - Run `devvm destroy` and verify that the `InstanceRootfsDir`, `InstanceSystemDir`, `InstanceUserDir`, `InstanceLogsDir` remain
   - Run `devvm destroy -s` and verify that `InstanceRootfsDir` and `InstanceSystemDir` are removed
   - Run `devvm destroy -d` and verify that only the `InstanceUserDir` is removed
   - Run `devvm destroy -sd` and verify that only the `InstanceDataDir` is removed
   - Run `devvm destroy -l` and verify that only the `InstanceStateDir` is removed
   - Run `devvm destroy -c` and verify that only the `ResolvedFlakeDir` is removed
   - Run `devvm init` to recreate the local configuration files needed for the following step

12. Validate allowlist entries
  - Run `devvm allow-domain Example.COM`.
  - Run `devvm allow-domain '*.Example.COM'`.
  - Run `devvm allow-domain '*'`.
  - Verify that `.devvm/devvm.local.toml` contains the normalized
    `example.com`, `*.example.com`, and `*` keys.
  - Verify that `devvm allow-domain https://example.com/path` fails as invalid input.
  - Run `devvm unallow-domain example.com`.
  - Run `devvm unallow-domain '*.example.com'`.
  - Run `devvm unallow-domain '*'`.
  - Verify that the effective TOML policy no longer enables these entries.
  - This validates persistence only; v0.3 does not enforce the domain policy on network traffic.

13. Validate proxy log handling
  - Append a request record to `logs/requests.jsonl`.
  - Create a compressed archive `requests-*.jsonl.zst` and keep the active `requests.jsonl` file.
  - Run `devvm proxy-logs`.
  - Verify that the archived request log series and the active log are both readable in order.

## Acceptance

- Every subcommand listed in `devvm --help` is executed at least once in this sequence
- The sequence covers local config, global config resolution, guest build, VM lifecycle control,
  dynamic mount management, logs, stats, port forwarding, domain-policy editing, proxy log tailing, and cleanup
- The sequence must pass without manual edits between steps other than the explicit host directories and
  files created for the scenario
- Please add a star on GitHub repository if you found this project useful
