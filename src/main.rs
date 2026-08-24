use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use elementtree::{Element as XmlElement, WriteOptions};
use flate2::read::GzDecoder;
use pathdiff::diff_paths;
use reqwest::{blocking::Client, header};
use rustix::{
    fs::CWD,
    io::{FdFlags, fcntl_setfd, read, write},
    mount::{MountFlags, MountPropagationFlags, MoveMountFlags, OpenTreeFlags, UnmountFlags},
    mount::{mount, mount_bind_recursive, mount_change, move_mount, open_tree, unmount},
    pipe::{PipeFlags, pipe_with},
    process::{Pid, Signal, WaitOptions, chdir, getuid, kill_process, pivot_root, setsid, waitpid},
    runtime::{Fork, exit_group, kernel_fork},
    thread::{UnshareFlags, unshare_unsafe},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{BufRead as _, BufReader, IsTerminal as _, Read as _, Write as _},
    net::IpAddr,
    os::fd::AsRawFd,
    os::unix::net::{UnixListener, UnixStream},
    os::unix::{fs::MetadataExt, fs::symlink, process::CommandExt},
    path::{Component, Path, PathBuf},
    process::{self, Stdio},
};
use toml_edit::{DocumentMut, Item, TableLike};

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const LOCAL_CONFIG_DIR: &str = concat!(".", env!("CARGO_PKG_NAME"));
const CONFIG_TOML: &str = concat!(env!("CARGO_PKG_NAME"), ".toml");
const CONFIG_LOCAL_TOML: &str = concat!(env!("CARGO_PKG_NAME"), ".local.toml");
const DEFAULT_DOMAIN_XML: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/template/domain.xml"));

#[derive(Parser)]
#[command(
    name = APP_NAME,
    about = "A secure, efficient, reproducible NixOS Linux VM for self-improving agentic workflows",
    version
)]
struct Cli {
    /// Use only global config (`$XDG_CONFIG_HOME/devvm`) and skip local upward search.
    #[arg(short = 'g', long, global = true)]
    global: bool,
    /// Select project name. Combined with hostname to form the instance name.
    #[arg(short = 'p', long, global = true, env = "DEVVM_PROJECT_NAME")]
    project_name: Option<String>,
    /// Select sandbox hostname (build target and instance identity input).
    #[arg(short = 'n', long, global = true, default_value = "default")]
    hostname: String,
    /// Resolve the active workspace and config as if running from this directory.
    #[arg(short = 'w', long, global = true, hide_default_value = true, default_value_os_t = env::current_dir().expect("invalid cwd") )]
    workspace: PathBuf,

