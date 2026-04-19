use clap::{Parser, Subcommand};

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
    Wait {
        states: Vec<String>,
    },
    /// Mount a directory into a running VM now and on future starts, or show current mounts
    Mount {
        path: Option<String>,
        name: Option<String>,
    },
    /// Unmount a directory from a running VM now and on future starts
    Unmount {
        path: String,
    },
    /// Prints the public port for a port binding
    Port {
        guest_port: Option<u16>,
        guest_proto: Option<String>,
    },
    /// Add a firewall rule that allows outbound traffic to a domain
    AllowDomain {
        domain: String,
    },
    /// Remove the rule for the domain
    UnallowDomain {
        domain: String,
    },
    /// Follow MITM proxy logs
    ProxyLogs,
    /// Verify and repair build
    Verify,
}

fn main() {
    match Cli::parse().command {
        Some(Command::Version) => println!("{}", env!("CARGO_PKG_VERSION")),
        None => {
            let _ = Cli::parse();
        }
        Some(_) => {}
    }
}
