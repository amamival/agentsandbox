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
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{Read as _, Write as _},
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
    about = "An unshared, efficient, reproducible NixOS Linux VM for self-improving agentic workflows",
    version
)]
struct Cli {
    /// Use the global sandbox scope instead of resolving the active workspace's local `.agentsandbox`.
    #[arg(short = 'g', long, global = true)]
    global: bool,
    /// Select sandbox hostname.
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
    /// Create `.agentsandbox/` and copy the initial template files
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
    #[command(alias = "destory")]
    Destroy {
        /// Remove sysroot
        #[arg(short = 's', long)]
        system: bool,
        /// Remove persistent data; with --system, remove the whole data dir
        #[arg(short = 'd', long)]
        data: bool,
        /// Remove the instance state dir
        #[arg(short = 'l', long)]
        logs: bool,
        /// Remove the resolved config dir
        #[arg(short = 'c', long)]
        conf: bool,
    },
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

fn main() {
    if let Err(err) = (|| -> anyhow::Result<()> {
        let cli = Cli::parse();
        let env = resolve_env(&cli).context("resolve environment")?;
        match cli.command {
            Some(Command::Version) => Ok(println!("{}", env!("CARGO_PKG_VERSION"))),
            Some(Command::Init { force }) => run_init(&env, force).context("init"),
            Some(Command::Build { bootstrap }) => run_build_or_up(&env, bootstrap, false, false).context("build"),
            Some(Command::Up { detach }) => run_build_or_up(&env, false, true, detach).context("up"),
            Some(Command::Down) => run_virsh_action(&env, "shutdown").context("down"),
            Some(Command::Kill) => run_virsh_action(&env, "destroy").context("kill"),
            Some(Command::Pause) => run_virsh_action(&env, "suspend").context("pause"),
            Some(Command::Unpause) => run_virsh_action(&env, "resume").context("unpause"),
            Some(Command::Ps) => run_ps(&env).context("ps"),
            Some(Command::Mount { path, name }) => run_mount(&env, path, name, true).context("mount"),
            Some(Command::Unmount { path }) => run_mount(&env, Some(path), None, false).context("unmount"),
            Some(Command::Destroy { system, data, logs, conf }) => run_destroy(&env, system, data, logs, conf).context("destroy"),
            Some(Command::Ssh { args }) => run_ssh(&env, &args, false).context("ssh"),
            Some(Command::Exec { args }) => run_ssh(&env, &args, true).context("exec"),
            Some(Command::Wait { states }) => run_wait(&resolve_instance(&env, &resolve_flake_dir(&env)?)?, &states).context("wait"),
            Some(Command::Verify) => run_verify(&env).context("verify"),
            None | Some(_) => Ok(println!("Comming soon(tm)...")),
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
        ("flake.nix", include_str!("../share/agentsandbox/template/flake.nix").to_owned()),
        ("configuration.nix", include_str!("../share/agentsandbox/template/configuration.nix").to_owned()),
        ("allowed_hosts", String::new()),
        ("mounts", format!("# <rel-host-path><TAB><guest-name>\n.\t{workspace_name}\n")),
        (
            "agentsandbox/flake.nix",
            include_str!("../share/agentsandbox/template/agentsandbox/flake.nix").to_owned(),
        ),
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
        bail!("{} not found", env.config_root.display())
    }
}

#[inline(never)]
fn resolve_instance(env: &Env, flake_dir: &Path) -> anyhow::Result<Instance> {
    let prefix_file = flake_dir.join("machine-prefix");
    let mut prefix = fs::read_to_string(&prefix_file).unwrap_or_default();
    if prefix.is_empty() {
        prefix = Sha256::digest(fs::canonicalize(flake_dir).context("canonicalize flake dir")?.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()[..24]
            .into();
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

// Prepare the minimal instance directories before sysroot build, virtiofsd, or log writers touch them.
fn prepare(instance: &Instance) -> anyhow::Result<()> {
    for dir in [&instance.sysroot, &instance.persistent, &instance.logs_dir, &instance.runtime_dir] {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

#[inline(never)]
fn run_build_or_up(env: &Env, bootstrap: bool, is_up: bool, detach: bool) -> anyhow::Result<()> {
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
    match domstate(&instance.id)?.as_str() {
        "down" | "shut off" | "crashed" => {
            let domain_profile = start_vm(env, &flake_dir, &instance, true)?;
            let flake = format!("/persistent/etc/nixos#{}", env.hostname);
            run_ssh(env, &["nixos-rebuild", "boot", "--flake", &flake], true)?;
            let new_profile = read_system_profile(&instance)?;
            println!("{}", new_profile.display());
            if !is_up {
                virsh(&["destroy", &instance.id]).context("return to down")?;
            } else if fs::canonicalize(domain_profile.join("domain.xml.sh")).context("resolve old domain.xml.sh")?
                != fs::canonicalize(new_profile.join("domain.xml.sh")).context("resolve new domain.xml.sh")?
            {
                virsh(&["destroy", &instance.id]).context("restart")?;
                start_vm(env, &flake_dir, &instance, false).context("restart")?;
            } else {
                run_ssh(env, &["systemctl", "isolate", "multi-user.target"], true).context("starting")?;
            }
        }
        "running" => {
            let domain_profile = read_domain_profile(&instance)?;
            let flake = format!("/persistent/etc/nixos#{}", env.hostname);
            let switch_or_boot = if is_switch { "switch" } else { "boot" };
            run_ssh(env, &["nixos-rebuild", switch_or_boot, "--flake", &flake], true)?;
            let new_profile = read_system_profile(&instance)?;
            if fs::canonicalize(domain_profile.join("domain.xml.sh")).context("resolve old domain.xml.sh")?
                != fs::canonicalize(new_profile.join("domain.xml.sh")).context("resolve new domain.xml.sh")?
            {
                eprintln!("build: domain definition changed; please restart the VM for the changes to take effect");
            }
            if !detach {
                run_ssh::<&str>(env, &[], false).context("detach")?;
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
    let profile_link = instance.sysroot.join("nix/var/nix/profiles/system");
    let system_profile = fs::read_link(profile_link).context("read system profile symlink")?;
    let rel_system_profile_path = system_profile.strip_prefix("/").context("require absolute system profile symlink")?;
    Ok(instance.sysroot.join(rel_system_profile_path))
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
                    .args([
                        "--extra-experimental-features",
                        "nix-command flakes",
                        "build",
                        &format!("/etc/nixos#nixosConfigurations.{hostname}.config.system.build.toplevel"),
                        "--out-link",
                        "/nix/var/nix/profiles/system",
                    ])
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
        if target == config_dir || target.starts_with(&workspace_dir)
        {
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
    Ok(())
}

fn validate_mount_source_field(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.contains('\t') || value.contains('\n') {
        bail!("invalid mount source: contains control separator characters or empty");
    }
    Ok(())
}

fn validate_mount_name_field(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value == "." || value == ".." || value.contains('\t') || value.contains('\n') {
        bail!("invalid mount name: contains control separator characters or empty");
    }
    Ok(())
}

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
        (_, _) => return Ok(println!("{}", fs::read_to_string(&mounts_path)?)),
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

#[inline(never)]
fn run_virsh_action(env: &Env, action: &str) -> anyhow::Result<()> {
    let instance = resolve_instance(env, &resolve_flake_dir(env)?)?;
    virsh(&[action, &instance.id])
    // TODO: wait for domain to be the desired state.
}

#[inline(never)]
fn run_ps(env: &Env) -> anyhow::Result<()> {
    let instance = resolve_instance(env, &resolve_flake_dir(env)?)?;
    println!("{}\t{}", instance.id, domstate(&instance.id)?);
    Ok(())
}

#[inline(never)]
fn run_verify(_env: &Env) -> anyhow::Result<()> {
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
#[inline(never)]
fn run_ssh<S: AsRef<str>>(env: &Env, args: &[S], is_root: bool) -> anyhow::Result<()> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
    run_wait(&instance, &["running"])?;
    let mut ssh_port = None;
    for line in fs::read_to_string(read_domain_profile(&instance)?.join("port-forwards"))
        .context("read domain profile port-forwards")?
        .lines()
    {
        let mut parts = line.split('\t');
        let (_name, proto, start, end, guest) = match (parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(name), Some(proto), Some(start), Some(end), Some(guest), None) => (name, proto, start, end, guest),
            _ => bail!("invalid port-forwards entry: {line}"),
        };
        if proto != "tcp" {
            continue;
        }
        let start = start.parse::<u64>().context("parse port-forwards host start")?;
        let end = end.parse::<u64>().context("parse port-forwards host end")?;
        let guest = guest.parse::<u64>().context("parse port-forwards guest port")?;
        if end < start {
            bail!("invalid port-forwards range: {line}");
        }
        if (22_u64 >= guest) && (22_u64 <= guest + (end - start)) {
            ssh_port = Some(u16::try_from(start + 22_u64 - guest).context("ssh host port exceeds u16")?);
            break;
        }
    }
    let ssh_port = match ssh_port {
        Some(port) => port,
        None => bail!("ssh port forward for guest tcp/22 is not configured"),
    };
    for attempt in 1..=120 {
        let status = process::Command::new("ssh")
            .arg(if is_root { "root@127.0.0.1" } else { "vscode@127.0.0.1" })
            .arg("-p")
            .arg(ssh_port.to_string())
            .args(["-o", "ConnectTimeout=1", "-o", "LogLevel=ERROR"])
            .args(["-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null"])
            .args(args.iter().map(AsRef::as_ref))
            .stdin(Stdio::inherit())
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

#[cfg(test)]
mod tests {
    use super::{APP_NAME, Cli, Instance, LOCAL_CONFIG_DIR, prepare, remove_dir_all_if_exists, resolve_env, resolve_flake_dir};
    use super::{resolve_instance, run_destroy, run_init};
    use sha2::{Digest, Sha256};
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::{Mutex, OnceLock},
    };

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn env_from_input(workspace: PathBuf, home: PathBuf, global: bool, xdg: Option<(PathBuf, PathBuf, PathBuf, PathBuf)>) -> super::Env {
        let (xdg_config_home, xdg_data_home, xdg_state_home, xdg_runtime_dir) = match xdg {
            Some((config, data, state, runtime)) => (Some(config), Some(data), Some(state), Some(runtime)),
            None => (None, None, None, None),
        };
        resolve_env(&Cli {
            global,
            hostname: "default".into(),
            workspace,
            home: Some(home),
            xdg_config_home,
            xdg_data_home,
            xdg_state_home,
            xdg_runtime_dir,
            command: None,
        })
        .unwrap()
    }

    fn test_root(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let root = env::temp_dir().join(format!("{APP_NAME}-rs-{name}"));
        let home = root.join("home");
        let _ = fs::remove_dir_all(&root);
        (root, home.clone(), home.join(".config").join(APP_NAME))
    }

    fn assert_dirs(dirs: &[&std::path::Path]) {
        for dir in dirs {
            fs::create_dir_all(dir).unwrap();
        }
    }

    fn reset_instance_dirs(paths: &Instance) {
        remove_dir_all_if_exists(&paths.data_dir).unwrap();
        remove_dir_all_if_exists(&paths.state_dir).unwrap();
        remove_dir_all_if_exists(&paths.runtime_dir).unwrap();
        fs::create_dir_all(&paths.sysroot).unwrap();
        fs::create_dir_all(&paths.persistent).unwrap();
        fs::create_dir_all(&paths.logs_dir).unwrap();
    }

    #[test]
    fn init_writes_expected_files_and_honors_force() {
        let _guard = env_lock().lock().unwrap();
        let (root, home, global) = test_root("init");
        let workspace = root.join("workspace");
        assert_dirs(&[&workspace, &home]);
        let mounts = format!(
            "# <host-path><TAB><guest-name>\n{}\tworkspace\n",
            fs::canonicalize(&workspace).unwrap().display()
        );
        let (local_env, global_env) = (
            env_from_input(workspace.clone(), home.clone(), false, None),
            env_from_input(workspace.clone(), home.clone(), true, None),
        );
        run_init(&local_env, false).unwrap();
        let local = workspace.join(LOCAL_CONFIG_DIR);
        run_init(&global_env, false).unwrap();
        for dir in [&local, &global] {
            for file in ["flake.nix", "configuration.nix", "allowed_hosts", "agentsandbox/flake.nix"] {
                assert!(dir.join(file).is_file());
            }
            assert_eq!(fs::read_to_string(dir.join("mounts")).unwrap(), mounts);
        }
        assert_eq!(run_init(&local_env, false).unwrap_err(), format!("{} already exists", local.display()));
        fs::write(global.join("allowed_hosts"), "stale\n").unwrap();
        run_init(&global_env, true).unwrap();
        assert_eq!(fs::read_to_string(global.join("allowed_hosts")).unwrap(), "");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolve_helpers() {
        let _guard = env_lock().lock().unwrap();
        let (root, home, global) = test_root("resolve");
        let workspace_root = root.join("workspace");
        let workspace = workspace_root.join("subdir");
        let flake_dir = workspace_root.join(LOCAL_CONFIG_DIR);
        assert_dirs(&[&home, &workspace]);
        run_init(&env_from_input(workspace_root.clone(), home.clone(), false, None), false).unwrap();
        assert_dirs(&[&global]);
        fs::write(global.join("flake.nix"), "").unwrap();
        let env = env_from_input(workspace.clone(), home.clone(), false, None);
        assert_dirs(&[&env.data_root]);
        assert_eq!(resolve_flake_dir(&env).unwrap(), flake_dir);
        fs::write(flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let machine_id = Sha256::digest(b"default").iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let existing = format!("renamed-default-0123456789abcdef01234567{}", &machine_id[..8]);
        fs::create_dir_all(env.data_root.join(&existing)).unwrap();
        assert_eq!(resolve_instance(&env, &flake_dir).unwrap().id, existing);
        let other_flake_dir = root.join("other").join(LOCAL_CONFIG_DIR);
        assert_dirs(&[&other_flake_dir, &root.join("data"), &root.join("state"), &root.join("runtime")]);
        fs::write(other_flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let mut other_env = env_from_input(
            "/target".into(),
            "/home".into(),
            false,
            Some(("/config".into(), "/data".into(), "/state".into(), "/runtime".into())),
        );
        other_env.hostname = "demo".into();
        let paths = resolve_instance(&other_env, &other_flake_dir).unwrap();
        let demo_machine_id = Sha256::digest(b"demo").iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        assert_eq!(paths.id, "other-demo-0123456789abcdef01234567".to_owned() + &demo_machine_id[..8]);
        assert_eq!(paths.data_dir, PathBuf::from("/data").join(APP_NAME).join(&paths.id));
        assert_eq!(paths.state_dir, PathBuf::from("/state").join(APP_NAME).join(&paths.id));
        assert_eq!(paths.runtime_dir, PathBuf::from("/runtime").join(APP_NAME).join(&paths.id));
        assert_eq!(paths.sysroot, paths.data_dir.join("sysroot"));
        assert_eq!(paths.persistent, paths.data_dir.join("persistent"));
        assert_eq!(paths.logs_dir, paths.state_dir.join("logs"));
        prepare(&Instance {
            id: "demo".into(),
            is_global: false,
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            runtime_dir: root.join("runtime"),
            sysroot: root.join("sysroot"),
            persistent: root.join("persistent"),
            logs_dir: root.join("logs"),
        })
        .unwrap();
        fs::remove_file(flake_dir.join("flake.nix")).unwrap();
        assert_eq!(resolve_flake_dir(&env_from_input(workspace.clone(), home.clone(), true, None)).unwrap(), global);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn destroy_respects_flag_combinations() {
        let _guard = env_lock().lock().unwrap();
        let (root, home, _global) = test_root("destroy");
        let workspace = root.join("workspace");
        assert_dirs(&[&home, &workspace]);
        let original_path = env::var("PATH").unwrap_or_default();
        let fake_bin = root.join("bin");
        fs::create_dir_all(&fake_bin).unwrap();
        let fake_virsh = fake_bin.join("virsh");
        fs::write(&fake_virsh, "#!/bin/sh\nexit 1\n").unwrap();
        let mut permissions = fs::metadata(&fake_virsh).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_virsh, permissions).unwrap();
        unsafe { env::set_var("PATH", format!("{}:{}", fake_bin.display(), original_path)) };
        run_init(&env_from_input(workspace.clone(), home.clone(), false, None), false).unwrap();
        let env = env_from_input(workspace.clone(), home.clone(), false, None);
        let flake_dir = workspace.join(LOCAL_CONFIG_DIR);
        fs::write(flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let paths = resolve_instance(&env, &flake_dir).unwrap();

        for (system, data, expect_sysroot, expect_persistent, expect_data_dir) in [
            (false, false, true, true, true),
            (true, false, false, true, true),
            (false, true, true, false, true),
            (true, true, false, false, false),
        ] {
            reset_instance_dirs(&paths);

            run_destroy(&env, system, data, false, false).unwrap();

            assert_eq!(paths.sysroot.exists(), expect_sysroot);
            assert_eq!(paths.persistent.exists(), expect_persistent);
            assert_eq!(paths.data_dir.exists(), expect_data_dir);
        }

        reset_instance_dirs(&paths);
        run_destroy(&env, false, false, true, false).unwrap();
        assert!(!paths.state_dir.exists());
        assert!(flake_dir.exists());

        reset_instance_dirs(&paths);
        run_destroy(&env, false, false, false, true).unwrap();
        assert!(!flake_dir.exists());

        unsafe { env::set_var("PATH", original_path) };
        fs::remove_dir_all(root).unwrap();
    }
}