    #[arg(long, hide = true, env = "HOME")]
    home: Option<PathBuf>,
    #[arg(long, hide = true, env = "XDG_CONFIG_HOME")]
    xdg_config_home: Option<PathBuf>,
    #[arg(long, hide = true, env = "XDG_DATA_HOME")]
    xdg_data_home: Option<PathBuf>,
    #[arg(long, hide = true, env = "XDG_STATE_HOME")]
    xdg_state_home: Option<PathBuf>,
    #[arg(long, hide = true, env = "XDG_RUNTIME_DIR")]
    xdg_runtime_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show version
    Version,
    /// Show diagnostics
    Doctor,
    /// Create `.devvm/` and write the initial template files
    Init {
        /// Overwrite existing files.
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Build the guest system
    Build {
        /// Build the initial template-based system profile instead. Current configs are kept.
        #[arg(short = 'b', long)]
        bootstrap: bool,
        /// Allow the build path to update `flake.lock`.
        #[arg(long)]
        write_lock: bool,
    },
    /// Rebuild and start a VM; if already running, build and switch
    Up {
        #[arg(short = 'd', long)]
        detach: bool,
        /// Allow the build path to update `flake.lock`.
        #[arg(long)]
        write_lock: bool,
    },
    /// Tear down the VM gracefully
    Down,
    /// Forcibly stop the VM
    Kill,
    /// Pause running VMs for all hostnames in the current config
    Pause,
    /// Unpause VMs for all hostnames in the current config
    Unpause,
    /// Kill and delete guest files selected by flags (none by default)
    ///
    /// For the non-project instance, use `--global`
    #[command(alias = "destory")]
    Destroy {
        /// Remove bootstrap rootfs and system data
        #[arg(short = 's', long)]
        system: bool,
        /// Remove user data; with --system, remove the whole data dir
        #[arg(short = 'd', long)]
        data: bool,
        /// Remove the instance states such as logs
        #[arg(short = 'l', long)]
        logs: bool,
        /// Remove the resolved config dir
        #[arg(short = 'c', long)]
        conf: bool,
    },
    /// List all VMs stored
    Ls,
    /// List VM statuses for all hostnames in the current config
    Ps,
    /// Run a command as a user in a running VM, or attach if omitted
    ///
    /// Resolves SSH host port from `port-forwards` using guest `tcp/22`.
    /// Fails when no matching mapping exists.
    ///
    /// Use `--` before arguments that conflict with global flags.
    Ssh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run a command as root in a running VM, or attach if omitted
    ///
    /// Use `--` before arguments that conflict with global flags.
    Exec {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show logs from a running VM. Runs `journalctl` with `-en1000` by default
    ///
    /// Use `--` before arguments that conflict with global flags.
    Logs {
        #[arg(default_values = ["-en1000"])]
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Display statistics of CPU time, memory for VMs
    Stats,
    /// Block until this VM becomes one of the states. Wait for stop states by default
    Wait {
        #[arg(default_values = ["down", "shut off", "crashed"])]
        states: Vec<String>,
    },
    /// Mount a file or directory into a running VM, or show mounts entries
    Mount {
        /// Host path. Relative paths are resolved from the current working directory.
        path: Option<String>,
        name: Option<String>,
        #[arg(long)]
        read_only: bool,
    },
    /// Unmount a file or directory from a running VM now and on future starts
    Unmount {
        /// Host path. Relative paths are resolved from the current working directory.
        path: String,
    },
    /// Prints the public port for a port binding
    Port {
        /// Guest port to resolve to a host port
        guest_port: Option<u16>,
        /// Protocol
        #[arg(long, value_parser = ["tcp", "udp"])]
        protocol: Option<String>,
    },
    /// Add a domain to the hostname-specific TOML policy
    AllowDomain { domain: String },
    /// Remove a domain from the hostname-specific TOML policy
    UnallowDomain { domain: String },
    /// Follow MITM proxy logs
    ProxyLogs,
    /// Verify and repair build
    ///
    /// 1) Uses host nix to run verify/repair against the guest store.
    /// 2) If guest is running, runs nixos-rebuild --repair inside the guest.
    ///
    /// Limitations:
    /// - Uses host nix binary (see doctor), substituter, trusted keys, etc.
    /// - Verifies/repairs store/system state; does not analyze malicious flake.nix or other executables.
    #[command(verbatim_doc_comment)]
    Verify,
    /// Run CVE scan against the guest store
    ///
    /// Extra arguments are passed to vulnix as-is.
    /// See upstream <https://github.com/nix-community/vulnix> for how to reduce false positives.
    #[command(
        verbatim_doc_comment,
        override_usage = "devvm audit [OPTIONS] -- [VULNIX_OPTIONS] [PATHS...]

Vulnix Options:
  -S, --system                    Scan the current system.
  -G, --gc-roots                  Scan all active GC roots (including old
                                  ones).
  -p, --profile PATH              Scan this profile (eg: ~/.nix-profile)
  -f, --from-file FILENAME        Read derivations from file
  -w, --whitelist TEXT            Load whitelist from file or URL (may be
                                  given multiple times).
  -W, --write-whitelist FILENAME  Write TOML whitelist containing current
                                  matches.
  -c, --cache-dir DIRECTORY       Cache directory to store parsed archive
                                  data. Default: ~/.cache/vulnix
  -r, --requisites / -R, --no-requisites
                                  Yes: determine transitive closure. No:
                                  examine just the passed derivations
                                  (default: yes).
  -C, --closure                   Examine the closure of an output path
                                  (runtime dependencies). Implies --no-
                                  requisites.
  -m, --mirror TEXT               Mirror to fetch NVD archives from. Default:
                                  https://github.com/fkie-cad/nvd-json-data-
                                  feeds/releases/latest/download/.
  -j, --json / --no-json          JSON vs. human readable output.
  -s, --show-whitelisted          Shows whitelisted items as well
  -D, --show-description          Show descriptions of vulnerabilities
  -v, --verbose                   Increase output verbosity (up to 2 times).
  -V, --version                   Print vulnix version and exit."
    )]
    Audit {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            hide = true,
            default_values = ["-G"]
        )]
        args: Vec<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct Env {
    is_global: bool,
    project_name: Option<String>,
    hostname: String,
    search_dir: PathBuf,
    config_root: PathBuf,
    data_root: PathBuf,
    state_root: PathBuf,
    runtime_root: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct Instance {
    id: String,
    is_global: bool,
    project_name: String,
    workspace: PathBuf,
    flake_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    runtime_dir: PathBuf,
    rootfs: PathBuf,
    system: PathBuf,
    user: PathBuf,
    logs_dir: PathBuf,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub vm: Vm,
    pub protection: Protection,
    pub mounts: BTreeMap<String, PolicyEntry<Mount>>,
    pub allowed_hosts: BTreeMap<String, PolicyEntry<AllowedHost>>,
    pub port_forwards: BTreeMap<String, PolicyEntry<PortForward>>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Vm {
    pub vcpus: Option<u32>,
    pub memory_mi_b: Option<u32>,
    pub libvirt_domain_xml: Option<String>,
    pub allow_domain_xml: Option<PolicyEntry<String>>,
    /// When true, guest should trust the host OS CA bundle.
    pub use_host_certs: Option<bool>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Protection {
    pub mutable_nix_store: Option<bool>,
    pub mutable_system_profile: Option<bool>,
}

#[derive(Deserialize)]
pub struct Mount {
    pub source: String,
    pub readonly: Option<bool>,
}

#[derive(Deserialize)]
pub struct AllowedHost {
    pub fetch_allowlist: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize)]
pub struct PortForward {
    pub proto: String,
    pub address: IpAddr,
    pub dev: Option<String>,
    #[serde(alias = "host_start")]
    pub host: u16,
    pub host_end: Option<u16>,
    pub guest: u16,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum PolicyEntry<T> {
    Remove(bool),
    Set(T),
}

fn main() {
    if let Err(err) = (|| -> anyhow::Result<()> {
        let cli = Cli::parse();
        let env = resolve_env(&cli).context("resolve environment")?;
        match cli.command {
            Some(Command::Version) => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            Some(Command::Doctor) => run_doctor(&env).context("doctor"),
            Some(Command::Init { force }) => run_init(&env, force).context("init"),
            Some(Command::Build { bootstrap, write_lock }) => run_build_or_up(&env, bootstrap, false, false, write_lock).context("build"),
            Some(Command::Up { detach, write_lock }) => run_build_or_up(&env, false, true, !detach, write_lock).context("up"),
            Some(Command::Down) => run_virsh_action(&env, "shutdown").context("down"),
            Some(Command::Kill) => run_virsh_action(&env, "destroy").context("kill"),
            Some(Command::Pause) => run_virsh_action_all(&env, "suspend").context("pause"),
            Some(Command::Unpause) => run_virsh_action_all(&env, "resume").context("unpause"),
            Some(Command::Destroy { system, data, logs, conf }) => run_destroy(&env, system, data, logs, conf).context("destroy"),
            Some(Command::Ls) => run_ls(&env).context("ls"),
            Some(Command::Ps) => run_ps(&env).context("ps"),
            Some(Command::Ssh { args }) => run_ssh(&env, &args, false, false).context("ssh"),
            Some(Command::Exec { args }) => run_ssh(&env, &args, true, false).context("exec"),
            Some(Command::Logs { args }) => run_logs(&env, &args).context("logs"),
            Some(Command::Stats) => run_stats(&env).context("stats"),
            Some(Command::Wait { states }) => run_wait(&resolve_instance(&env)?, &states).context("wait"),
            Some(Command::Mount { path, name, read_only }) => run_mount(&env, path, name, true, read_only).context("mount"),
            Some(Command::Unmount { path }) => run_mount(&env, Some(path), None, false, false).context("unmount"),
            Some(Command::Port { guest_port, protocol }) => run_port(&env, guest_port, protocol.as_deref()).context("port"),
            Some(Command::AllowDomain { domain }) => run_allow_domain(&env, &domain).context("allow-domain"),
            Some(Command::UnallowDomain { domain }) => run_unallow_domain(&env, &domain).context("unallow-domain"),
            Some(Command::Verify) => run_verify(&env).context("verify"),
            Some(Command::Audit { args }) => run_audit(&env, &args).context("audit"),
            None => run_build_or_up(&env, false, true, true, false).context("up"),
            Some(_) => {
                println!("Comming soon(tm)...");
                Ok(())
            }
        }
    })() {
        eprintln!("{err:#}");
        process::exit(1);
    }
}

#[inline(never)]
fn resolve_env(cli: &Cli) -> anyhow::Result<Env> {
    let uid = getuid().as_raw();
    let home = cli.home.clone().expect("please set HOME");
    Ok(Env {
        is_global: cli.global,
        hostname: cli.hostname.clone(),
        project_name: cli.project_name.clone(),
        search_dir: cli.workspace.clone(),
        config_root: cli.xdg_config_home.clone().unwrap_or_else(|| home.join(".config")).join(APP_NAME),
        data_root: cli.xdg_data_home.clone().unwrap_or_else(|| home.join(".local/share")).join(APP_NAME),
        state_root: cli.xdg_state_home.clone().unwrap_or_else(|| home.join(".local/state")).join(APP_NAME),
        runtime_root: cli
            .xdg_runtime_dir
            .clone()
            .unwrap_or_else(|| if uid == 0 { "/run".into() } else { format!("/run/user/{uid}").into() })
            .join(APP_NAME),
    })
}

#[inline(never)]
fn run_doctor(env: &Env) -> anyhow::Result<()> {
    let resolve_cmd_path = |name: &str| -> String {
        let output = process::Command::new("which").arg(name).output();
        match output {
            Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            Ok(_) => "missing!".to_owned(),
            Err(err) => format!("missing! ({err:#})"),
        }
    };
    println!("AppName:\t\t\t{}", APP_NAME);
    println!("Version:\t\t\t{}", env!("CARGO_PKG_VERSION"));
    println!("CmdVirshPath:\t\t\t{}", resolve_cmd_path("virsh"));
    println!("CmdPasstPath:\t\t\t{}", resolve_cmd_path("passt"));
    println!("CmdSshPath:\t\t\t{}", resolve_cmd_path("ssh"));
    println!("CmdVirtiofsdPath:\t\t{}", resolve_cmd_path("virtiofsd"));
    println!("CmdUnsharePath:\t\t\t{}", resolve_cmd_path("unshare"));
    println!("CmdNixStorePathForVerifyCmd:\t{}", resolve_cmd_path("nix-store"));
    println!("CmdVulnixPathForAuditCmd:\t{}", resolve_cmd_path("vulnix"));

    println!("ResolvedConfigRoot:\t\t{}", env.config_root.display());
    println!("ResolvedDataRoot:\t\t{}", env.data_root.display());
    println!("ResolvedStateRoot:\t\t{}", env.state_root.display());
    println!("ResolvedRuntimeRoot:\t\t{}", env.runtime_root.display());
    println!("WorkspaceArg:\t\t\t{}", env.search_dir.display());
    println!("IsUserGlobalProject:\t\t{}", env.is_global);
    println!("InstanceHostnameArg:\t\t{}", env.hostname);
    println!("ProjectNameArg:\t\t\t{}", env.project_name.as_deref().unwrap_or(""));

    let workspace = resolve_workspace(env);
    match &workspace {
        Err(err) => println!("ResolveWorkspaceError:\t\t{err:#}"),
        Ok((workspace, flake_dir)) => {
            println!("ResolvedWorkspace:\t\t{}", workspace.display());
            println!("ResolvedFlakeDir:\t\t{}", flake_dir.display());
            println!("FileFlakeNixExists:\t\t{}", flake_dir.join("flake.nix").is_file());
            println!("FileConfigTomlExists:\t\t{}", flake_dir.join(CONFIG_TOML).is_file());
            println!("FileLocalConfigTomlExists:\t{}", flake_dir.join(CONFIG_LOCAL_TOML).is_file());
            println!("FileFlakeLockExists:\t\t{}", flake_dir.join("flake.lock").is_file());
            match list_instance_ids(env) {
                Err(err) => println!("ListInstanceIdsError:\t\t{err:#}"),
                Ok(ids) => println!("InstanceIds:\n\t{}", if ids.is_empty() { "none".into() } else { ids.join("\n\t") }),
            }
        }
    }

    let instance = workspace.ok().map(|_| resolve_instance(env));
    match instance {
        None => println!("InstanceId:\t\t\tN/A"),
        Some(Err(err)) => println!("ResolveInstanceError:\t\t{err:#}"),
        Some(Ok(instance)) => {
            println!("InstanceId:\t\t\t{}", instance.id);
            println!("InstanceIsGlobal:\t\t{}", instance.is_global);
            println!("InstanceDataDir:\t\t{}", instance.data_dir.display());
            println!("InstanceRootfsDir:\t\t{}", instance.rootfs.display());
            println!("InstanceSystemDir:\t\t{}", instance.system.display());
            println!("InstanceUserDir:\t\t{}", instance.user.display());
            println!("InstanceStateDir:\t\t{}", instance.state_dir.display());
            println!("InstanceLogsDir:\t\t{}", instance.logs_dir.display());
            println!("InstanceRuntimeDir:\t\t{}", instance.runtime_dir.display());
            match read_port_forwards_lookup(&instance, None, None) {
                Ok((forwards, _)) => println!(
                    "InstancePortForwards:\n{}",
                    if forwards.is_empty() {
                        "\tnone".into()
                    } else {
                        forwards
                            .iter()
                            .map(|(name, f)| {
                                let end = f.host_end.unwrap_or(f.host);
                                format!("\t{name}\t{}\t{}:{}-{end}\t{}", f.proto, f.address, f.host, f.guest)
                            })
                            .collect::<Vec<String>>()
                            .join("\n")
                    }
                ),
                Err(err) => println!("ReadPortForwardsLookupError:\t{err:#}"),
            }
        }
    }
    Ok(())
}

#[inline(never)]
fn resolve_workspace(env: &Env) -> anyhow::Result<(PathBuf, PathBuf)> {
    if !env.is_global {
        let mut dir = env.search_dir.clone();
        loop {
            if dir.join(LOCAL_CONFIG_DIR).is_dir() {
                return Ok((dir.clone(), dir.join(LOCAL_CONFIG_DIR)));
            }
            let direct_config = dir.file_name().and_then(|p| p.to_str()) != Some(LOCAL_CONFIG_DIR)
                && (dir.join(CONFIG_TOML).is_file() || dir.join(CONFIG_LOCAL_TOML).is_file());
            if direct_config {
                return Ok((dir.clone(), dir));
            }
            if !dir.pop() {
                break;
            }
        }
    }
    let global_flake_dir = env.config_root.join(env.project_name.as_deref().unwrap_or(APP_NAME));
    if global_flake_dir.is_dir() {
        Ok((global_flake_dir.clone(), global_flake_dir))
    } else {
        bail!("{} not found. Try `devvm init` to start in a new project.", global_flake_dir.display())
    }
}

#[inline(never)]
fn resolve_instance(env: &Env) -> anyhow::Result<Instance> {
    let (workspace, flake_dir) = resolve_workspace(env)?;
    let project_name = env
        .project_name
        .clone()
        .unwrap_or_else(|| workspace.file_name().and_then(|name| name.to_str()).unwrap_or(APP_NAME).to_owned());
    let id = format!("{}[{}]", project_name, env.hostname);
    let data_dir = env.data_root.join(&id);
    let state_dir = env.state_root.join(&id);
    Ok(Instance {
        runtime_dir: env.runtime_root.join(&id),
        id,
        is_global: flake_dir == env.config_root.join(&project_name),
        project_name,
        workspace,
        flake_dir,
        rootfs: data_dir.join("rootfs"),
        system: data_dir.join("system"),
        user: data_dir.join("user"),
        logs_dir: state_dir.join("logs"),
        data_dir,
        state_dir,
    })
}

type PortLookup = (BTreeMap<String, PortForward>, Option<(String, u16)>);

fn read_port_forwards_lookup(instance: &Instance, guest_port: Option<u16>, protocol: Option<&str>) -> anyhow::Result<PortLookup> {
    let port_forwards_path = instance.runtime_dir.join("port-forwards");
    let forwards: BTreeMap<String, PortForward> = fs::read_to_string(&port_forwards_path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_else(|| {
            BTreeMap::from([(
                "ssh".into(),
                PortForward {
                    proto: "tcp".to_owned(),
                    address: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    dev: None,
                    host: 2223,
                    host_end: Some(2223),
                    guest: 22,
                },
            )])
        });
    match guest_port {
        Some(guest_port) => {
            for (_, f) in forwards {
                if protocol.is_some_and(|proto| f.proto != proto) {
                    continue;
                }
                if let Some(host_port) = guest_port.checked_sub(f.guest).and_then(|offset| f.host.checked_add(offset))
                    && host_port <= f.host_end.unwrap_or(f.host)
                {
                    return Ok((BTreeMap::new(), Some((f.address.to_string(), host_port))));
                }
            }
            Ok((BTreeMap::new(), None))
        }
        None => Ok((forwards, None)),
    }
}

pub fn parse_config(config_toml: &str, local_toml: Option<&str>, host: &str, _warn_allowed_hosts: bool) -> anyhow::Result<Config> {
    let config: DocumentMut = config_toml.parse().context(format!("parse {CONFIG_TOML}"))?;
    let local: DocumentMut = local_toml.unwrap_or("").parse().context(format!("parse {CONFIG_LOCAL_TOML}"))?;
    let has_domain_xml = |v: &Item| v.get("vm").and_then(|vm| vm.get("allowDomainXml")).is_some();
    if has_domain_xml(config.as_item())
        || config
            .get("hosts")
            .and_then(Item::as_table)
            .into_iter()
            .flat_map(|table| table.iter().map(|(_, item)| item))
            .any(has_domain_xml)
    {
        bail!("allowDomainXml is only allowed in {}", CONFIG_LOCAL_TOML);
    }
    let config_host = config.get("hosts").and_then(|hosts| hosts.get(host));
    let local_host = local.get("hosts").and_then(|hosts| hosts.get(host));

    /* // TODO: add allow-domain option to accept new domains, then add an instruction to use it.
    if warn_allowed_hosts && !local.is_empty() {
        let names = |roots: [Option<&Item>; 2]| -> BTreeSet<String> {
            roots
                .into_iter()
                .flatten()
                .flat_map(|v| v.get("allowedHosts").and_then(Item::as_table).into_iter())
                .flat_map(|table| table.iter().map(|(key, _)| key.to_owned()))
                .collect()
        };
        for name in names([Some(config.as_item()), config_host]).difference(&names([Some(local.as_item()), local_host])) {
            eprintln!("warning: {CONFIG_TOML} allows allowedHosts.{name:?}; set it to false in {CONFIG_LOCAL_TOML} to disable");
        }
    }*/

    fn merge(dst: &mut dyn TableLike, src: &dyn TableLike) {
        for (key, value) in src.iter() {
            if key == "hosts" {
                continue;
            }
            match (dst.get_mut(key).and_then(Item::as_table_like_mut), value.as_table_like()) {
                (Some(dst), Some(src)) => merge(dst, src),
                _ => {
                    dst.insert(key, value.clone());
                }
            }
        }
    }

    let mut out = DocumentMut::new();
    for src in [
        Some(config.as_table() as &dyn TableLike),
        Some(local.as_table() as &dyn TableLike),
        config_host.and_then(Item::as_table_like),
        local_host.and_then(Item::as_table_like),
    ]
    .into_iter()
    .flatten()
    {
        merge(out.as_table_mut(), src);
    }
    toml_edit::de::from_document(out).map_err(Into::into)
}

/// Host CA bundle for [`Vm::use_host_certs`]: `SSL_CERT_FILE` then well-known distro paths (`nix-profile.sh.in` order). `is_file()` only.
pub fn resolve_host_ca_bundle() -> Option<PathBuf> {
    std::env::var_os("SSL_CERT_FILE")
        .into_iter()
        .chain(
            [
                "/etc/ssl/certs/ca-certificates.crt",
                "/etc/ssl/ca-bundle.pem",
                "/etc/ssl/certs/ca-bundle.crt",
                "/etc/pki/tls/certs/ca-bundle.crt",
            ]
            .map(OsString::from),
        )
        .find(|path| Path::new(path).is_file())
        .map(PathBuf::from)
}

fn read_optional(path: &Path) -> std::io::Result<String> {
    fs::read_to_string(path).or_else(|err| (err.kind() == std::io::ErrorKind::NotFound).then(String::new).ok_or(err))
}

fn render_domain_xml(env: &Env, instance: &Instance, system_profile: &Path, is_build: bool) -> anyhow::Result<(String, BTreeMap<String, PortForward>)> {
    fn find_or_append_mut<'a>(parent: &'a mut XmlElement, tag: &'a str) -> &'a mut XmlElement {
        if parent.find(tag).is_none() {
            parent.append_new_child(tag);
        }
        parent.find_mut(tag).unwrap()
    }

    let config_toml = fs::read_to_string(instance.flake_dir.join(CONFIG_TOML)).context(format!("read {CONFIG_TOML}"))?;
    let local_toml = read_optional(&instance.flake_dir.join(CONFIG_LOCAL_TOML)).context(format!("read {CONFIG_LOCAL_TOML}"))?;
    let config = parse_config(&config_toml, Some(&local_toml), &env.hostname, true)?;

    let base_xml = config.vm.libvirt_domain_xml.as_deref().unwrap_or(DEFAULT_DOMAIN_XML);
    if config.vm.libvirt_domain_xml.is_some() {
        let domain_xml_hash = format!(
            "sha256:{}",
            Sha256::digest(base_xml.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>()
        );
        let domain_xml_allowed = match &config.vm.allow_domain_xml {
            Some(PolicyEntry::Remove(true)) => true,
            Some(PolicyEntry::Set(approved)) => approved == &domain_xml_hash,
            _ => false,
        };
        if !domain_xml_allowed {
            bail!(
                "custom vm.libvirtDomainXml is not approved for this host.\n\
                 To use the current XML, review it and record its hash in {CONFIG_LOCAL_TOML}:\n\n\
                 [hosts.{}.vm]\n\
                 allowDomainXml = {domain_xml_hash:?}",
                env.hostname
            );
        }
    }

    let mut forwards = BTreeMap::new();
    for (name, entry) in &config.port_forwards {
        let PolicyEntry::Set(forward) = entry else {
            continue;
        };
        if forward.proto != "tcp" && forward.proto != "udp" {
            bail!("invalid portForwards.{name}.proto: expected tcp or udp");
        }
        let host_end = forward.host_end.unwrap_or(forward.host);
        if host_end < forward.host {
            bail!("invalid portForwards.{name}: host_end is before host");
        }
        let count = host_end - forward.host;
        if forward.guest.checked_add(count).is_none() {
            bail!("invalid portForwards.{name}: guest port range overflows u16");
        }
        forwards.insert(
            name.clone(),
            PortForward {
                proto: forward.proto.clone(),
                address: forward.address,
                dev: forward.dev.clone(),
                host: forward.host,
                host_end: Some(host_end),
                guest: forward.guest,
            },
        );
    }
    let memory_mib = config.vm.memory_mi_b.unwrap_or(8192);
    let vcpus = config.vm.vcpus.unwrap_or(4);
    let machine_id = Sha256::digest(instance.id.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..32]
        .to_owned();
    let domain_uuid = format!(
        "{}-{}-{}-{}-{}",
        &machine_id[..8],
        &machine_id[8..12],
        &machine_id[12..16],
        &machine_id[16..20],
        &machine_id[20..32]
    );
    let kernel_target = fs::read_link(system_profile.join("kernel")).context("read_link($profile/kernel)")?;
    let initrd_target = fs::read_link(system_profile.join("initrd")).context("read_link($profile/initrd)")?;
    let kernel = instance.system.join(kernel_target.strip_prefix("/").context("get kernel image path")?);
    let initrd = instance.system.join(initrd_target.strip_prefix("/").context("get initrd image path")?);
    let kernel_params = fs::read_to_string(system_profile.join("kernel-params")).context("read kernel-params")?;
    let build_unit = if is_build { " systemd.unit=devvm-build.target" } else { "" };
    let host_certs = if config.vm.use_host_certs.unwrap_or(false) {
        " devvm.use-host-certs"
    } else {
        ""
    };
    let cmdline = format!("{kernel_params} init=/nix/var/nix/profiles/system/init systemd.machine_id={machine_id}{build_unit}{host_certs}");

    let mut domain = XmlElement::from_reader(base_xml.as_bytes()).context("parse domain xml")?;
    if domain.tag().name() != "domain" {
        bail!("vm.libvirtDomainXml root element must be <domain>");
    }
    domain.retain_children(|child| !matches!(child.tag().name(), "name" | "uuid" | "memory"));
    domain.append_new_child("name").set_text(instance.id.clone());
    domain.append_new_child("uuid").set_text(domain_uuid);
    domain.append_new_child("memory").set_text((memory_mib * 1024).to_string());

    let vcpu = find_or_append_mut(&mut domain, "vcpu");
    vcpu.set_text(vcpus.to_string());

    let os = find_or_append_mut(&mut domain, "os");
    os.retain_children(|child| !matches!(child.tag().name(), "type" | "kernel" | "initrd" | "cmdline"));
    os.append_new_child("type")
        .set_text("hvm")
        .set_attr("arch", "x86_64")
        .set_attr("machine", "q35");
    os.append_new_child("kernel").set_text(kernel.display().to_string());
    os.append_new_child("initrd").set_text(initrd.display().to_string());
    os.append_new_child("cmdline").set_text(cmdline);

    let devices = find_or_append_mut(&mut domain, "devices");
    devices.retain_children(|element| {
        element.tag().name() != "interface"
            && !(element.tag().name() == "filesystem"
                && element
                    .find("target")
                    .and_then(|target| target.get_attr("dir"))
                    .is_some_and(|dir| dir == "rootfs" || dir == "system" || dir == "user"))
    });
    // TODO: Remove this. Host must set up the emulator.
    //if devices.find("emulator").is_none() {
    //    devices.append_new_child("emulator").set_text(resolve_cmd_path("qemu-system-x86_64"));
    //}

    // The supervisor owns both daemons; libvirt only connects QEMU to their sockets.
    for tag in ["system", "user"] {
        let filesystem = devices.append_new_child("filesystem");
        filesystem.set_attr("type", "mount").append_new_child("driver").set_attr("type", "virtiofs");
        filesystem
            .append_new_child("source")
            .set_attr("socket", instance.runtime_dir.join(format!("{tag}.sock")).display().to_string());
        filesystem.append_new_child("target").set_attr("dir", tag);
    }

    let interface = devices.append_new_child("interface");
    interface.set_attr("type", "user");
    interface.append_new_child("backend").set_attr("type", "passt");
    interface.append_new_child("model").set_attr("type", "virtio");
    for forward in forwards.values() {
        let host_end = forward.host_end.unwrap_or(forward.host);
        let port_forward = interface.append_new_child("portForward");
        port_forward
            .set_attr("proto", forward.proto.clone())
            .set_attr("address", forward.address.to_string());
        if let Some(dev) = &forward.dev {
            port_forward.set_attr("dev", dev.clone());
        }
        let range = port_forward.append_new_child("range");
        range.set_attr("start", forward.host.to_string());
        if host_end != forward.host {
            range.set_attr("end", host_end.to_string());
        }
        range.set_attr("to", forward.guest.to_string());
    }

    let mut xml = Vec::new();
    domain
        .to_writer_with_options(&mut xml, WriteOptions::new().set_xml_prolog(None))
        .context("serialize domain xml")?;
    Ok((String::from_utf8(xml).context("domain xml is not utf-8")?, forwards))
}

// Create the config dir and initial files for local/global init, or return a displayable error.
#[inline(never)]
fn run_init(env: &Env, force: bool) -> anyhow::Result<()> {
    let (target, source) = if env.is_global {
        let target = env.config_root.join(env.project_name.as_deref().unwrap_or(APP_NAME));
        let source = diff_paths(std::path::absolute(&env.search_dir)?, &target).context("resolve workspace mount source")?;
        (target, if source.as_os_str().is_empty() { PathBuf::from(".") } else { source })
    } else {
        (env.search_dir.join(LOCAL_CONFIG_DIR), PathBuf::from("."))
    };
    write_template_config(&target, &env.search_dir, &source, force)?;
    eprintln!("init: wrote template files to {}", target.display());
    Ok(())
}

fn write_template_config(target: &Path, workspace: &Path, source: &Path, force: bool) -> anyhow::Result<()> {
    if target.exists() && !force {
        bail!("{} already exists", target.display());
    }
    let workspace_name = workspace.file_name().and_then(|name| name.to_str()).context("derive workspace name")?;
    fs::create_dir_all(target.join(APP_NAME)).context("create devvm dir")?;
    let app_flake = format!("{APP_NAME}/flake.nix");
    let app_claude_nixos = format!("{APP_NAME}/claude-nixos.nix");
    let config_template = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/template/", env!("CARGO_PKG_NAME"), ".toml"));
    let mut config: DocumentMut = config_template.parse().context(format!("parse template {CONFIG_TOML}"))?;
    let mut workspace_mount = toml_edit::InlineTable::new();
    workspace_mount.insert("source", toml_edit::Value::from(source.display().to_string()));
    workspace_mount.insert("readonly", toml_edit::Value::from(false));
    config["mounts"][workspace_name] = toml_edit::value(workspace_mount);
    for (name, contents) in [
        (CONFIG_TOML, config.to_string()),
        ("flake.nix", include_str!("../template/flake.nix").to_owned()),
        ("configuration.nix", include_str!("../template/configuration.nix").to_owned()),
        (app_flake.as_str(), include_str!("../template/devvm/flake.nix").to_owned()),
        (app_claude_nixos.as_str(), include_str!("../template/devvm/claude-nixos.nix").to_owned()),
    ] {
        fs::write(target.join(name), contents).context("write template file")?;
    }
    Ok(())
}

#[inline(never)]
fn run_build_or_up(env: &Env, bootstrap: bool, is_up: bool, attach: bool, mut write_lock: bool) -> anyhow::Result<()> {
    let is_switch = is_up;
    let instance = resolve_instance(env)?;
    // Prepare the minimal instance directories before bootstrap, virtiofsd, or log writers touch them.
    for dir in [&instance.rootfs, &instance.system, &instance.user, &instance.logs_dir, &instance.runtime_dir] {
        fs::create_dir_all(dir).context("prepare instance directories")?;
    }
    if !instance.system.join("nix/var/nix/profiles/default").is_symlink() {
        if instance.system.join("nix").exists() {
            bail!("system Nix store exists without a default profile; destroy --system and retry");
        }
        if !instance.rootfs.join("nix/var/nix/profiles/default").is_symlink() {
            fetch_nix_dockerhub(&instance.rootfs).context("fetch")?;
        }
        // The image is only bootstrap scaffolding; the Nix store belongs to persistent system data.
        fs::rename(instance.rootfs.join("nix"), instance.system.join("nix")).context("move bootstrap Nix store into system data")?;
    }
    fs::create_dir_all(instance.rootfs.join("nix")).context("prepare bootstrap Nix mountpoint")?;
    if bootstrap || !instance.system.join("nix/var/nix/profiles/system").is_symlink() {
        install_initial_nixos_profile(&instance.workspace, &instance.rootfs, &instance.system, "default")?;
    }
    // Provide a minimum writable flake.lock for the initial build.
    if !instance.flake_dir.join("flake.lock").exists() {
        fs::write(instance.flake_dir.join("flake.lock"), r#"{"root":"","version":7}"#).context("write flake.lock")?;
        write_lock = true;
    }
    let rebuild = |action| rebuild_guest(env, &instance, action, write_lock);
    let mut domstate = domstate(&instance.id)?;
    // A domain cannot recover its external virtiofs connections after their supervisor disappears.
    if domstate == "running" && !instance.runtime_dir.join("control.sock").exists() {
        eprintln!("supervisor is missing; recreating the VM");
        virsh(&["destroy", &instance.id]).context("recover missing supervisor")?;
        domstate = "down".to_owned();
    }
    match domstate.as_str() {
        "down" | "shut off" | "crashed" => {
            start_vm(env, &instance, true)?;
            rebuild("boot")?;
            let new_profile = read_system_profile(&instance)?;
            println!("{}", new_profile.display());
            if !is_up {
                virsh(&["destroy", &instance.id]).context("return to down")?;
                stop_supervisor(&instance)?;
            } else {
                let domain_xml_path = instance.runtime_dir.join("domain.xml");
                let old_xml = fs::read_to_string(&domain_xml_path).unwrap_or_default();
                let (new_xml, forwards) = render_domain_xml(env, &instance, &new_profile, false)?;
                if old_xml != new_xml {
                    virsh(&["destroy", &instance.id]).context("restart")?;
                    stop_supervisor(&instance)?;
                    start_vm(env, &instance, false).context("restart")?;
                } else {
                    fs::write(&domain_xml_path, new_xml).context("write normalized runtime domain xml")?;
                    fs::write(instance.runtime_dir.join("port-forwards"), serde_json::to_string_pretty(&forwards)?).context("write runtime port-forwards")?;
                    run_ssh(env, &["systemctl", "isolate", "multi-user.target"], true, true).context("starting")?;
                }
            }
        }
        "running" => {
            let switch_or_boot = if is_switch { "switch" } else { "boot" };
            rebuild(switch_or_boot)?;
            let new_profile = read_system_profile(&instance)?;
            let old_xml = fs::read_to_string(instance.runtime_dir.join("domain.xml")).unwrap_or_default();
            let (new_xml, _) = render_domain_xml(env, &instance, &new_profile, false)?;
            if old_xml != new_xml {
                eprintln!("build: domain definition changed; please restart the VM for the changes to take effect");
            }
            if attach {
                run_ssh::<&str>(env, &[], false, true).context("attach")?;
            }
        }
        domstate => bail!("VM is {domstate}; expected running, down, shut off, or crashed"),
    };
    Ok(())
}

fn rebuild_guest(env: &Env, instance: &Instance, action: &str, write_lock: bool) -> anyhow::Result<()> {
    let flake = format!("{}#{}", supervisor_command(instance, "BuildOn")?, env.hostname);
    let args = ["nixos-rebuild", action, "--flake", flake.as_str()]
        .into_iter()
        .chain((!write_lock).then_some("--no-write-lock-file"))
        .collect::<Vec<_>>();
    let rebuild = run_ssh(env, &args, true, true);
    let build_off = supervisor_command(instance, "BuildOff").map(|_| ());
    rebuild.and(build_off)
}

#[inline(never)]
fn fetch_nix_dockerhub(rootfs: &Path) -> anyhow::Result<()> {
    let repo = "nixos/nix";
    let registry = "https://registry-1.docker.io/v2";
    eprintln!("fetch: requesting docker auth token for {repo}");
    let client = Client::builder().build()?;
    let token = client
        .get(format!("https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:pull"))
        .send()?;
    let token = token.error_for_status()?.json::<Value>()?;
    let token = token["token"].as_str().context("docker token missing")?;
    let auth = format!("Bearer {token}");
    eprintln!("fetch: resolving image manifest list (latest)");
    let manifests = client.get(format!("{registry}/{repo}/manifests/latest")).header(header::AUTHORIZATION, &auth);
    let manifests = manifests.send()?.error_for_status()?.json::<Value>()?;
    let digest = manifests["manifests"]
        .as_array()
        .context("docker manifest list missing")?
        .iter()
        .find(|manifest| manifest["platform"]["architecture"] == "amd64" && manifest["platform"]["os"] == "linux")
        .context("linux/amd64 docker manifest missing")?;
    let digest = digest["digest"].as_str().context("linux/amd64 docker manifest missing")?;
    eprintln!("fetch: selected linux/amd64 image digest {digest}");
    let manifest = client.get(format!("{registry}/{repo}/manifests/{digest}")).header(header::AUTHORIZATION, &auth);
    let manifest = manifest.send()?.error_for_status()?.json::<Value>()?;
    let layers = manifest["layers"].as_array().context("docker layers missing")?;
    eprintln!("fetch: extracting {} layers into {}", layers.len(), rootfs.display());
    for (index, blob) in layers.iter().filter_map(|layer| layer["digest"].as_str()).enumerate() {
        eprintln!("fetch: layer {}/{} {}", index + 1, layers.len(), blob);
        let response = client.get(format!("{registry}/{repo}/blobs/{blob}")).header(header::AUTHORIZATION, &auth);
        let response = response.send()?.error_for_status()?;
        tar::Archive::new(GzDecoder::new(response)).unpack(rootfs)?;
    }
    eprintln!("fetch: completed docker image extraction");
    Ok(())
}

// The initial profile is assumed safe, so building it in a simple container is acceptable.
#[inline(never)]
fn install_initial_nixos_profile(workspace: &Path, rootfs: &Path, system: &Path, hostname: &str) -> anyhow::Result<()> {
    let config_target = rootfs.join("etc/nixos");
    eprintln!("install: writing template config into {}", config_target.display());
    write_template_config(&config_target, workspace, Path::new("."), true)?;

    // Remove previous out-link so `nix build --out-link /nix/var/nix/profiles/system` can overwrite it.
    let _ = fs::remove_file(system.join("nix/var/nix/profiles/system"));
    spawn_mapped_namespace(true, true, || {
        (|| -> anyhow::Result<()> {
            // Prepare new file system hierarchy.
            let oldroot = Path::new("/");
            eprintln!("install: binding system/nix to rootfs/nix");
            mount_bind_recursive(system.join("nix"), rootfs.join("nix"))?;
            eprintln!("install: creating mountpoint dir rootfs/dev");
            fs::create_dir_all(rootfs.join("dev"))?;
            eprintln!("install: mounting tmpfs to rootfs/dev");
            mount("tmpfs", rootfs.join("dev"), "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, c"mode=0755")?;
            for dir in ["dev/shm", "dev/pts", "proc", "tmp"] {
                eprintln!("install: creating mountpoint dir rootfs/{dir}");
                fs::create_dir_all(rootfs.join(dir))?;
            }
            // Python's _multiprocessing.SemLock expects a writable 1777 /dev/shm.
            eprintln!("install: mounting tmpfs to rootfs/dev/shm");
            let shm_opts = c"mode=01777";
            mount("tmpfs", rootfs.join("dev/shm"), "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, shm_opts)?;
            eprintln!("install: mounting devpts to rootfs/dev/pts");
            let opts = c"newinstance,ptmxmode=0666,mode=620";
            mount("devpts", rootfs.join("dev/pts"), "devpts", MountFlags::NOSUID | MountFlags::NOEXEC, opts)?;
            eprintln!("install: binding host /proc to rootfs/proc");
            mount_bind_recursive(oldroot.join("proc"), rootfs.join("proc"))?;
            // Bind host devices etc to new root's /dev.
            for file in ["dev/null", "dev/zero", "dev/full", "dev/random", "dev/urandom", "dev/tty"] {
                eprintln!("install: touching rootfs/{file} and binding host /{file}");
                fs::write(rootfs.join(file), "")?;
                mount_bind_recursive(oldroot.join(file), rootfs.join(file))?;
            }
            eprintln!("install: touching and binding host /etc/resolv.conf read-only");
            fs::write(rootfs.join("etc/resolv.conf"), "")?;
            let flags = MountFlags::BIND | MountFlags::REC | MountFlags::RDONLY;
            mount(oldroot.join("etc/resolv.conf"), rootfs.join("etc/resolv.conf"), "", flags, c"")?;
            eprintln!("install: creating symlinks for /dev/{{stdin,stdout,stderr,fd,core,ptmx}}");
            for (fd, file) in [(0, "dev/stdin"), (1, "dev/stdout"), (2, "dev/stderr")] {
                symlink(format!("/proc/self/fd/{fd}"), rootfs.join(file))?;
            }
            symlink("/proc/self/fd", rootfs.join("dev/fd"))?;
            symlink("/proc/kcore", rootfs.join("dev/core"))?;
            symlink("pts/ptmx", rootfs.join("dev/ptmx"))?;

            eprintln!("install: pivoting root: / => (rootfs)/tmp, rootfs => /");
            // pivot_root() new_root must be a mountpoint. Bind rootfs to itself.
            mount_bind_recursive(rootfs, rootfs)?;
            // Pivot root: / => (rootfs)/tmp, rootfs => /.
            pivot_root(rootfs, rootfs.join("tmp"))?;
            eprintln!("install: detaching host / (currently pivoted to /tmp)");
            unmount("/tmp", UnmountFlags::DETACH)?; // Unmount old root.
            eprintln!("install: cd to the brand new root / (rootfs)");
            chdir("/")?;
            // Err(process::Command::new("cat").args(["/proc/self/mountinfo"]).exec().to_string())

            eprintln!("install: execing nix build for hostname={hostname}");
            bail!(
                process::Command::new("/nix/var/nix/profiles/default/bin/nix")
                    .args(["build", &format!("/etc/nixos#nixosConfigurations.{hostname}.config.system.build.toplevel")])
                    .args(["--extra-experimental-features", "nix-command flakes"])
                    .args(["--option", "ssl-cert-file", "/etc/ssl/certs/ca-bundle.crt"])
                    .args(["--option", "max-jobs", "auto"])
                    .args(["--out-link", "/nix/var/nix/profiles/system"])
                    .exec()
                    .to_string()
            )
        })()
    })
    .context("install")?;
    Ok(())
}

#[inline(never)]
fn domstate(instance_id: &str) -> anyhow::Result<String> {
    // The supervisor is namespace root, so pin libvirt to the launcher's host-user session; use the ordinary URI only to preserve daemon autostart.
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required for libvirt session access")?;
    let socket = format!("{runtime_dir}/libvirt/virtqemud-sock");
    if !Path::new(&socket).exists() {
        let status = process::Command::new("virsh")
            .args(["-c", "qemu:///session", "uri"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .context("start libvirt session daemon")?;
        if !status.success() {
            bail!("failed to start libvirt session daemon");
        }
    }
    let uri = format!("qemu+unix:///session?socket={socket}");
    let output = process::Command::new("virsh")
        .args(["-c", &uri])
        .arg("domstate")
        .arg(instance_id)
        .output()
        .context("run virsh domstate")?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("failed to get domain") || stderr.contains("Domain not found") {
        Ok("down".to_owned())
    } else {
        bail!("{}", stderr.trim())
    }
}

fn domain_is_active(instance_id: &str) -> anyhow::Result<bool> {
    Ok(!matches!(domstate(instance_id)?.as_str(), "down" | "shut off" | "crashed"))
}

/// Read the NixOS profile to be used to start the VM next time.
fn read_system_profile(instance: &Instance) -> anyhow::Result<PathBuf> {
    let mut path = instance.system.join("nix/var/nix/profiles/system");
    for _ in 0..16 {
        let target = fs::read_link(&path).context("read system profile symlink")?;
        path = if target.is_absolute() {
            instance
                .system
                .join(target.strip_prefix("/").context("resolve absolute system profile symlink target")?)
        } else {
            path.parent().context("resolve relative system profile symlink parent")?.join(target)
        };
        if !fs::symlink_metadata(&path)
            .context("read resolved system profile metadata")?
            .file_type()
            .is_symlink()
        {
            return Ok(path);
        }
    }
    bail!("system profile symlink chain is too deep")
}

/// Run virsh against the session URI (qemu:///session).
///
/// Networking uses `<backend type='passt'/>`; libvirt looks up `passt` on the
/// user-session libvirtd PATH, not the shell that launched devvm. NixOS
/// `systemd.services.libvirtd.path` does not apply here. If passt was installed
/// or PATH changed after the session daemon started, restart it (or kill stale
/// libvirt/qemu/passt children) so the daemon picks up the current PATH.
#[inline(never)]
fn virsh(args: &[&str]) -> anyhow::Result<()> {
    // This may run as namespace root; keep all operations in the launcher's host-user libvirt session.
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required for libvirt session access")?;
    let uri = format!("qemu+unix:///session?socket={runtime_dir}/libvirt/virtqemud-sock");
    let status = process::Command::new("virsh")
        .args(["-c", &uri])
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("run virsh")?;
    if !status.success() {
        bail!(
            "virsh failed with status {}",
            status.code().map_or("signal".to_owned(), |code| code.to_string())
        );
    }
    Ok(())
}

fn list_instance_ids(env: &Env) -> anyhow::Result<Vec<String>> {
    let project_name = resolve_instance(env)?.project_name;
    Ok(fs::read_dir(&env.data_root)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            entry.file_type().ok()?.is_dir().then(|| entry.file_name().into_string().ok())?
        })
        .filter(|id| id.rsplit_once('[').map(|(name, _)| name) == Some(project_name.as_str()))
        .collect())
}

fn spawn_mapped_namespace<F>(map_root: bool, wait_child: bool, child: F) -> anyhow::Result<Pid>
where
    F: FnOnce() -> anyhow::Result<()>,
{
    let uid_map = capture_host_idmap("/proc/self/uid_map", map_root)?;
    let gid_map = capture_host_idmap("/proc/self/gid_map", map_root)?;
    let (child_ready_read, child_ready_write) = pipe_with(PipeFlags::CLOEXEC).context("create child-to-parent setup pipe")?;
    let (parent_done_read, parent_done_write) = pipe_with(PipeFlags::CLOEXEC).context("create parent-to-child setup pipe")?;
    match unsafe { kernel_fork() }.context("fork namespace setup process")? {
        Fork::Child(_) => {
            drop(child_ready_read);
            drop(parent_done_write);
            let status = match (|| {
                unsafe { unshare_unsafe(UnshareFlags::NEWUSER | UnshareFlags::NEWNS) }.context("enter user+mount namespaces")?;
                if write(&child_ready_write, &[1]).context("send child namespace-ready notification")? != 1 {
                    bail!("failed to notify parent that child namespace is ready");
                }
                let mut byte = [0_u8; 1];
                if read(&parent_done_read, &mut byte).context("wait for parent idmap-written notification")? != 1 {
                    bail!("parent closed setup pipe before uid/gid maps were written");
                }
                child()
            })() {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!("{err:#}");
                    1
                }
            };
            exit_group(status)
        }
        Fork::ParentOf(child_pid) => {
            drop(child_ready_write);
            drop(parent_done_read);
            let parent_error = (|| {
                let mut byte = [0_u8; 1];
                if read(&child_ready_read, &mut byte).context("wait for child namespace-ready notification")? != 1 {
                    bail!("child closed setup pipe before entering namespace setup");
                }
                for (kind, id_map) in [("uid", &uid_map), ("gid", &gid_map)] {
                    let status = process::Command::new(format!("new{kind}map"))
                        .arg(child_pid.as_raw_pid().to_string())
                        .args(id_map.split_whitespace().map(str::to_owned))
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .status()
                        .context(format!("run new{kind}map"))?;
                    if !status.success() {
                        bail!(
                            "new{kind}map failed with status {}",
                            status.code().map_or("signal".to_owned(), |code| code.to_string())
                        );
                    }
                }
                if write(&parent_done_write, &[1]).context("send parent idmap-written notification")? == 1 {
                    Ok(())
                } else {
                    bail!("failed to notify child that uid/gid maps are written")
                }
            })();
            drop(parent_done_write);
            if let Err(err) = parent_error {
                let _ = waitpid(Some(child_pid), WaitOptions::empty());
                Err(err)
            } else if wait_child {
                let status = waitpid(Some(child_pid), WaitOptions::empty())
                    .context("wait namespace setup child process")?
                    .ok_or_else(|| anyhow::anyhow!("child disappeared before waitpid reported status"))?
                    .1;
                match (status.exit_status(), status.terminating_signal()) {
                    (Some(0), _) => Ok(child_pid),
                    (Some(code), _) => bail!("child exited with status {code}"),
                    (None, Some(signal)) => bail!("child terminated by signal {signal}"),
                    (None, None) => bail!("child ended unexpectedly: {status:?}"),
                }
            } else {
                Ok(child_pid)
            }
        }
    }
}

#[derive(Clone)]
struct MountMapping {
    source: PathBuf,
    target: PathBuf,
    is_dir: bool,
}

fn start_vm(env: &Env, instance: &Instance, is_build: bool) -> anyhow::Result<PathBuf> {
    let system_profile = read_system_profile(instance)?;
    let system_socket = instance.runtime_dir.join("system.sock");
    let user_socket = instance.runtime_dir.join("user.sock");
    let control_socket = instance.runtime_dir.join("control.sock");
    let lock_path = instance.runtime_dir.join("lock");
    let pid_path = instance.runtime_dir.join("devvm.pid");
    let domain_profile = instance.runtime_dir.join("domain-profile");
    let (mut parent_sock, mut child_sock) = UnixStream::pair().context("create supervisor startup socket")?;
    let supervisor_pid = spawn_mapped_namespace(true, false, || -> anyhow::Result<()> {
        let mut daemons = Vec::new();
        let result = (|| -> anyhow::Result<()> {
            setsid().context("detach supervisor session")?;
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .context("open supervisor lock")?;
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == -1 {
                return Err(std::io::Error::last_os_error()).context("lock instance runtime");
            }
            for path in [&system_socket, &user_socket, &control_socket, &pid_path, &domain_profile] {
                let _ = fs::remove_file(path);
            }
            let downstream_rec = MountPropagationFlags::DOWNSTREAM | MountPropagationFlags::REC;
            mount_change("/", downstream_rec).context("set / mount propagation downstream+rec")?;
            // Propagate later bind mounts below either export into virtiofsd's child mount namespace.
            for (name, root) in [("system", &instance.system), ("user", &instance.user)] {
                mount_bind_recursive(root, root).with_context(|| format!("self bind {name} dir"))?;
                mount_change(root, MountPropagationFlags::SHARED | MountPropagationFlags::REC).with_context(|| format!("set {name} mount shared+rec"))?;
            }

            let mut mounted = Vec::new();
            let mut build_mounted = Vec::new();
            let mut build_active = false;
            apply_user_mounts(env, instance, &mut mounted).context("apply configured mounts")?;

            let control = UnixListener::bind(&control_socket).context("bind control socket")?;
            control.set_nonblocking(true).context("set control socket nonblocking")?;
            let system = UnixListener::bind(&system_socket).context("bind system virtiofs socket")?;
            let user = UnixListener::bind(&user_socket).context("bind user virtiofs socket")?;
            fcntl_setfd(&system, FdFlags::empty()).context("keep system virtiofs socket fd across exec")?;
            fcntl_setfd(&user, FdFlags::empty()).context("keep user virtiofs socket fd across exec")?;
            fs::write(&pid_path, format!("{}\n", process::id())).context("write supervisor pid")?;

            // Keep the highest mapped ID as a guest-inaccessible scratch slot for owner translation.
            let mut reserved = [0; 2];
            for (limit, path) in reserved.iter_mut().zip(["/proc/self/uid_map", "/proc/self/gid_map"]) {
                for line in fs::read_to_string(path)?.lines() {
                    let mut fields = line.split_whitespace();
                    let start = fields.next().context("missing namespace ID")?.parse::<u32>()?;
                    let _parent = fields.next().context("missing parent ID")?.parse::<u32>()?;
                    let count = fields.next().context("missing ID map count")?.parse::<u32>()?;
                    let end = start.checked_add(count).and_then(|end| end.checked_sub(1)).context("ID map range overflow")?;
                    *limit = (*limit).max(end);
                }
            }
            let [uid_reserved, gid_reserved] = reserved;
            // nixbld1 is 30001, so the scratch slot must be above it.
            if uid_reserved <= 30_001 || gid_reserved <= 30_001 {
                bail!("at least 30003 mapped UIDs and GIDs are required");
            }
            for (name, shared_dir, listener) in [("system", &instance.system, &system), ("user", &instance.user, &user)] {
                let mut command = process::Command::new("virtiofsd");
                command
                    .args(["--shared-dir", &shared_dir.display().to_string()])
                    .args(["--fd", &listener.as_raw_fd().to_string()])
                    .args(["--sandbox", "namespace", "--cache", "auto", "--xattr", "--log-level", "error"])
                    .uid(0)
                    .gid(0)
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
                if name == "user" {
                    // Guest root and the normal guest owner both become the real host owner;
                    // the forbidden scratch ID prevents ambiguity in the reverse translation.
                    for (option, owner, reserved) in [("--translate-uid", 1000, uid_reserved), ("--translate-gid", 100, gid_reserved)] {
                        for mapping in [
                            "squash-guest:0:0:1".to_owned(),
                            format!("squash-guest:{owner}:0:1"),
                            format!("host:0:{owner}:1"),
                            format!("host:{owner}:{reserved}:1"),
                            format!("forbid-guest:{reserved}:1"),
                        ] {
                            command.arg(option).arg(mapping);
                        }
                    }
                }
                unsafe {
                    command.pre_exec(|| {
                        // Never leave an export daemon behind if its supervisor dies.
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                            Err(std::io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    });
                }
                daemons.push((name, command.spawn().with_context(|| format!("spawn {name} virtiofsd"))?));
            }
            drop(system);
            drop(user);
            for (name, daemon) in &mut daemons {
                if let Some(status) = daemon.try_wait().with_context(|| format!("poll {name} virtiofsd"))? {
                    bail!("{name} virtiofsd exited before ready: {status}");
                }
            }
            child_sock.write_all(&[1]).context("notify launcher that supervisor is ready")?;
            let mut commit = [0_u8; 1];
            // Stay alive only after libvirt accepts a domain that consumes the export sockets.
            if child_sock.read_exact(&mut commit).is_err() || commit[0] != 1 {
                bail!("launcher exited before startup commit");
            }
            let _ = child_sock.shutdown(std::net::Shutdown::Both);

            let mut ticks = 0_u8;
            'supervise: loop {
                for (name, daemon) in &mut daemons {
                    if let Some(status) = daemon.try_wait().with_context(|| format!("poll {name} virtiofsd"))? {
                        if !domain_is_active(&instance.id)? {
                            break 'supervise;
                        }
                        bail!("{name} virtiofsd exited unexpectedly: {status}");
                    }
                }
                match control.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
                        let mut command = String::new();
                        BufReader::new(stream.try_clone()?).read_line(&mut command)?;
                        let reply = match command.trim() {
                            "Reload" if build_active => Err(anyhow::anyhow!("build mount is active")),
                            "Reload" => apply_user_mounts(env, instance, &mut mounted).map(|_| String::new()),
                            "BuildOn" if build_active => Err(anyhow::anyhow!("build mount is already active")),
                            "BuildOn" => mount_build_source(instance, &mut build_mounted).inspect(|_| {
                                build_active = true;
                            }),
                            "BuildOff" => {
                                clear_mounts(&mut build_mounted)?;
                                build_active = false;
                                apply_user_mounts(env, instance, &mut mounted)?;
                                Ok(String::new())
                            }
                            "Stop" => {
                                writeln!(stream, "OK")?;
                                break 'supervise;
                            }
                            other => Err(anyhow::anyhow!("unknown control command: {other}")),
                        };
                        match reply {
                            Ok(value) if value.is_empty() => writeln!(stream, "OK")?,
                            Ok(value) => writeln!(stream, "OK {value}")?,
                            Err(err) => writeln!(stream, "ERR {}", format!("{err:#}").replace('\n', ": "))?,
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(err) => return Err(err).context("accept control connection"),
                }
                ticks = ticks.wrapping_add(1);
                if ticks == 10 {
                    ticks = 0;
                    if !domain_is_active(&instance.id)? {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            clear_mounts(&mut build_mounted)?;
            clear_mounts(&mut mounted)?;
            drop(lock);
            Ok(())
        })();
        for (_, mut daemon) in daemons {
            if daemon.try_wait().ok().flatten().is_none() {
                let _ = kill_process(Pid::from_raw(daemon.id() as i32).expect("child pid is nonzero"), Signal::TERM);
            }
            let _ = daemon.wait();
        }
        for path in [&control_socket, &system_socket, &user_socket, &pid_path, &lock_path] {
            let _ = fs::remove_file(path);
        }
        result
    })
    .context("start supervisor")?;
    drop(child_sock);
    let mut ready = [0_u8; 1];
    parent_sock.read_exact(&mut ready).context("wait for supervisor readiness")?;

    let mut created = false;
    let transaction = (|| -> anyhow::Result<()> {
        let xml_path = instance.runtime_dir.join("domain.xml");
        let (domain_xml, forwards) = render_domain_xml(env, instance, &system_profile, is_build)?;
        fs::write(&xml_path, domain_xml).context("write generated domain xml")?;
        fs::write(instance.runtime_dir.join("port-forwards"), serde_json::to_string_pretty(&forwards)?).context("write runtime port-forwards")?;
        let create = process::Command::new("virsh")
            .arg("create")
            .arg(&xml_path)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("run virsh create")?;
        if !create.success() {
            bail!(
                "virsh create failed with status {}",
                create.code().map_or("signal".to_owned(), |code| code.to_string())
            );
        }
        created = true;
        symlink(&system_profile, domain_profile).context("write runtime domain-profile symlink")?;
        parent_sock.write_all(&[1]).context("commit supervisor startup")
    })();
    if let Err(err) = transaction {
        if created {
            let _ = virsh(&["destroy", &instance.id]);
        }
        drop(parent_sock);
        let _ = waitpid(Some(supervisor_pid), WaitOptions::empty());
        return Err(err);
    }
    Ok(system_profile)
}

fn apply_user_mounts(env: &Env, instance: &Instance, mounted: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    clear_mounts(mounted)?;
    let workspace_dir = instance.user.join("workspace");
    let config_dir = instance.user.join("config");
    let host_ca_target = instance.user.join("host-ca.crt");
    let config_toml = fs::read_to_string(instance.flake_dir.join(CONFIG_TOML)).context(format!("read {CONFIG_TOML}"))?;
    let local_toml = read_optional(&instance.flake_dir.join(CONFIG_LOCAL_TOML)).context(format!("read {CONFIG_LOCAL_TOML}"))?;
    let config = parse_config(&config_toml, Some(&local_toml), &env.hostname, false)?;
    let mut mappings = Vec::new();

    for (name, entry) in config.mounts {
        let PolicyEntry::Set(mount_config) = entry else {
            continue;
        };
        validate_mount_source_field(&mount_config.source)?;
        let source = if Path::new(&mount_config.source).is_absolute() {
            PathBuf::from(&mount_config.source)
        } else {
            instance.workspace.join(&mount_config.source)
        }
        .canonicalize()
        .with_context(|| format!("canonicalize mount source {}", mount_config.source))?;
        validate_mount_name_field(&name)?;
        mount_mapping(
            MountMapping {
                is_dir: source.is_dir(),
                source,
                target: workspace_dir.join(name),
            },
            !mount_config.readonly.unwrap_or(false),
            mounted,
            &mut mappings,
        )?;
    }
    mount_mapping(
        MountMapping {
            source: instance.flake_dir.canonicalize().context("canonicalize config dir")?,
            target: config_dir,
            is_dir: true,
        },
        true,
        mounted,
        &mut mappings,
    )?;
    if config.vm.use_host_certs.unwrap_or(false) {
        mount_mapping(
            MountMapping {
                source: resolve_host_ca_bundle().context("vm.useHostCerts is enabled, but no host CA bundle was found")?,
                target: host_ca_target,
                is_dir: false,
            },
            false,
            mounted,
            &mut mappings,
        )?;
    }
    protect_active_config(instance, &mappings, mounted)
}

fn mount_mapping(mapping: MountMapping, writable: bool, mounted: &mut Vec<PathBuf>, mappings: &mut Vec<MountMapping>) -> anyhow::Result<()> {
    if !mapping.source.is_dir() && !mapping.source.is_file() {
        bail!("mount source is neither file nor directory: {}", mapping.source.display());
    }
    if mapping.is_dir {
        fs::create_dir_all(&mapping.target).context("create mount target dir")?;
    } else {
        if let Some(parent) = mapping.target.parent() {
            fs::create_dir_all(parent).context("create mount target parent")?;
        }
        if !mapping.target.exists() {
            fs::write(&mapping.target, "").context("create mount target file")?;
        }
    }
    if writable && mapping.is_dir {
        mount_bind_recursive(&mapping.source, &mapping.target).context("bind-mount dir")?;
    } else if writable {
        mount(&mapping.source, &mapping.target, "", MountFlags::BIND, c"").context("bind-mount file")?;
    } else {
        mount_readonly(&mapping.source, &mapping.target, mapping.is_dir)?;
    }
    mounted.push(mapping.target.clone());
    mappings.push(mapping);
    Ok(())
}

fn protect_active_config(instance: &Instance, mappings: &[MountMapping], mounted: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for config_name in [CONFIG_TOML, CONFIG_LOCAL_TOML] {
        let active = instance.flake_dir.join(config_name);
        if !active.exists() {
            continue;
        }
        if fs::metadata(&active)?.nlink() != 1 {
            bail!("refuse hard-linked config: {}", active.display());
        }
        let active = active.canonicalize().context("canonicalize active config")?;
        for mapping in mappings {
            let alias = if mapping.is_dir {
                active.strip_prefix(&mapping.source).ok().map(|relative| mapping.target.join(relative))
            } else {
                (active == mapping.source).then(|| mapping.target.clone())
            };
            if let Some(alias) = alias
                && alias.exists()
                && !mounted.contains(&alias)
            {
                mount_readonly(&active, &alias, false)?;
                mounted.push(alias);
            }
        }
    }
    Ok(())
}

fn mount_build_source(instance: &Instance, mounted: &mut Vec<PathBuf>) -> anyhow::Result<String> {
    clear_mounts(mounted)?;
    let Some((git_root, relative_config)) = resolve_git_build_source(instance)? else {
        return Ok("/persistent/home/config".to_owned());
    };
    let target = instance.user.join("build");
    let mut mappings = Vec::new();
    mount_mapping(
        MountMapping {
            source: git_root,
            target: target.clone(),
            is_dir: true,
        },
        true,
        mounted,
        &mut mappings,
    )?;
    protect_active_config(instance, &mappings, mounted)?;
    Ok(if relative_config.as_os_str().is_empty() {
        "/persistent/home/build".to_owned()
    } else {
        format!("/persistent/home/build/{}", relative_config.display())
    })
}

fn resolve_git_build_source(instance: &Instance) -> anyhow::Result<Option<(PathBuf, PathBuf)>> {
    let output = process::Command::new("git")
        .args(["-C", &instance.flake_dir.display().to_string(), "rev-parse", "--show-toplevel"])
        .output()
        .context("resolve git root")?;
    if !output.status.success() {
        return Ok(None);
    }
    let git_root = PathBuf::from(String::from_utf8(output.stdout)?.trim())
        .canonicalize()
        .context("canonicalize git root")?;
    let config = instance.flake_dir.canonicalize().context("canonicalize config dir")?;
    let relative = config.strip_prefix(&git_root).context("config dir is outside git root")?.to_path_buf();
    let flake = relative.join("flake.nix");
    let tracked = process::Command::new("git")
        .arg("-C")
        .arg(&git_root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(&flake)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("check tracked flake")?;
    Ok(tracked.success().then_some((git_root, relative)))
}

fn clear_mounts(mounted: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    mounted.sort_by_key(|path| path.components().count());
    mounted.dedup();
    for target in mounted.drain(..).rev() {
        if unmount(&target, UnmountFlags::DETACH).is_err() {
            continue;
        }
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_dir() => match fs::remove_dir(&target) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                Err(err) => return Err(err).with_context(|| format!("remove mountpoint {}", target.display())),
            },
            Ok(metadata) if metadata.is_file() && metadata.len() == 0 => fs::remove_file(&target)?,
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("stat mountpoint {}", target.display())),
        }
    }
    Ok(())
}

fn supervisor_command(instance: &Instance, command: &str) -> anyhow::Result<String> {
    let mut stream = UnixStream::connect(instance.runtime_dir.join("control.sock")).context("connect supervisor")?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    writeln!(stream, "{command}")?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    let response = response.trim();
    if response == "OK" {
        Ok(String::new())
    } else if let Some(value) = response.strip_prefix("OK ") {
        Ok(value.to_owned())
    } else if let Some(err) = response.strip_prefix("ERR ") {
        bail!("supervisor: {err}")
    } else {
        bail!("invalid supervisor reply: {response}")
    }
}

fn stop_supervisor(instance: &Instance) -> anyhow::Result<()> {
    let socket = instance.runtime_dir.join("control.sock");
    if !socket.exists() {
        return Ok(());
    }
    let request = supervisor_command(instance, "Stop");
    for _ in 0..100 {
        if !socket.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    request?;
    bail!("supervisor did not stop")
}

fn mount_readonly(source: &Path, target: &Path, recursive: bool) -> anyhow::Result<()> {
    let flags =
        OpenTreeFlags::OPEN_TREE_CLONE | OpenTreeFlags::OPEN_TREE_CLOEXEC | if recursive { OpenTreeFlags::AT_RECURSIVE } else { OpenTreeFlags::empty() };
    let tree = open_tree(CWD, source, flags).context("clone mount read-only")?;
    let attr = libc::mount_attr {
        attr_set: libc::MOUNT_ATTR_RDONLY,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    let flags = libc::AT_EMPTY_PATH | if recursive { libc::AT_RECURSIVE } else { 0 };
    // rustix wraps open_tree and move_mount, but not mount_setattr; only this operation uses libc directly.
    let result = unsafe { libc::syscall(libc::SYS_mount_setattr, tree.as_raw_fd(), c"".as_ptr(), flags, &attr, size_of_val(&attr)) };
    if result == -1 {
        return Err(std::io::Error::last_os_error()).context("mount read-only");
    }
    move_mount(&tree, c"", CWD, target, MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH).context("attach mount read-only")
}

/// Get compatible subuid/subgid maps from host.
fn capture_host_idmap(path: &str, map_root: bool) -> anyhow::Result<String> {
    let output = process::Command::new("unshare")
        .args(["--map-auto", if map_root { "--map-root-user" } else { "--map-current-user" }, "cat", path])
        .output()
        .context(format!("resolve host idmap from {path}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        bail!("unshare cat {path} failed: {}", String::from_utf8_lossy(&output.stderr).trim())
    }
}

fn validate_mount_name_field(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || !Path::new(value).components().all(|part| matches!(part, Component::Normal(_))) || value.contains(['\t', '\n']) {
        bail!("invalid mount name: expected a relative path without special components");
    }
    Ok(())
}

fn run_virsh_action(env: &Env, action: &str) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    virsh(&[action, &instance.id])?;
    if action == "shutdown" {
        run_wait(&instance, &["down", "shut off", "crashed"])?;
    }
    stop_supervisor(&instance)
}

fn run_virsh_action_all(env: &Env, action: &str) -> anyhow::Result<()> {
    for id in list_instance_ids(env)? {
        virsh(&[action, &id])?;
    }
    Ok(())
}

#[inline(never)]
fn run_destroy(env: &Env, system: bool, data: bool, logs: bool, conf: bool) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    let _ = virsh(&["destroy", &instance.id]);
    stop_supervisor(&instance)?;
    if instance.is_global && !env.is_global {
        bail!("destroy files for the non-project instance requires --global");
    }
    if system {
        spawn_mapped_namespace(false, true, || remove_dir_all_if_exists(&instance.rootfs)).context("destroy rootfs")?;
        spawn_mapped_namespace(false, true, || remove_dir_all_if_exists(&instance.system)).context("destroy system data")?;
    }
    if data {
        spawn_mapped_namespace(true, true, || remove_dir_all_if_exists(&instance.user)).context("destroy user data")?;
    }
    if system && data {
        remove_dir_all_if_exists(&instance.data_dir)?;
    }
    if logs {
        remove_dir_all_if_exists(&instance.state_dir)?;
    }
    if conf {
        remove_dir_all_if_exists(&instance.flake_dir)?;
    }
    Ok(())
}

fn remove_dir_all_if_exists(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        eprintln!("remove: removing {}", path.display());
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[inline(never)]
fn run_ls(env: &Env) -> anyhow::Result<()> {
    match fs::read_dir(&env.data_root) {
        Ok(entries) => {
            for id in entries.filter_map(|entry| {
                let entry = entry.ok()?;
                entry.file_type().ok()?.is_dir().then(|| entry.file_name().into_string().ok())?
            }) {
                println!("{id}");
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("read instance data root"),
    }
}

#[inline(never)]
fn run_ps(env: &Env) -> anyhow::Result<()> {
    for id in list_instance_ids(env)? {
        println!("{id}\t{}", domstate(&id)?);
    }
    Ok(())
}

#[inline(never)]
fn run_ssh<S: AsRef<str>>(env: &Env, args: &[S], is_root: bool, inherit_tty: bool) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    run_wait(&instance, &["running"])?;
    let (address, ssh_port) = (read_port_forwards_lookup(&instance, Some(22), Some("tcp"))?.1)
        .ok_or_else(|| anyhow::anyhow!("ssh port forward for guest tcp/22 is not configured"))?;
    let user = if is_root { "root" } else { "vscode" };
    for attempt in 1..=120 {
        let status = process::Command::new("ssh")
            .arg(format!("{user}@{address}"))
            .arg("-p")
            .arg(ssh_port.to_string())
            .args((inherit_tty && std::io::stdin().is_terminal()).then_some("-t"))
            .args(["-o", "ConnectTimeout=1", "-o", "LogLevel=ERROR"])
            .args(["-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null"])
            .args(args.iter().map(AsRef::as_ref))
            .stdin(Stdio::inherit()) // allow ssh to read input and ioctl.
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("run ssh")?;
        if status.success() {
            return Ok(());
        }
        if status.code() != Some(255) || domstate(&instance.id)? != "running" || attempt == 120 {
            let status_text = status.code().map_or("signal".to_owned(), |code| code.to_string());
            bail!("ssh failed with status {status_text}");
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    bail!("ssh did not become ready for {}", env.hostname)
}

#[inline(never)]
fn run_logs<S: AsRef<str>>(env: &Env, args: &[S]) -> anyhow::Result<()> {
    let mut command: Vec<&str> = vec!["journalctl"];
    command.extend(args.iter().map(AsRef::as_ref));
    run_ssh(env, &command, true, true)
}

#[inline(never)]
fn run_stats(env: &Env) -> anyhow::Result<()> {
    for id in list_instance_ids(env)? {
        let output = process::Command::new("virsh")
            .args(["domstats", &id, "--raw", "--state", "--cpu-total", "--vcpu", "--balloon"])
            .output()
            .context("run virsh domstats")?;
        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr).trim());
            continue;
        }
        let stats: std::collections::HashMap<String, String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        println!("Domain:\t{id}");
        for (label, key) in [
            ("StateCode", "state.state"),
            ("StateReason", "state.reason"),
            ("CpuTimeNs", "cpu.time"),
            ("CpuUserNs", "cpu.user"),
            ("CpuSystemNs", "cpu.system"),
            ("VcpuCurrent", "vcpu.current"),
            ("VcpuMaximum", "vcpu.maximum"),
            ("MemCurrentKiB", "balloon.current"),
            ("MemRssKiB", "balloon.rss"),
            ("MemAvailableKiB", "balloon.available"),
            ("MemUsableKiB", "balloon.usable"),
        ] {
            println!("{label}:\t{}", stats.get(key).map(String::as_str).unwrap_or("N/A"));
        }
    }
    Ok(())
}

#[inline(never)]
fn run_wait<S: AsRef<str>>(instance: &Instance, states: &[S]) -> anyhow::Result<()> {
    loop {
        let state = domstate(&instance.id)?;
        if states.iter().any(|expected| expected.as_ref() == state.as_str()) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[inline(never)]
fn run_mount(env: &Env, path: Option<String>, name: Option<String>, is_mount: bool, read_only: bool) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    let config_toml_path = instance.flake_dir.join(CONFIG_TOML);
    let config_local_toml_path = instance.flake_dir.join(CONFIG_LOCAL_TOML);
    let config_toml_contents = fs::read_to_string(&config_toml_path).context(format!("read {CONFIG_TOML}"))?;
    let config_local_toml_contents = read_optional(&config_local_toml_path).context(format!("read {CONFIG_LOCAL_TOML}"))?;
    let config = parse_config(&config_toml_contents, Some(&config_local_toml_contents), &env.hostname, false).context("validate current TOML config")?;

    let base_abs = instance.workspace.canonicalize()?;
    let cwd_abs = env::current_dir()?.canonicalize()?;
    let to_base_rel = |path: &Path| -> anyhow::Result<(PathBuf, PathBuf)> {
        let path_abs = if path.is_absolute() { path.to_path_buf() } else { cwd_abs.join(path) };
        let path_rel = diff_paths(&path_abs, &base_abs).context("resolve relative path")?;
        let path_rel = if path_rel.as_os_str().is_empty() { PathBuf::from(".") } else { path_rel };
        Ok((path_rel, path_abs))
    };

    let (new_entry, kill_entry) = match (is_mount, path) {
        // mount.
        (true, Some(path)) => {
            let (source_rel, source_abs) = to_base_rel(Path::new(&path))?;
            validate_mount_source_field(&source_rel.display().to_string())?;
            if !source_abs.is_dir() && !source_abs.is_file() {
                bail!("mount source is neither file nor directory: {}", source_abs.display());
            }
            let name = match name {
                Some(name) => name,
                None => source_abs
                    .file_name()
                    .expect("failed to infer mount name from path")
                    .to_string_lossy()
                    .to_string(),
            };
            validate_mount_name_field(&name)?;
            (Some((source_rel, name)), None)
        }
        // unmount.
        (false, Some(source)) => {
            validate_mount_source_field(&source)?;
            let (source_rel, _) = to_base_rel(Path::new(&source))?;
            (None, Some(source_rel.display().to_string()))
        }
        // list mounts.
        (_, _) => {
            for (name, entry) in config.mounts {
                if let PolicyEntry::Set(mount) = entry {
                    println!("{}\t{name}\t{}", mount.source, if mount.readonly.unwrap_or(false) { "ro" } else { "rw" });
                }
            }
            return Ok(());
        }
    };

    if let Some((new_source, new_name)) = new_entry.as_ref() {
        let new_source = new_source.display().to_string();
        for (name, entry) in &config.mounts {
            if let PolicyEntry::Set(mount) = entry {
                if mount.source == new_source {
                    bail!("mount path already exists: {new_source}");
                }
                if name == new_name {
                    bail!("mount name already exists: {name}");
                }
            }
        }
    }

    let mut config_local_toml: DocumentMut = config_local_toml_contents.parse().context(format!("parse {CONFIG_LOCAL_TOML}"))?;
    if let Some((source, name)) = new_entry.as_ref() {
        let source = source.display().to_string();
        let mut mount = toml_edit::InlineTable::new();
        mount.insert("source", toml_edit::Value::from(source));
        mount.insert("readonly", toml_edit::Value::from(read_only));
        config_local_toml["hosts"][env.hostname.as_str()]["mounts"][name.as_str()] = toml_edit::value(mount);
    }
    if let Some(source) = kill_entry.as_ref() {
        let names = config
            .mounts
            .iter()
            .filter_map(|(name, entry)| match entry {
                PolicyEntry::Set(mount) if mount.source == *source => Some(name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if names.is_empty() {
            eprintln!("unmount: no changes to apply");
            return Ok(());
        }
        for name in names {
            if let Some(mounts) = config_local_toml
                .get_mut("hosts")
                .and_then(|hosts| hosts.get_mut(&env.hostname))
                .and_then(|host| host.get_mut("mounts"))
                .and_then(Item::as_table_like_mut)
            {
                mounts.remove(&name);
            }
            let without_override = parse_config(&config_toml_contents, Some(&config_local_toml.to_string()), &env.hostname, false)?;
            if matches!(without_override.mounts.get(&name), Some(PolicyEntry::Set(_))) {
                config_local_toml["hosts"][env.hostname.as_str()]["mounts"][name.as_str()] = toml_edit::value(false);
            }
        }
    }
    fs::write(&config_local_toml_path, config_local_toml.to_string()).context(format!("write {CONFIG_LOCAL_TOML}"))?;

    if instance.runtime_dir.join("control.sock").exists() {
        supervisor_command(&instance, "Reload")?;
        eprintln!("mounts: reloading");
    }
    Ok(())
}

fn parse_allowed_host_cli_argument(domain: &str) -> anyhow::Result<String> {
    // This CLI edits allowlist keys, not URLs.  Accepting URLs, paths, ports,
    // or root-dot spellings here would hide accidental input shape mistakes and
    // make the persisted TOML differ from the form users should review by hand.
    // Uppercase is the only tolerated non-storage spelling because DNS host
    // labels are case-insensitive and the stored policy key should be stable.
    if domain.is_empty() || domain != domain.trim() || domain.ends_with('.') || domain.contains(['\t', '\n', ' ', '/', '?', '#', ':']) || domain.contains("://")
    {
        bail!("invalid domain: use a host such as example.com or *.example.com");
    }

    let domain = domain.to_ascii_lowercase();
    if domain == "*" {
        return Ok(domain);
    }
    let host_without_wildcard = match domain.strip_prefix("*.") {
        Some(host_without_wildcard) => host_without_wildcard,
        None => {
            if domain.contains('*') {
                bail!("invalid domain: use a host such as example.com or *.example.com");
            }
            domain.as_str()
        }
    };

    // Keep the accepted key syntax deliberately narrow.  The downstream policy
    // file and TOML are host/glob allowlists; allowing empty labels or unusual
    // characters would create entries that look intentional but are unlikely to
    // match the user's domain intent.
    if host_without_wildcard.is_empty()
        || host_without_wildcard.contains('*')
        || host_without_wildcard.split('.').any(|label| {
            label.is_empty() || label.starts_with('-') || label.ends_with('-') || !label.chars().all(|char| char.is_ascii_alphanumeric() || char == '-')
        })
    {
        bail!("invalid domain: use a host such as example.com or *.example.com");
    }

    Ok(domain)
}

#[inline(never)]
fn run_allow_domain(env: &Env, domain: &str) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    let config_toml_path = instance.flake_dir.join(CONFIG_TOML);
    let config_local_toml_path = instance.flake_dir.join(CONFIG_LOCAL_TOML);
    let normalized_domain = parse_allowed_host_cli_argument(domain)?;
    let config_toml_contents = fs::read_to_string(&config_toml_path).context(format!("read {CONFIG_TOML}"))?;
    let config_local_toml_contents = read_optional(&config_local_toml_path).context(format!("read {CONFIG_LOCAL_TOML}"))?;
    parse_config(&config_toml_contents, Some(&config_local_toml_contents), &env.hostname, false).context("validate current TOML config")?;

    let mut config_local_toml: DocumentMut = config_local_toml_contents.parse().context(format!("parse {CONFIG_LOCAL_TOML}"))?;
    config_local_toml["hosts"][env.hostname.as_str()]["allowedHosts"][normalized_domain.as_str()] = toml_edit::value(toml_edit::InlineTable::new());
    fs::write(&config_local_toml_path, config_local_toml.to_string()).context(format!("write {CONFIG_LOCAL_TOML}"))?;
    Ok(())
}

#[inline(never)]
fn run_unallow_domain(env: &Env, domain: &str) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    let config_toml_path = instance.flake_dir.join(CONFIG_TOML);
    let config_local_toml_path = instance.flake_dir.join(CONFIG_LOCAL_TOML);
    let normalized_domain = parse_allowed_host_cli_argument(domain)?;
    let config_toml_contents = fs::read_to_string(&config_toml_path).context(format!("read {CONFIG_TOML}"))?;
    let config_local_toml_contents = read_optional(&config_local_toml_path).context(format!("read {CONFIG_LOCAL_TOML}"))?;
    let config = parse_config(&config_toml_contents, Some(&config_local_toml_contents), &env.hostname, false).context("validate current TOML config")?;
    if !matches!(config.allowed_hosts.get(&normalized_domain), Some(PolicyEntry::Set(_))) {
        eprintln!("unallow-domain: no changes to apply");
        return Ok(());
    }

    let mut config_local_toml: DocumentMut = config_local_toml_contents.parse().context(format!("parse {CONFIG_LOCAL_TOML}"))?;
    if let Some(allowed_hosts) = config_local_toml
        .get_mut("hosts")
        .and_then(|hosts| hosts.get_mut(&env.hostname))
        .and_then(|host| host.get_mut("allowedHosts"))
        .and_then(Item::as_table_like_mut)
    {
        allowed_hosts.remove(&normalized_domain);
    }
    let without_override = parse_config(&config_toml_contents, Some(&config_local_toml.to_string()), &env.hostname, false)?;
    if matches!(without_override.allowed_hosts.get(&normalized_domain), Some(PolicyEntry::Set(_))) {
        config_local_toml["hosts"][env.hostname.as_str()]["allowedHosts"][normalized_domain.as_str()] = toml_edit::value(false);
    }
    fs::write(&config_local_toml_path, config_local_toml.to_string()).context(format!("write {CONFIG_LOCAL_TOML}"))?;
    Ok(())
}

fn validate_mount_source_field(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.contains('\t') || value.contains('\n') {
        bail!("invalid mount source: contains control separator characters or empty");
    }
    Ok(())
}

#[inline(never)]
fn run_port(env: &Env, guest_port: Option<u16>, protocol: Option<&str>) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    match read_port_forwards_lookup(&instance, guest_port, protocol)? {
        (_, Some((address, host_port))) => println!("{address}:{host_port}"),
        (forwards, _) => {
            for (name, f) in forwards {
                for host_port in f.host..=f.host_end.unwrap_or(f.host) {
                    println!("{name}\t{}\t{}:{host_port}\t{}", f.proto, f.address, f.dev.clone().unwrap_or_default());
                }
            }
        }
    }
    Ok(())
}

#[inline(never)]
fn run_verify(env: &Env) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    // Tried to obtain signatures for untrusted paths, but not effective.
    // $ nix store copy-sigs -rvs https://cache.nixos.org /nix/var/nix/profiles/system
    let output = process::Command::new("nix-store")
        .args(["--verify", "--check-contents", "--repair", "--store"])
        .arg(format!("local?root={}", instance.system.display()))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .context("depends on host nix-store binary as store verifier")?;
    match output.status.code().context("guest nix store verify failed")? {
        0 => eprintln!("verify: nix store verify/repair succeeded (no remaining store corruptions)"),
        _ => eprintln!("verify: nix store verify/repair failed (unverifiable paths or remaining store corruptions)"),
    }
    if domstate(&instance.id)? != "running" {
        bail!("guest is not running, skipping nixos-rebuild --repair");
    }
    let flake = format!("{}#{}", supervisor_command(&instance, "BuildOn")?, env.hostname);
    let repair = run_ssh(env, &["nixos-rebuild", "build", "--repair", "--flake", &flake], true, true);
    let build_off = supervisor_command(&instance, "BuildOff").map(|_| ());
    repair.and(build_off)?;
    eprintln!("verify: nixos-rebuild build --repair succeeded (no remaining system profile corruptions)");
    Ok(())
}

fn run_audit(env: &Env, args: &[String]) -> anyhow::Result<()> {
    let instance = resolve_instance(env)?;
    let status = process::Command::new("vulnix")
        .args(["-g", &instance.system.display().to_string()])
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("depends on host patched vulnix binary for now. run in `nix develop` of devvm")?;
    process::exit(status.code().unwrap_or(1))
}
