use clap::{Parser, Subcommand};
use std::{env, fs, path::Path, process};

#[derive(Parser)]
#[command(
    name = "agentsandbox",
    about = "An unshared, efficient, reproducible NixOS Linux VM for self-improving agentic workflows",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show version
    Version,
    /// Show diagnostics
    Doctor,
    /// Create `.agentsandbox/` and copy the initial template files
    Init {
        #[arg(short = 'g', long)]
        global: bool,
    },
    /// Build the guest system
    Build,
    /// Rebuild and start a VM; fails if it is already running
    Up {
        #[arg(short = 'd', long)]
        detach: bool,
    },
    /// Tear down the VM gracefully
    Down,
    /// Forcibly stop the VM
    Kill,
    /// Pause all running VMs
    Pause,
    /// Unpause all running VMs
    Unpause,
    /// Delete the guest system while preserving persistent data
    Destroy,
    /// Show status of the VMs
    Ps,
    /// Connect to a regular user shell in a running VM. Equivalent to `ssh -p <port> <user>@127.0.0.1 ...`
    Ssh {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Execute a command in a running VM, or attach if omitted
    Exec {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Show logs from a running VM. Runs `journalctl` with `-en1000` by default
    Logs {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Display percentage of CPU, memory, network I/O, block I/O and PIDs for VMs
    Stats,
    /// Wait for running VMs to stop
    Wait { states: Vec<String> },
    /// Mount a directory into a running VM now and on future starts, or show current mounts
    Mount { path: Option<String>, name: Option<String> },
    /// Unmount a directory from a running VM now and on future starts
    Unmount { path: String },
    /// Prints the public port for a port binding
    Port { guest_port: Option<u16>, guest_proto: Option<String> },
    /// Add a firewall rule that allows outbound traffic to a domain
    AllowDomain { domain: String },
    /// Remove the rule for the domain
    UnallowDomain { domain: String },
    /// Follow MITM proxy logs
    ProxyLogs,
    /// Verify and repair build
    Verify,
}

// Create the config dir and initial files for local/global init, or return a displayable error.
fn init(current_dir: &Path, global: bool, xdg_config_home: Option<&Path>, home: Option<&Path>) -> Result<(), String> {
    let target = (if global {
        xdg_config_home
            .map(Path::to_path_buf)
            .or_else(|| home.map(|home| home.join(".config")))
            .ok_or("HOME is not set".to_owned())?
            .join("agentsandbox")
    } else {
        current_dir.join(".agentsandbox")
    });
    target.exists().then(|| Err(format!("{} already exists", target.display()))).unwrap_or(Ok(()))?;
    fs::create_dir_all(&target).map_err(|err| err.to_string())?;
    let current_dir = current_dir.canonicalize().map_err(|err| err.to_string())?;
    let workspace_name = current_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("failed to derive workspace name".to_owned())?;
    for (name, contents) in [
        ("flake.nix", include_str!("../share/agentsandbox/template/flake.nix").to_owned()),
        ("configuration.nix", include_str!("../share/agentsandbox/template/configuration.nix").to_owned()),
        ("allowed_hosts", String::new()),
        ("mounts", format!("# <host-path><TAB><guest-name>\n{}\t{workspace_name}\n", current_dir.display())),
    ] {
        fs::write(target.join(name), contents).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn main() {
    match Cli::parse().command {
        Some(Command::Version) => println!("{}", env!("CARGO_PKG_VERSION")),
        Some(Command::Init { global }) => {
            let xdg_config_home = env::var_os("XDG_CONFIG_HOME");
            let home = env::var_os("HOME");
            if let Err(err) = init(
                &env::current_dir().expect("current directory"),
                global,
                xdg_config_home.as_deref().map(Path::new),
                home.as_deref().map(Path::new),
            ) {
                eprintln!("{err}");
                process::exit(1);
            }
        }
        None => {}
        Some(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::init;
    use std::{env, fs};

    #[test]
    fn init_writes_expected_files_and_rejects_existing_targets() {
        let root = env::temp_dir().join("agentsandbox-rs-test");
        let _ = fs::remove_dir_all(&root);
        let workspace = root.join("workspace");
        let global_home = root.join("home");
        let global_config = root.join("xdg-config");
        for dir in [&workspace, &global_home, &global_config] {
            fs::create_dir_all(dir).unwrap();
        }
        let mounts = format!("# <host-path><TAB><guest-name>\n{}\tworkspace\n", workspace.canonicalize().unwrap().display());
        init(&workspace, false, Some(&global_config), Some(&global_home)).unwrap();
        let local = workspace.join(".agentsandbox");
        init(&workspace, true, Some(&global_config), Some(&global_home)).unwrap();
        let global = global_config.join("agentsandbox");
        for dir in [&local, &global] {
            for file in ["flake.nix", "configuration.nix", "allowed_hosts"] {
                assert!(dir.join(file).is_file());
            }
            assert_eq!(fs::read_to_string(dir.join("mounts")).unwrap(), mounts);
        }
        assert_eq!(
            init(&workspace, false, Some(&global_config), Some(&global_home)).unwrap_err(),
            format!("{} already exists", local.display())
        );
        assert_eq!(
            init(&workspace, true, Some(&global_config), Some(&global_home)).unwrap_err(),
            format!("{} already exists", global.display())
        );
        fs::remove_dir_all(root).unwrap();
    }
}
