use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use pathdiff::diff_paths;
use reqwest::{blocking::Client, header};
use rustix::{
    io::{Errno, FdFlags, fcntl_setfd, read, write},
    mount::{MountFlags, MountPropagationFlags, UnmountFlags},
    mount::{mount, mount_bind, mount_bind_recursive, mount_change, mount_remount, unmount},
    pipe::{PipeFlags, pipe_with},
    process::{Pid, Signal, WaitOptions, chdir, getuid, kill_process, pivot_root, waitpid},
    runtime::{Fork, How, KernelSigSet, Timespec, exit_group, kernel_fork, kernel_sigprocmask, kernel_sigtimedwait},
    thread::{UnshareFlags, unshare_unsafe},
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{IsTerminal as _, Read as _, Write as _},
    os::fd::AsRawFd,
    os::unix::net::{UnixListener, UnixStream},
    os::unix::{fs::symlink, process::CommandExt},
    path::{Path, PathBuf},
    process::{self, Stdio},
};

const APP_NAME: &str = "agentsandbox";
const LOCAL_CONFIG_DIR: &str = ".agentsandbox";

#[derive(Parser)]
#[command(
    name = APP_NAME,
    about = "A secure, efficient, reproducible NixOS Linux VM for self-improving agentic workflows",
    version
)]
struct Cli {
    /// Use only global config (`$XDG_CONFIG_HOME/agentsandbox`) and skip local upward search.
    #[arg(short = 'g', long, global = true)]
    global: bool,
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
    /// Create `.agentsandbox/` and write the initial template files
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
    },
    /// Rebuild and start a VM; if already running, build and switch
    Up {
        #[arg(short = 'd', long)]
        detach: bool,
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
        /// Remove sysroot
        #[arg(short = 's', long)]
        system: bool,
        /// Remove persistent data; with --system, remove the whole data dir
        #[arg(short = 'd', long)]
        data: bool,
        /// Remove the instance states such as logs
        #[arg(short = 'l', long)]
        logs: bool,
        /// Remove the resolved config dir
        #[arg(short = 'c', long)]
        conf: bool,
    },
    /// List VM statuses for all hostnames in the current config
    Ps,
    /// Run a command as a user in a running VM, or attach if omitted
    ///
    /// Resolves SSH host port from `port-forwards` using guest `tcp/22`.
    /// Fails when no matching mapping exists.
    Ssh {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Run a command as root in a running VM, or attach if omitted
    Exec {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Show logs from a running VM. Runs `journalctl` with `-en1000` by default
    Logs {
        #[arg(default_values = ["-en1000"])]
        #[arg(trailing_var_arg = true)]
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
    Mount { path: Option<String>, name: Option<String> },
    /// Unmount a file or directory from a running VM now and on future starts
    Unmount { path: String },
    /// Prints the public port for a port binding
    Port {
        /// Guest port to resolve to a host port
        guest_port: Option<u16>,
        /// Protocol
        #[arg(long, value_parser = ["tcp", "udp"])]
        protocol: Option<String>,
    },
    /// Add a firewall rule that allows outbound traffic to a domain
    AllowDomain { domain: String },
    /// Remove the rule for the domain
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
        override_usage = "agentsandbox audit [OPTIONS] -- [VULNIX_OPTIONS] [PATHS...]

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
        #[arg(trailing_var_arg = true, hide = true, default_values = ["-G"])]
        args: Vec<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct Env {
    is_global: bool,
    hostname: String,
    workspace: PathBuf,
    config_root: PathBuf,
    data_root: PathBuf,
    state_root: PathBuf,
    runtime_root: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct Instance {
    id: String,
    is_global: bool,
    data_dir: PathBuf,
    state_dir: PathBuf,
    runtime_dir: PathBuf,
    sysroot: PathBuf,
    persistent: PathBuf,
    logs_dir: PathBuf,
}

#[derive(Deserialize)]
struct PortForward {
    proto: String,
    address: String,
    dev: Option<String>,
    host_start: u16,
    host_end: u16,
    guest: u16,
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
            Some(Command::Init { force }) => run_init(&env, force).context("init"),
            Some(Command::Build { bootstrap }) => run_build_or_up(&env, bootstrap, false, false).context("build"),
            Some(Command::Up { detach }) => run_build_or_up(&env, false, true, !detach).context("up"),
            Some(Command::Down) => run_virsh_action(&env, "shutdown").context("down"),
            Some(Command::Kill) => run_virsh_action(&env, "destroy").context("kill"),
            Some(Command::Pause) => run_virsh_action_all(&env, "suspend").context("pause"),
            Some(Command::Unpause) => run_virsh_action_all(&env, "resume").context("unpause"),
            Some(Command::Ps) => run_ps(&env).context("ps"),
            Some(Command::Doctor) => run_doctor(&env).context("doctor"),
            Some(Command::Mount { path, name }) => run_mount(&env, path, name, true).context("mount"),
            Some(Command::Unmount { path }) => run_mount(&env, Some(path), None, false).context("unmount"),
            Some(Command::Destroy { system, data, logs, conf }) => run_destroy(&env, system, data, logs, conf).context("destroy"),
            Some(Command::Ssh { args }) => run_ssh(&env, &args, false, false).context("ssh"),
            Some(Command::Exec { args }) => run_ssh(&env, &args, true, false).context("exec"),
            Some(Command::Port { guest_port, protocol }) => run_port(&env, guest_port, protocol.as_deref()).context("port"),
            Some(Command::Logs { args }) => run_logs(&env, &args).context("logs"),
            Some(Command::Stats) => run_stats(&env).context("stats"),
            Some(Command::Wait { states }) => run_wait(&resolve_instance(&env, &resolve_flake_dir(&env)?)?, &states).context("wait"),
            Some(Command::Verify) => run_verify(&env).context("verify"),
            Some(Command::Audit { args }) => run_audit(&env, &args).context("audit"),
            None | Some(_) => {
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
        workspace: cli.workspace.clone(),
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

// Create the config dir and initial files for local/global init, or return a displayable error.
#[inline(never)]
fn run_init(env: &Env, force: bool) -> anyhow::Result<()> {
    let target = if env.is_global {
        env.config_root.clone()
    } else {
        env.workspace.join(LOCAL_CONFIG_DIR)
    };
    write_template_config(&target, &env.workspace, force)?;
    eprintln!("init: wrote template files to {}", target.display());
    Ok(())
}

fn write_template_config(target: &Path, workspace: &Path, force: bool) -> anyhow::Result<()> {
    if target.exists() && !force {
        bail!("{} already exists", target.display());
    }
    let workspace_name = workspace.file_name().and_then(|name| name.to_str()).context("derive workspace name")?;
    fs::create_dir_all(target.join("agentsandbox")).context("create agentsandbox dir")?;
    for (name, contents) in [
        ("flake.nix", include_str!("../template/flake.nix").to_owned()),
        ("configuration.nix", include_str!("../template/configuration.nix").to_owned()),
        ("allowed_hosts", include_str!("../template/allowed_hosts").to_owned()),
        ("mounts", format!("# <rel-host-path><TAB><guest-name>\n.\t{workspace_name}\n")),
        ("agentsandbox/flake.nix", include_str!("../template/agentsandbox/flake.nix").to_owned()),
    ] {
        fs::write(target.join(name), contents).context("write template file")?;
    }
    Ok(())
}

#[inline(never)]
fn resolve_flake_dir(env: &Env) -> anyhow::Result<PathBuf> {
    if !env.is_global {
        let mut dir = env.workspace.clone();
        loop {
            if dir.join(format!("{LOCAL_CONFIG_DIR}/flake.nix")).is_file() {
                return Ok(dir.join(LOCAL_CONFIG_DIR));
            }
            if !dir.pop() {
                break;
            }
        }
    }
    if env.config_root.join("flake.nix").is_file() {
        Ok(env.config_root.clone())
    } else {
        bail!("{} not found. Try `agentsandbox init` to start in a new project.", env.config_root.display())
    }
}

#[inline(never)]
fn resolve_instance(env: &Env, flake_dir: &Path) -> anyhow::Result<Instance> {
    let prefix_file = flake_dir.join("machine-prefix");
    let mut prefix = match fs::read_to_string(&prefix_file) {
        Ok(prefix) => prefix.trim().to_owned(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).context("read machine-prefix"),
    };
    if prefix.is_empty() {
        prefix = Sha256::digest(fs::canonicalize(flake_dir).context("canonicalize flake dir")?.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()[..24]
            .to_owned();
        fs::write(&prefix_file, &prefix).context("write machine-prefix")?;
    }
    let machine_id = format!(
        "{prefix}{}",
        Sha256::digest(env.hostname.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let machine_id = &machine_id[..32];
    let name = if flake_dir.ends_with(LOCAL_CONFIG_DIR) {
        flake_dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .context("derive workspace name")?
    } else {
        APP_NAME
    };
    let id = fs::read_dir(&env.data_root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .find(|entry| entry.ends_with(machine_id))
        .unwrap_or_else(|| format!("{name}-{}-{machine_id}", env.hostname));
    let data_dir = env.data_root.join(&id);
    let state_dir = env.state_root.join(&id);
    Ok(Instance {
        runtime_dir: env.runtime_root.join(&id),
        id,
        is_global: flake_dir == env.config_root,
        sysroot: data_dir.join("sysroot"),
        persistent: data_dir.join("persistent"),
        logs_dir: state_dir.join("logs"),
        data_dir,
        state_dir,
    })
}

fn list_instance_ids(env: &Env, flake_dir: &Path) -> anyhow::Result<Vec<String>> {
    let prefix = fs::read_to_string(flake_dir.join("machine-prefix")).context("read machine-prefix")?;
    let prefix = prefix.trim();
    Ok(fs::read_dir(&env.data_root)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|id| id.get(id.len().saturating_sub(32)..id.len().saturating_sub(8)) == Some(prefix))
        .collect())
}

// Prepare the minimal instance directories before sysroot build, virtiofsd, or log writers touch them.
fn prepare(instance: &Instance) -> anyhow::Result<()> {
    for dir in [&instance.sysroot, &instance.persistent, &instance.logs_dir, &instance.runtime_dir] {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

#[inline(never)]
fn run_build_or_up(env: &Env, bootstrap: bool, is_up: bool, attach: bool) -> anyhow::Result<()> {
    let is_switch = is_up;
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
    prepare(&instance).context("prepare instance directories")?;
    if !instance.sysroot.join("nix/var/nix/profiles/default").is_symlink() {
        fetch_nix_dockerhub(&instance.sysroot).context("fetch")?;
    }
    if bootstrap || !instance.sysroot.join("nix/var/nix/profiles/system").is_symlink() {
        install_initial_nixos_profile(&env.workspace, &instance.sysroot, &env.hostname)?;
    }
    if !flake_dir.join("flake.lock").exists() {
        let status = process::Command::new("nix")
            .args(["flake", "lock", "--extra-experimental-features", "nix-command flakes"])
            .arg(format!("path:{}", flake_dir.display()))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("run host nix flake lock")?;
        if !status.success() {
            bail!(
                "nix flake lock failed with status {}",
                status.code().map_or("signal".to_owned(), |code| code.to_string())
            );
        }
    }
    let domstate = domstate(&instance.id)?;
    match domstate.as_str() {
        "down" | "shut off" | "crashed" => {
            let domain_profile = start_vm(env, &flake_dir, &instance, true)?;
            let flake = format!("/persistent/etc/nixos#{}", env.hostname);
            run_ssh(env, &["nixos-rebuild", "boot", "--flake", &flake], true, true)?;
            let new_profile = read_system_profile(&instance)?;
            println!("{}", new_profile.display());
            if !is_up {
                virsh(&["destroy", &instance.id]).context("return to down")?;
            } else if fs::read(domain_profile.join("domain.xml.sh"))? != fs::read(new_profile.join("domain.xml.sh"))? {
                virsh(&["destroy", &instance.id]).context("restart")?;
                start_vm(env, &flake_dir, &instance, false).context("restart")?;
            } else {
                run_ssh(env, &["systemctl", "isolate", "multi-user.target"], true, true).context("starting")?;
            }
        }
        "running" => {
            let domain_profile = read_domain_profile(&instance)?;
            let flake = format!("/persistent/etc/nixos#{}", env.hostname);
            let switch_or_boot = if is_switch { "switch" } else { "boot" };
            run_ssh(env, &["nixos-rebuild", switch_or_boot, "--flake", &flake], true, true)?;
            let new_profile = read_system_profile(&instance)?;
            if fs::read(domain_profile.join("domain.xml.sh"))? != fs::read(new_profile.join("domain.xml.sh"))? {
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

/// Read the NixOS profile which the domain is created with.
fn read_domain_profile(instance: &Instance) -> anyhow::Result<PathBuf> {
    fs::read_link(instance.runtime_dir.join("domain-profile")).context("read domain-profile symlink")
}

/// Read the NixOS profile to be used to start the VM next time.
fn read_system_profile(instance: &Instance) -> anyhow::Result<PathBuf> {
    let mut path = instance.sysroot.join("nix/var/nix/profiles/system");
    for _ in 0..16 {
        let target = fs::read_link(&path).context("read system profile symlink")?;
        path = if target.is_absolute() {
            instance
                .sysroot
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

#[inline(never)]
fn fetch_nix_dockerhub(sysroot: &Path) -> anyhow::Result<()> {
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
    eprintln!("fetch: extracting {} layers into {}", layers.len(), sysroot.display());
    for (index, blob) in layers.iter().filter_map(|layer| layer["digest"].as_str()).enumerate() {
        eprintln!("fetch: layer {}/{} {}", index + 1, layers.len(), blob);
        let response = client.get(format!("{registry}/{repo}/blobs/{blob}")).header(header::AUTHORIZATION, &auth);
        let response = response.send()?.error_for_status()?;
        tar::Archive::new(GzDecoder::new(response)).unpack(sysroot)?;
    }
    eprintln!("fetch: completed docker image extraction");
    Ok(())
}

// The initial profile is assumed safe, so building it in a simple container is acceptable.
#[inline(never)]
fn install_initial_nixos_profile(workspace: &Path, sysroot: &Path, hostname: &str) -> anyhow::Result<()> {
    let config_target = sysroot.join("etc/nixos");
    eprintln!("install: writing template config into {}", config_target.display());
    write_template_config(&config_target, workspace, true)?;

    // Remove previous out-link so `nix build --out-link /nix/var/nix/profiles/system` can overwrite it.
    let _ = fs::remove_file(sysroot.join("nix/var/nix/profiles/system"));
    spawn_mapped_namespace(true, true, || {
        (|| -> anyhow::Result<()> {
            // Prepare new file system hierarchy.
            let oldroot = Path::new("/");
            eprintln!("install: creating mountpoint dir sysroot/dev");
            fs::create_dir_all(sysroot.join("dev"))?;
            eprintln!("install: mounting tmpfs to sysroot/dev");
            mount("tmpfs", sysroot.join("dev"), "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, c"mode=0755")?;
            for dir in ["dev/shm", "dev/pts", "proc", "tmp"] {
                eprintln!("install: creating mountpoint dir sysroot/{dir}");
                fs::create_dir_all(sysroot.join(dir))?;
            }
            // Python's _multiprocessing.SemLock expects a writable 1777 /dev/shm.
            eprintln!("install: mounting tmpfs to sysroot/dev/shm");
            let shm_opts = c"mode=01777";
            mount("tmpfs", sysroot.join("dev/shm"), "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, shm_opts)?;
            eprintln!("install: mounting devpts to sysroot/dev/pts");
            let opts = c"newinstance,ptmxmode=0666,mode=620";
            mount("devpts", sysroot.join("dev/pts"), "devpts", MountFlags::NOSUID | MountFlags::NOEXEC, opts)?;
            eprintln!("install: binding host /proc to sysroot/proc");
            mount_bind_recursive(oldroot.join("proc"), sysroot.join("proc"))?;
            // Bind host devices etc to new root's /dev.
            for file in ["dev/null", "dev/zero", "dev/full", "dev/random", "dev/urandom", "dev/tty", "etc/resolv.conf"] {
                eprintln!("install: touching sysroot/{file} and binding host /{file}");
                fs::write(sysroot.join(file), "")?;
                mount_bind_recursive(oldroot.join(file), sysroot.join(file))?;
            }
            eprintln!("install: remounting sysroot/etc/resolv.conf read-only");
            mount_remount(sysroot.join("etc/resolv.conf"), MountFlags::BIND | MountFlags::RDONLY, c"")?;
            eprintln!("install: creating symlinks for /dev/{{stdin,stdout,stderr,fd,core,ptmx}}");
            for (fd, file) in [(0, "dev/stdin"), (1, "dev/stdout"), (2, "dev/stderr")] {
                symlink(format!("/proc/self/fd/{fd}"), sysroot.join(file))?;
            }
            symlink("/proc/self/fd", sysroot.join("dev/fd"))?;
            symlink("/proc/kcore", sysroot.join("dev/core"))?;
            symlink("pts/ptmx", sysroot.join("dev/ptmx"))?;

            eprintln!("install: pivoting root: / => (sysroot)/tmp, sysroot => /");
            // pivot_root() new_root must be a mountpoint. Bind sysroot to itself.
            mount_bind_recursive(sysroot, sysroot)?;
            // Pivot root: / => (sysroot)/tmp, sysroot => /.
            pivot_root(sysroot, sysroot.join("tmp"))?;
            eprintln!("install: detaching host / (currently pivoted to /tmp)");
            unmount("/tmp", UnmountFlags::DETACH)?; // Unmount old root.
            eprintln!("install: cd to the brand new root / (sysroot)");
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
                    eprintln!("{err}");
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

fn start_vm(env: &Env, flake_dir: &Path, instance: &Instance, is_build: bool) -> anyhow::Result<PathBuf> {
    let system_profile = read_system_profile(instance)?;
    let pv_socket = instance.runtime_dir.join("pv.sock");
    let pid_path = instance.runtime_dir.join("agentsandbox.pid");
    let domain_profile = instance.runtime_dir.join("domain-profile");
    let _ = fs::remove_file(&pv_socket);
    let _ = fs::remove_file(&pid_path);
    let _ = fs::remove_file(&domain_profile);
    let (mut parent_sock, mut child_sock) = UnixStream::pair().context("create supervisor socket pair")?;
    let supervisor_pid = spawn_mapped_namespace(false, false, || -> anyhow::Result<()> {
        let result = (|| -> anyhow::Result<()> {
            let downstream_rec = MountPropagationFlags::DOWNSTREAM | MountPropagationFlags::REC;
            mount_change("/", downstream_rec).context("set / mount propagation downstream+rec")?;
            mount_bind_recursive(&instance.persistent, &instance.persistent).context("self bind persistent dir")?;
            let shared_rec = MountPropagationFlags::SHARED | MountPropagationFlags::REC;
            mount_change(&instance.persistent, shared_rec).context("set persistent mount shared+rec")?;
            apply_mounts(env, flake_dir, instance, &system_profile).context("apply configured mounts")?;
            fs::write(&pid_path, format!("{}\n", process::id())).context("write pid file")?;

            let mut mask = KernelSigSet::empty();
            mask.insert(Signal::HUP);
            unsafe { kernel_sigprocmask(How::BLOCK, Some(&mask)) }.context("block HUP signal")?;

            let listener = UnixListener::bind(&pv_socket).context("bind virtiofs socket")?;
            fcntl_setfd(&listener, FdFlags::empty()).context("keep virtiofs socket fd across exec")?;
            let mut daemon = process::Command::new("virtiofsd")
                .args(["--shared-dir", &instance.persistent.display().to_string()])
                .args(["--fd", &listener.as_raw_fd().to_string()])
                .args(["--sandbox", "namespace", "--cache", "auto", "--xattr", "--log-level", "error"])
                // Use the supervisor user namespace as the single idmap source.
                .uid(0)
                .gid(0)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .context("spawn virtiofsd")?;
            drop(listener);

            if let Some(status) = daemon.try_wait().context("poll virtiofsd process")? {
                bail!("virtiofsd exited before socket became ready: {status}");
            }
            child_sock.write_all(&[1]).context("notify launcher that virtiofs socket is ready")?;

            loop {
                if let Some(status) = daemon.try_wait().context("poll virtiofsd process")? {
                    if status.success() {
                        break;
                    }
                    bail!("virtiofsd exited unexpectedly: {status}");
                }
                let timeout = Timespec {
                    tv_sec: 0,
                    tv_nsec: 200_000_000,
                };
                match unsafe { kernel_sigtimedwait(&mask, Some(&timeout)) } {
                    Ok(_info) => {
                        let reload_result = apply_mounts(env, flake_dir, instance, &system_profile).context("reload mounts");
                        if let Err(err) = reload_result {
                            eprintln!("apply_mounts: {err}");
                        }
                        continue;
                    }
                    Err(Errno::AGAIN) | Err(Errno::INTR) => continue,
                    Err(err) => bail!("wait HUP signal: {err}"),
                }
            }

            Ok(())
        })();
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(&pv_socket);
        result
    })
    .context("up")?;
    drop(child_sock);
    let mut ready = [0_u8; 1];
    parent_sock.read_exact(&mut ready).context("wait for virtiofs socket readiness notification")?;
    let machine_id = &instance.id[instance.id.len() - 32..];
    let domain_uuid = format!(
        "{}-{}-{}-{}-{}",
        &machine_id[..8],
        &machine_id[8..12],
        &machine_id[12..16],
        &machine_id[16..20],
        &machine_id[20..32]
    );
    let xml_path = instance.runtime_dir.join("domain.xml");
    let output = process::Command::new(system_profile.join("domain.xml.sh"))
        .env("NIX_DIR", instance.sysroot.join("nix"))
        .env("UID_MAP", capture_host_idmap("/proc/self/uid_map", true).context("resolve UID_MAP")?)
        .env("GID_MAP", capture_host_idmap("/proc/self/gid_map", true).context("resolve GID_MAP")?)
        .env("INSTANCE_ID", &instance.id)
        .env("DOMAIN_UUID", &domain_uuid)
        .env("MACHINE_ID", machine_id)
        .env("AGENTSANDBOX_BUILD", if is_build { "1" } else { "" })
        .env(
            "PERSISTENT_SOCKET_XML",
            pv_socket
                .display()
                .to_string()
                .replace('&', "&amp;")
                .replace('\'', "&apos;")
                .replace('<', "&lt;")
                .replace('>', "&gt;"),
        )
        .output()
        .context("run domain.xml.sh")?;
    if !output.status.success() {
        bail!("domain.xml.sh failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    fs::write(&xml_path, output.stdout).context("write generated domain xml")?;
    let status = process::Command::new("virsh")
        .arg("create")
        .arg(&xml_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("run virsh create")?;
    if !status.success() {
        let _ = kill_process(supervisor_pid, Signal::TERM);
        let _ = waitpid(Some(supervisor_pid), WaitOptions::empty());
        bail!(
            "virsh create failed with status {}",
            status.code().map_or("signal".to_owned(), |code| code.to_string())
        );
    }
    symlink(&system_profile, domain_profile).context("write runtime domain-profile symlink")?;
    Ok(system_profile)
}

/// Get compatible uid/gid maps from host.
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

/// Apply mounts from mounts file to the guest system.
fn apply_mounts(env: &Env, flake_dir: &Path, instance: &Instance, system_profile: &Path) -> anyhow::Result<()> {
    let mounts_path = flake_dir.join("mounts");
    let workspace_dir = instance.persistent.join("workspace");
    let config_dir = instance.persistent.join("etc/nixos");
    let mut mounted = Vec::new();

    // Collect all mounted directories under /persistent/workspace.
    let output = (process::Command::new("findmnt").args(["-Rlno", "target"]).output()).context("run findmnt -Rlno target")?;
    if !output.status.success() {
        bail!("findmnt: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    for target in String::from_utf8_lossy(&output.stdout).lines() {
        let target = Path::new(target);
        if target == config_dir || target.starts_with(&workspace_dir) {
            mounted.push(target.to_path_buf());
        }
    }
    mounted.sort();

    // Collect and validate all mounts from mounts file.
    let mut parsed_mounts = Vec::new();
    for line in fs::read_to_string(&mounts_path).context("read mounts file")?.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (source, name) = match (parts.next(), parts.next(), parts.next()) {
            (Some(source), Some(name), None) => (source, name),
            _ => bail!("invalid mounts entry: {line}"),
        };
        validate_mount_source_field(source)?;
        let source_abs = if Path::new(source).is_absolute() {
            PathBuf::from(source)
        } else {
            env.workspace.join(source)
        };
        let source_abs = source_abs.canonicalize().context("canonicalize mount source")?;
        if !source_abs.exists() {
            bail!("mount source does not exist: {}", source_abs.display());
        }
        if !source_abs.is_dir() && !source_abs.is_file() {
            bail!("mount source is neither file nor directory: {}", source_abs.display());
        }
        validate_mount_name_field(name)?;
        let target = workspace_dir.join(name);
        let is_dir = source_abs.is_dir();
        parsed_mounts.push((source_abs, target, is_dir, true));
    }
    parsed_mounts.push((
        flake_dir.canonicalize().context("canonicalize config dir")?,
        instance.persistent.join("etc/nixos"),
        true,
        system_profile.join("mutable-sandbox-config").exists(),
    ));
    parsed_mounts.sort_by(|a, b| a.1.cmp(&b.1));

    // Unmount all mounted directories in order of depth.
    for target in mounted.iter().rev() {
        let metadata = match fs::symlink_metadata(target) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => bail!("stat mounted target {}: {err}", target.display()),
        };
        if unmount(target, UnmountFlags::DETACH).is_err() {
            continue;
        }
        if metadata.is_dir() {
            match fs::remove_dir(target) {
                Ok(()) => {}
                // Underlaying mount is still visible.
                Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                Err(err) => bail!("remove mounted dir {}: {err}", target.display()),
            }
        } else if metadata.is_file() && metadata.len() == 0 {
            // Only remove empty files.
            fs::remove_file(target).context("remove mounted file")?;
        }
    }

    // Mount all mounts from mounts file.
    for (source_abs, target, is_dir, writable) in parsed_mounts {
        if is_dir {
            fs::create_dir_all(&target).context("create target dir")?;
            mount_bind_recursive(&source_abs, &target).context("bind-mount dir")?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).context("create target parent dir")?;
            }
            if !target.exists() {
                fs::write(&target, "").context("create target file")?;
            }
            mount_bind(&source_abs, &target).context("bind-mount file")?;
        }
        if !writable {
            mount_remount(&target, MountFlags::BIND | MountFlags::RDONLY, c"").context("remount file read-only")?;
        }
    }
    // To mitigate the risk of writing to the policy files.
    for name in ["mounts", "allowed_hosts"] {
        let target = config_dir.join(name);
        mount_bind(&target, &target).context("bind-mount policy file")?;
        mount_remount(&target, MountFlags::BIND | MountFlags::RDONLY, c"").context("remount policy file read-only")?;
    }
    Ok(())
}

fn validate_mount_source_field(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.contains('\t') || value.contains('\n') {
        bail!("invalid mount source: contains control separator characters or empty");
    }
    Ok(())
}

fn validate_mount_name_field(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || matches!(value, "." | "..") || value.contains(['\t', '\n']) || value.contains("../") {
        bail!("invalid mount name: contains control separator characters or empty");
    }
    Ok(())
}

#[inline(never)]
fn run_mount(env: &Env, path: Option<String>, name: Option<String>, is_mount: bool) -> anyhow::Result<()> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
    let mounts_path = flake_dir.join("mounts");

    let to_base_rel = |path: &Path| -> anyhow::Result<(PathBuf, PathBuf)> {
        let base_abs = env.workspace.canonicalize()?;
        let path_abs = if path.is_absolute() { path.to_path_buf() } else { base_abs.join(path) };
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
            println!("{}", fs::read_to_string(&mounts_path)?);
            return Ok(());
        }
    };

    let kill_matcher = format!("{}\t", kill_entry.as_deref().unwrap_or(""));
    let mut contents = String::new();
    let mut updated = false;
    for line in fs::read_to_string(&mounts_path)?.lines() {
        if line.starts_with(&kill_matcher) {
            updated = true;
        } else {
            contents.push_str(line);
            contents.push('\n');
            if let (Some((new_source, new_name)), Some((source, name))) = (new_entry.as_ref(), line.split_once('\t')) {
                if new_source.as_os_str() == source {
                    bail!("mount path already exists: {source}");
                }
                if new_name == name {
                    bail!("mount name already exists: {name}");
                }
            }
        }
    }
    if let Some(new_entry) = new_entry {
        updated = true;
        contents.push_str(&format!("{}\t{}\n", new_entry.0.display(), new_entry.1));
    }
    if !updated {
        eprintln!("unmount: no changes to apply");
        return Ok(());
    }
    fs::write(&mounts_path, contents)?;

    let pid_path = instance.runtime_dir.join("agentsandbox.pid");
    if let Ok(pid) = fs::read_to_string(&pid_path) {
        let pid = pid.trim().parse::<i32>()?;
        let pid = Pid::from_raw(pid).context("invalid agentsandbox.pid")?;
        if let Err(err) = kill_process(pid, Signal::HUP) {
            eprintln!("mounts: failed to reload: {err}");
            bail!("{err}");
        }
        eprintln!("mounts: reloading");
    }
    Ok(())
}

fn run_virsh_action(env: &Env, action: &str) -> anyhow::Result<()> {
    let instance = resolve_instance(env, &resolve_flake_dir(env)?)?;
    virsh(&[action, &instance.id])?;
    match action {
        "shutdown" => run_wait(&instance, &["down", "shut off", "crashed"]),
        _ => Ok(()),
    }
}

fn run_virsh_action_all(env: &Env, action: &str) -> anyhow::Result<()> {
    let flake_dir = resolve_flake_dir(env)?;
    for id in list_instance_ids(env, &flake_dir)? {
        virsh(&[action, &id])?;
    }
    Ok(())
}

#[inline(never)]
fn run_ps(env: &Env) -> anyhow::Result<()> {
    let flake_dir = resolve_flake_dir(env)?;
    for id in list_instance_ids(env, &flake_dir)? {
        println!("{id}\t{}", domstate(&id)?);
    }
    Ok(())
}

#[inline(never)]
fn run_logs<S: AsRef<str>>(env: &Env, args: &[S]) -> anyhow::Result<()> {
    let mut command: Vec<&str> = vec!["journalctl"];
    command.extend(args.iter().map(AsRef::as_ref));
    run_ssh(env, &command, true, false)
}

#[inline(never)]
fn run_stats(env: &Env) -> anyhow::Result<()> {
    let flake_dir = resolve_flake_dir(env)?;
    for id in list_instance_ids(env, &flake_dir)? {
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
    println!("CmdSshPath:\t\t\t{}", resolve_cmd_path("ssh"));
    println!("CmdVirtiofsdPath:\t\t{}", resolve_cmd_path("virtiofsd"));
    println!("CmdUnsharePath:\t\t\t{}", resolve_cmd_path("unshare"));
    println!("CmdNixStorePathForVerifyCmd:\t{}", resolve_cmd_path("nix-store"));
    println!("CmdVulnixPathForAuditCmd:\t{}", resolve_cmd_path("vulnix"));

    println!("ResolvedConfigRoot:\t\t{}", env.config_root.display());
    println!("ResolvedDataRoot:\t\t{}", env.data_root.display());
    println!("ResolvedStateRoot:\t\t{}", env.state_root.display());
    println!("ResolvedRuntimeRoot:\t\t{}", env.runtime_root.display());
    println!("WorkspaceArg:\t\t\t{}", env.workspace.display());
    println!("IsUserGlobalProject:\t\t{}", env.is_global);
    println!("InstanceHostnameArg:\t\t{}", env.hostname);

    let flake_dir = resolve_flake_dir(env);
    match &flake_dir {
        Err(err) => println!("ResolveFlakeDirError:\t\t{err:#}"),
        Ok(flake_dir) => {
            println!("ResolvedFlakeDir:\t\t{}", flake_dir.display());
            println!("FileFlakeNixExists:\t\t{}", flake_dir.join("flake.nix").is_file());
            println!("FileMountsExists:\t\t{}", flake_dir.join("mounts").is_file());
            println!("FileMachinePrefixExists:\t{}", flake_dir.join("machine-prefix").is_file());
            println!("FileAllowedHostsExists:\t\t{}", flake_dir.join("allowed_hosts").is_file());
            println!("FileFlakeLockExists:\t\t{}", flake_dir.join("flake.lock").is_file());
            match list_instance_ids(env, &flake_dir) {
                Err(err) => println!("ListInstanceIdsError:\t\t{err:#}"),
                Ok(ids) => println!("InstanceIds:\n\t{}", if ids.is_empty() { "none".into() } else { ids.join("\n\t") }),
            }
        }
    }

    let instance = flake_dir.ok().map(|flake_dir| resolve_instance(env, &flake_dir));
    match instance {
        None => println!("InstanceId:\t\t\tN/A"),
        Some(Err(err)) => println!("ResolveInstanceError:\t\t{err:#}"),
        Some(Ok(instance)) => {
            println!("InstanceId:\t\t\t{}", instance.id);
            println!("InstanceIsGlobal:\t\t{}", instance.is_global);
            println!("InstanceDataDir:\t\t{}", instance.data_dir.display());
            println!("InstanceSysrootDir:\t\t{}", instance.sysroot.display());
            println!("InstancePersistentDir:\t\t{}", instance.persistent.display());
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
                            .map(|(name, f)| format!("\t{name}\t{}\t{}:{}-{}\t{}", f.proto, f.address, f.host_start, f.host_end, f.guest))
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
fn run_verify(env: &Env) -> anyhow::Result<()> {
    let instance = resolve_instance(env, &resolve_flake_dir(env)?)?;
    // Tried to obtain signatures for untrusted paths, but not effective.
    // $ nix store copy-sigs -rvs https://cache.nixos.org /nix/var/nix/profiles/system
    let output = process::Command::new("nix-store")
        .args(["--verify", "--check-contents", "--repair", "--store"])
        .arg(format!("local?root={}", instance.sysroot.display()))
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
    let flake = format!("/persistent/etc/nixos#{}", env.hostname);
    run_ssh(env, &["nixos-rebuild", "build", "--repair", "--flake", &flake], true, true)?;
    eprintln!("verify: nixos-rebuild build --repair succeeded (no remaining system profile corruptions)");
    Ok(())
}

#[inline(never)]
fn run_destroy(env: &Env, system: bool, data: bool, logs: bool, conf: bool) -> anyhow::Result<()> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
    let _ = virsh(&["destroy", &instance.id]);
    if instance.is_global && !env.is_global {
        bail!("destroy files for the non-project instance requires --global");
    }
    if system {
        spawn_mapped_namespace(false, true, || remove_dir_all_if_exists(&instance.sysroot)).context("destroy sysroot")?;
    }
    if data {
        spawn_mapped_namespace(true, true, || remove_dir_all_if_exists(&instance.persistent)).context("destroy persistent")?;
    }
    if system && data {
        remove_dir_all_if_exists(&instance.data_dir)?;
    }
    if logs {
        remove_dir_all_if_exists(&instance.state_dir)?;
    }
    if conf {
        remove_dir_all_if_exists(&flake_dir)?;
    }
    Ok(())
}

fn read_port_forwards_lookup(
    instance: &Instance,
    guest_port: Option<u16>,
    protocol: Option<&str>,
) -> anyhow::Result<(BTreeMap<String, PortForward>, Option<(String, u16)>)> {
    let forwards: BTreeMap<String, PortForward> =
        serde_json::from_str(&fs::read_to_string(read_domain_profile(instance)?.join("port-forwards")).context("read port-forwards")?)
            .context("parse port-forwards json").unwrap_or(BTreeMap::from([("ssh".into(), PortForward { proto: "tcp".to_owned(), address: "127.0.0.1".to_owned(), dev: None, host_start: 2223, host_end: 2223, guest: 22 })]));
    match guest_port {
        Some(guest_port) => {
            for (_, f) in forwards {
                if protocol.is_some_and(|proto| f.proto != proto) {
                    continue;
                }
                let count = f.host_end - f.host_start + 1;
                if f.guest <= guest_port && guest_port <= f.guest + count - 1 {
                    return Ok((BTreeMap::new(), Some((f.address.clone(), f.host_start + (guest_port - f.guest)))));
                }
            }
            Ok((BTreeMap::new(), None))
        }
        None => Ok((forwards, None)),
    }
}

#[inline(never)]
fn run_port(env: &Env, guest_port: Option<u16>, protocol: Option<&str>) -> anyhow::Result<()> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
    match read_port_forwards_lookup(&instance, guest_port, protocol)? {
        (_, Some((address, host_port))) => println!("{address}:{host_port}"),
        (forwards, _) => {
            for (name, f) in forwards {
                for host_port in f.host_start..=f.host_end {
                    println!("{name}\t{}\t{}:{host_port}\t{}", f.proto, f.address, f.dev.clone().unwrap_or_default());
                }
            }
        }
    }
    Ok(())
}

#[inline(never)]
fn run_ssh<S: AsRef<str>>(env: &Env, args: &[S], is_root: bool, inherit_tty: bool) -> anyhow::Result<()> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
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
fn domstate(instance_id: &str) -> anyhow::Result<String> {
    let output = process::Command::new("virsh")
        .arg("domstate")
        .arg(instance_id)
        .output()
        .context("run virsh domstate")?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("failed to get domain") {
        Ok("down".to_owned())
    } else {
        bail!("{}", stderr.trim())
    }
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

fn remove_dir_all_if_exists(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        eprintln!("remove: removing {}", path.display());
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[inline(never)]
fn virsh(args: &[&str]) -> anyhow::Result<()> {
    let status = process::Command::new("virsh")
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

fn run_audit(env: &Env, args: &[String]) -> anyhow::Result<()> {
    let instance = resolve_instance(&env, &resolve_flake_dir(&env)?)?;
    let status = process::Command::new("vulnix")
        .args(["-g", &instance.sysroot.display().to_string()])
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("depends on host patched vulnix binary for now. run in `nix develop` of agentsandbox")?;
    process::exit(status.code().unwrap_or(1))
}
