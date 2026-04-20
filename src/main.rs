use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use reqwest::{blocking::Client, header};
use rustix::{
    io::{read, write},
    mount::{MountFlags, MountPropagationFlags, UnmountFlags},
    mount::{mount, mount_bind, mount_bind_recursive, mount_change, mount_remount, unmount},
    pipe::{PipeFlags, pipe_with},
    process::{WaitOptions, chdir, fchdir, getuid, pivot_root, waitpid},
    runtime::{Fork, exit_group, kernel_fork},
    thread::{UnshareFlags, unshare_unsafe},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::ErrorKind,
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
    #[arg(short = 'w', global = true, env = "PWD")]
    workspace: Option<PathBuf>,

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
    let cli = Cli::parse();
    let env = resolve_env(&cli).unwrap_or_else(|err| {
        eprintln!("{err}");
        process::exit(1);
    });
    if let Err(err) = match cli.command {
        Some(Command::Version) => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Command::Init { force }) => run_init(&env, force),
        Some(Command::Build) => run_build(&env),
        Some(Command::Up { detach: _ }) => run_build(&env),
        Some(Command::Down) => run_down(&env),
        Some(Command::Kill) => run_kill(&env),
        Some(Command::Pause) => run_pause(&env),
        Some(Command::Unpause) => run_unpause(&env),
        Some(Command::Ps) => run_ps(&env),
        Some(Command::Destroy { system, data, logs, conf }) => run_destroy(&env, system, data, logs, conf),
        Some(Command::Ssh { args }) => run_ssh(&env, &args),
        None | Some(_) => Ok(()),
    } {
        eprintln!("{err}");
        process::exit(1);
    }
}

fn resolve_env(cli: &Cli) -> Result<Env, String> {
    let uid = getuid().as_raw();
    let workspace = cli.workspace.clone().ok_or("PWD is not set; pass --target".to_owned())?;
    let home = cli.home.clone().unwrap_or_else(|| workspace.clone());
    Ok(Env {
        is_global: cli.global,
        hostname: cli.hostname.clone(),
        workspace,
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
fn run_init(env: &Env, force: bool) -> Result<(), String> {
    let target = if env.is_global {
        env.config_root.clone()
    } else {
        env.workspace.join(LOCAL_CONFIG_DIR)
    };
    let workspace = env.workspace.canonicalize().map_err(|err| err.to_string())?;
    write_template_config(&target, &workspace, force)?;
    eprintln!("init: wrote template files to {}", target.display());
    Ok(())
}

fn run_build(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, &env.hostname)?;
    prepare(&instance)?;
    if !instance.sysroot.join("nix/store").is_dir() {
        fetch_nix_dockerhub(&instance.sysroot)?;
    }
    install_initial_nixos_profile(env, &instance.sysroot, &flake_dir, &env.hostname)?;
    Ok(())
}

// Prepare the minimal instance directories before sysroot build, virtiofsd, or log writers touch them.
fn prepare(instance: &Instance) -> Result<(), String> {
    for dir in [&instance.sysroot, &instance.persistent, &instance.logs_dir] {
        fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn fetch_nix_dockerhub(sysroot: &Path) -> Result<(), String> {
    let repo = "nixos/nix";
    let registry = "https://registry-1.docker.io/v2";
    eprintln!("fetch: requesting docker auth token for {repo}");
    let client = Client::builder().build().map_err(|err| err.to_string())?;
    let token = client
        .get(format!("https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:pull"))
        .send()
        .map_err(|err| err.to_string())?;
    let token = token.error_for_status().map_err(|err| err.to_string())?;
    let token = token.json::<Value>().map_err(|err| err.to_string())?;
    let token = token["token"].as_str().ok_or("docker token missing".to_owned())?;
    let auth = format!("Bearer {token}");
    eprintln!("fetch: resolving image manifest list (latest)");
    let manifests = client.get(format!("{registry}/{repo}/manifests/latest")).header(header::AUTHORIZATION, &auth);
    let manifests = manifests.send().map_err(|err| err.to_string())?;
    let manifests = manifests.error_for_status().map_err(|err| err.to_string())?;
    let manifests = manifests.json::<Value>().map_err(|err| err.to_string())?;
    let digest = manifests["manifests"]
        .as_array()
        .ok_or("docker manifest list missing".to_owned())?
        .iter()
        .find(|manifest| manifest["platform"]["architecture"] == "amd64" && manifest["platform"]["os"] == "linux")
        .ok_or("linux/amd64 docker manifest missing".to_owned())?;
    let digest = digest["digest"].as_str().ok_or("linux/amd64 docker manifest missing".to_owned())?;
    eprintln!("fetch: selected linux/amd64 image digest {digest}");
    let manifest = client.get(format!("{registry}/{repo}/manifests/{digest}")).header(header::AUTHORIZATION, &auth);
    let manifest = manifest.send().map_err(|err| err.to_string())?;
    let manifest = manifest.error_for_status().map_err(|err| err.to_string())?;
    let manifest = manifest.json::<Value>().map_err(|err| err.to_string())?;
    let layers = manifest["layers"].as_array().ok_or("docker layers missing".to_owned())?;
    eprintln!("fetch: extracting {} layers into {}", layers.len(), sysroot.display());
    for (index, blob) in layers.iter().filter_map(|layer| layer["digest"].as_str()).enumerate() {
        eprintln!("fetch: layer {}/{} {}", index + 1, layers.len(), blob);
        let url = format!("{registry}/{repo}/blobs/{blob}");
        let response = client.head(&url).header(header::AUTHORIZATION, &auth);
        let response = response.send().map_err(|err| err.to_string())?;
        response.error_for_status().map_err(|err| err.to_string())?;
        let response = client.get(url).header(header::AUTHORIZATION, &auth);
        let response = response.send().map_err(|err| err.to_string())?;
        let response = response.error_for_status().map_err(|err| err.to_string())?;
        tar::Archive::new(GzDecoder::new(response)).unpack(sysroot).map_err(|err| err.to_string())?;
    }
    eprintln!("fetch: completed docker image extraction");
    Ok(())
}

fn install_initial_nixos_profile(env: &Env, sysroot: &Path, _flake_dir: &Path, hostname: &str) -> Result<(), String> {
    let config_target = sysroot.join("etc/nixos");
    eprintln!("install: writing template config into {}", config_target.display());
    write_template_config(&config_target, &env.workspace.canonicalize().map_err(|err| err.to_string())?, true)?;
    // Remove previous out-link so `nix build --out-link /nix/var/nix/profiles/system` can overwrite it.
    match fs::remove_file(sysroot.join("nix/var/nix/profiles/system")) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.to_string()),
    }
    eprintln!("install: capturing uid/gid maps via unshare");
    let uid_map = run_capture("unshare", &["--user", "--map-root-user", "--map-auto", "cat", "/proc/self/uid_map"])?;
    let gid_map = run_capture("unshare", &["--user", "--map-root-user", "--map-auto", "cat", "/proc/self/gid_map"])?;
    let (child_ready_read, child_ready_write) = pipe_with(PipeFlags::CLOEXEC).map_err(|err| err.to_string())?;
    let (parent_done_read, parent_done_write) = pipe_with(PipeFlags::CLOEXEC).map_err(|err| err.to_string())?;
    match unsafe { kernel_fork() }.map_err(|err| err.to_string())? {
        Fork::Child(_) => {
            drop(child_ready_read);
            drop(parent_done_write);
            let status = match install_initial_nixos_profile_child(sysroot, hostname, &child_ready_write, &parent_done_read) {
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
                wait_for_pipe_signal(&child_ready_read, "child namespace setup")?;
                install_user_namespace_maps(child_pid, &uid_map, &gid_map)?;
                write_all_fd(&parent_done_write, &[1])
            })()
            .err();
            drop(parent_done_write);
            let (_, status) = waitpid(Some(child_pid), WaitOptions::empty())
                .map_err(|err| err.to_string())?
                .ok_or("install child disappeared before waitpid reported status".to_owned())?;
            if let Some(err) = parent_error {
                return Err(err);
            }
            if let Some(code) = status.exit_status() {
                if code == 0 {
                    Ok(())
                } else {
                    Err(format!("install child exited with status {code}"))
                }
            } else if let Some(signal) = status.terminating_signal() {
                Err(format!("install child terminated by signal {signal}"))
            } else {
                Err(format!("install child ended unexpectedly: {status:?}"))
            }
        }
    }
}

fn install_initial_nixos_profile_child(
    sysroot: &Path,
    hostname: &str,
    child_ready_write: &std::os::fd::OwnedFd,
    parent_done_read: &std::os::fd::OwnedFd,
) -> Result<(), String> {
    eprintln!("install: entering user+mount namespace");
    unsafe { unshare_unsafe(UnshareFlags::NEWUSER | UnshareFlags::NEWNS) }.map_err(|err| err.to_string())?;
    write_all_fd(child_ready_write, &[1])?;
    wait_for_pipe_signal(parent_done_read, "parent uid/gid map install")?;
    eprintln!("install: changing mount propagation to slave | recursive");
    mount_change("/", MountPropagationFlags::DOWNSTREAM | MountPropagationFlags::REC).map_err(|err| err.to_string())?;
    eprintln!("install: mounting tmpfs staging root on /tmp");
    mount("tmpfs", "/tmp", "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, c"").map_err(|err| err.to_string())?;
    chdir("/tmp").map_err(|err| err.to_string())?;
    fs::create_dir_all("newroot").map_err(|err| err.to_string())?;
    mount_bind_recursive("newroot", "newroot").map_err(|err| err.to_string())?;
    fs::create_dir_all("oldroot").map_err(|err| err.to_string())?;
    eprintln!("install: pivoting into tmpfs staging root");
    pivot_root(".", "oldroot").map_err(|err| err.to_string())?;
    chdir("/").map_err(|err| err.to_string())?;

    eprintln!("install: binding sysroot to /newroot");
    bind_tree(&oldroot_path(sysroot)?, Path::new("/newroot"))?;
    eprintln!("install: binding host /etc/resolv.conf to /newroot/etc/resolv.conf");
    bind_tree(&oldroot_path(Path::new("/etc/resolv.conf"))?, Path::new("/newroot/etc/resolv.conf"))?;
    eprintln!("install: remounting /newroot/etc/resolv.conf read-only");
    mount_remount("/newroot/etc/resolv.conf", MountFlags::BIND | MountFlags::RDONLY, c"").map_err(|err| err.to_string())?;
    eprintln!("install: binding host /proc to /newroot/proc");
    bind_tree(&oldroot_path(Path::new("/proc"))?, Path::new("/newroot/proc"))?;

    fs::create_dir_all("/newroot/dev").map_err(|err| err.to_string())?;
    eprintln!("install: mounting tmpfs /newroot/dev");
    mount("tmpfs", "/newroot/dev", "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, c"mode=0755").map_err(|err| err.to_string())?;
    for node in ["null", "zero", "full", "random", "urandom", "tty"] {
        bind_tree(&oldroot_path(&Path::new("/dev").join(node))?, &Path::new("/newroot/dev").join(node))?;
    }
    for (fd, node) in [(0, "stdin"), (1, "stdout"), (2, "stderr")] {
        symlink(format!("/proc/self/fd/{fd}"), Path::new("/newroot/dev").join(node)).map_err(|err| err.to_string())?;
    }
    symlink("/proc/self/fd", "/newroot/dev/fd").map_err(|err| err.to_string())?;
    symlink("/proc/kcore", "/newroot/dev/core").map_err(|err| err.to_string())?;
    fs::create_dir_all("/newroot/dev/shm").map_err(|err| err.to_string())?;
    fs::create_dir_all("/newroot/dev/pts").map_err(|err| err.to_string())?;
    eprintln!("install: mounting devpts to /newroot/dev/pts");
    mount(
        "devpts",
        "/newroot/dev/pts",
        "devpts",
        MountFlags::NOSUID | MountFlags::NOEXEC,
        c"newinstance,ptmxmode=0666,mode=620",
    )
    .map_err(|err| err.to_string())?;
    symlink("pts/ptmx", "/newroot/dev/ptmx").map_err(|err| err.to_string())?;
    eprintln!("install: mounting tmpfs to /newroot/dev/shm");
    mount("tmpfs", "/newroot/dev/shm", "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, c"mode=01777").map_err(|err| err.to_string())?;

    eprintln!("install: detaching old root");
    mount_change("/oldroot", MountPropagationFlags::PRIVATE | MountPropagationFlags::REC).map_err(|err| err.to_string())?;
    unmount("/oldroot", UnmountFlags::DETACH).map_err(|err| err.to_string())?;
    let oldroot = fs::File::open("/").map_err(|err| err.to_string())?;
    chdir("/newroot").map_err(|err| err.to_string())?;
    pivot_root(".", ".").map_err(|err| err.to_string())?;
    fchdir(&oldroot).map_err(|err| err.to_string())?;
    unmount(".", UnmountFlags::DETACH).map_err(|err| err.to_string())?;
    chdir("/").map_err(|err| err.to_string())?;

    eprintln!("install: execing nix build for hostname={hostname}");
    Err(process::Command::new("/nix/var/nix/profiles/default/bin/nix")
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "build",
            &format!("/etc/nixos#nixosConfigurations.{hostname}.config.system.build.toplevel"),
            "--out-link",
            "/nix/var/nix/profiles/system",
        ])
        .exec()
        .to_string())
}

fn run_down(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, &env.hostname)?;
    virsh(&["shutdown", instance.id.as_str()]);
    Ok(())
}

fn run_kill(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, &env.hostname)?;
    virsh(&["destroy", instance.id.as_str()]);
    Ok(())
}

fn run_pause(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, &env.hostname)?;
    virsh(&["suspend", instance.id.as_str()]);
    Ok(())
}

fn run_unpause(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, &env.hostname)?;
    virsh(&["resume", instance.id.as_str()]);
    Ok(())
}

fn run_ps(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, &env.hostname)?;
    let state = match domstate(instance.id.as_str())? {
        Some(state) => state,
        None => "down".to_owned(),
    };
    println!("{}\t{}", instance.id, state);
    Ok(())
}

fn run_ssh(env: &Env, args: &[String]) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, &env.hostname)?;
    if domstate(&instance.id)?.as_deref() != Some("running") {
        return Err("VM is not running".to_owned());
    }
    let ssh_port = host_port_for_guest(&eval_port_forwards(&flake_dir, &env.hostname)?, 22, "tcp")
        .ok_or("ssh port forward for guest tcp/22 is not configured".to_owned())?;
    Err(process::Command::new("ssh")
        .arg("vscode@127.0.0.1")
        .arg("-p")
        .arg(ssh_port.to_string())
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .exec()
        .to_string())
}

fn run_destroy(env: &Env, system: bool, data: bool, logs: bool, conf: bool) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, &env.hostname)?;
    virsh(&["destroy", instance.id.as_str()]);
    if instance.is_global && !env.is_global {
        return Err("destroy files for the non-project instance requires --global".to_owned());
    }
    if system && data {
        remove_dir_all_if_exists(&instance.data_dir)?;
    } else if system {
        remove_dir_all_if_exists(&instance.sysroot)?;
    } else if data {
        remove_dir_all_if_exists(&instance.persistent)?;
    }
    if logs {
        remove_dir_all_if_exists(&instance.state_dir)?;
    }
    if conf {
        remove_dir_all_if_exists(&flake_dir)?;
    }
    Ok(())
}

fn virsh(args: &[&str]) {
    let _ = process::Command::new("virsh")
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
}

fn run_capture(command: &str, args: &[&str]) -> Result<String, String> {
    let output = process::Command::new(command).args(args).output().map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn run_inherit_strings(command: &str, args: &[String]) -> Result<(), String> {
    let status = process::Command::new(command)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{command} failed with status {}",
            status.code().map_or("signal".to_owned(), |code| code.to_string())
        ))
    }
}

fn install_user_namespace_maps(child_pid: rustix::process::Pid, uid_map: &str, gid_map: &str) -> Result<(), String> {
    eprintln!("install: writing uid map via newuidmap for child {child_pid}");
    run_inherit_strings("newuidmap", &id_map_command_args(child_pid, uid_map)?)?;
    eprintln!("install: writing setgroups deny for child {child_pid}");
    fs::write(format!("/proc/{}/setgroups", child_pid.as_raw_pid()), "deny\n").map_err(|err| err.to_string())?;
    eprintln!("install: writing gid map via newgidmap for child {child_pid}");
    run_inherit_strings("newgidmap", &id_map_command_args(child_pid, gid_map)?)
}

fn id_map_command_args(child_pid: rustix::process::Pid, id_map: &str) -> Result<Vec<String>, String> {
    let mut args = vec![child_pid.as_raw_pid().to_string()];
    for line in id_map.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(format!("invalid id map line: {line}"));
        }
        args.extend(fields.into_iter().map(str::to_owned));
    }
    if args.len() == 1 {
        return Err("captured id map was empty".to_owned());
    }
    Ok(args)
}

fn write_all_fd(fd: &std::os::fd::OwnedFd, mut buf: &[u8]) -> Result<(), String> {
    while !buf.is_empty() {
        let written = write(fd, buf).map_err(|err| err.to_string())?;
        if written == 0 {
            return Err("short write to pipe".to_owned());
        }
        buf = &buf[written..];
    }
    Ok(())
}

fn wait_for_pipe_signal(fd: &std::os::fd::OwnedFd, stage: &str) -> Result<(), String> {
    let mut byte = [0_u8; 1];
    if read(fd, &mut byte).map_err(|err| err.to_string())? == 1 {
        Ok(())
    } else {
        Err(format!("{stage} failed before signaling readiness"))
    }
}

fn oldroot_path(path: &Path) -> Result<PathBuf, String> {
    Ok(Path::new("/oldroot").join(path.strip_prefix("/").map_err(|_| format!("{} is not absolute", path.display()))?))
}

fn bind_tree(source: &Path, target: &Path) -> Result<(), String> {
    if ensure_mount_target(source, target)? {
        mount_bind_recursive(source, target).map_err(|err| err.to_string())
    } else {
        mount_bind(source, target).map_err(|err| err.to_string())
    }
}

fn ensure_mount_target(source: &Path, target: &Path) -> Result<bool, String> {
    let metadata = fs::metadata(source).map_err(|err| err.to_string())?;
    if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|err| err.to_string())?;
        Ok(true)
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        if !target.exists() {
            fs::write(target, "").map_err(|err| err.to_string())?;
        }
        Ok(false)
    }
}

fn domstate(instance_id: &str) -> Result<Option<String>, String> {
    let output = process::Command::new("virsh")
        .arg("domstate")
        .arg(instance_id)
        .output()
        .map_err(|err| err.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    } else if stderr.contains("failed to get domain") {
        Ok(None)
    } else {
        Err(stderr)
    }
}

fn eval_port_forwards(flake_dir: &Path, hostname: &str) -> Result<Value, String> {
    let output = process::Command::new("nix")
        .arg("eval")
        .arg("--json")
        .arg(format!(
            "{}#nixosConfigurations.{hostname}.config.agentsandbox.portForwards",
            flake_dir.display()
        ))
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())
}

fn host_port_for_guest(port_forwards: &Value, guest_port: u16, proto: &str) -> Option<u16> {
    for forward in port_forwards.as_object()?.values() {
        if forward.get("proto")?.as_str()? != proto {
            continue;
        }
        let start = if forward.get("host")?.is_number() {
            forward.get("host")?.as_u64()?
        } else {
            forward.get("host")?.get("start")?.as_u64()?
        };
        let end = if forward.get("host")?.is_number() {
            start
        } else {
            forward.get("host")?.get("end")?.as_u64()?
        };
        let guest = forward.get("guest")?.as_u64()?;
        let guest_port = u64::from(guest_port);
        if guest_port >= guest && guest_port <= guest + (end - start) {
            return u16::try_from(start + guest_port - guest).ok();
        }
    }
    None
}

fn resolve_instance(env: &Env, flake_dir: &Path, hostname: &str) -> Result<Instance, String> {
    let prefix_file = flake_dir.join("machine-prefix");
    let mut prefix = fs::read_to_string(&prefix_file).unwrap_or_default();
    if prefix.is_empty() {
        prefix = Sha256::digest(flake_dir.canonicalize().map_err(|err| err.to_string())?.display().to_string().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()[..24]
            .into();
        fs::write(&prefix_file, &prefix).map_err(|err| err.to_string())?;
    }
    let machine_id = format!(
        "{prefix}{}",
        Sha256::digest(hostname.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>()
    );
    let machine_id = &machine_id[..32];
    let name = if flake_dir.ends_with(LOCAL_CONFIG_DIR) {
        flake_dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or("failed to derive workspace name".to_owned())?
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
        .unwrap_or_else(|| format!("{name}-{hostname}-{machine_id}"));
    let data_dir = env.data_root.join(&id);
    let state_dir = env.state_root.join(&id);
    let runtime_dir = env.runtime_root.join(&id);
    Ok(Instance {
        id,
        is_global: flake_dir == env.config_root,
        sysroot: data_dir.join("sysroot"),
        persistent: data_dir.join("persistent"),
        logs_dir: state_dir.join("logs"),
        data_dir,
        state_dir,
        runtime_dir,
    })
}

fn remove_dir_all_if_exists(path: &Path) -> Result<(), String> {
    if path.exists() {
        eprintln!("remove: removing {}", path.display());
        fs::remove_dir_all(path).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn write_template_config(target: &Path, workspace: &Path, force: bool) -> Result<(), String> {
    if target.exists() && !force {
        return Err(format!("{} already exists", target.display()));
    }
    fs::create_dir_all(target).map_err(|err| err.to_string())?;
    let workspace_name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("failed to derive workspace name".to_owned())?;
    fs::create_dir_all(target.join("agentsandbox")).map_err(|err| err.to_string())?;
    for (name, contents) in [
        ("flake.nix", include_str!("../share/agentsandbox/template/flake.nix").to_owned()),
        ("configuration.nix", include_str!("../share/agentsandbox/template/configuration.nix").to_owned()),
        ("allowed_hosts", String::new()),
        ("mounts", format!("# <host-path><TAB><guest-name>\n{}\t{workspace_name}\n", workspace.display())),
        (
            "agentsandbox/flake.nix",
            include_str!("../share/agentsandbox/template/agentsandbox/flake.nix").to_owned(),
        ),
    ] {
        fs::write(target.join(name), contents).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn resolve_flake_dir(env: &Env) -> Result<PathBuf, String> {
    if !env.is_global {
        let mut dir = env.workspace.canonicalize().map_err(|err| err.to_string())?;
        loop {
            if dir.join(format!("{LOCAL_CONFIG_DIR}/flake.nix")).is_file() {
                return Ok(dir.join(LOCAL_CONFIG_DIR));
            }
            if !dir.pop() {
                break;
            }
        }
    }
    let config_dir = env.config_root.clone();
    if config_dir.join("flake.nix").is_file() {
        Ok(config_dir)
    } else {
        Err(format!("{} not found", config_dir.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        APP_NAME, Cli, Instance, LOCAL_CONFIG_DIR, prepare, remove_dir_all_if_exists, resolve_env, resolve_flake_dir, resolve_instance, run_destroy, run_init,
    };
    use sha2::{Digest, Sha256};
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
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
            workspace: Some(workspace),
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
        let config = home.join(".config").join(APP_NAME);
        let _ = fs::remove_dir_all(&root);
        (root, home, config)
    }

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn install_fake_commands(root: &Path) -> PathBuf {
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable(&bin_dir.join("virsh"), "#!/bin/sh\nexit 1\n");
        bin_dir
    }

    fn with_fake_path(root: &Path) -> String {
        let old_path = env::var("PATH").unwrap_or_default();
        let fake_bin = install_fake_commands(root);
        unsafe { env::set_var("PATH", format!("{}:{}", fake_bin.display(), old_path)) };
        old_path
    }

    fn restore_path(path: &str) {
        unsafe { env::set_var("PATH", path) };
    }

    fn assert_template(dir: &std::path::Path, mounts: &str) {
        for file in ["flake.nix", "configuration.nix", "allowed_hosts", "agentsandbox/flake.nix"] {
            assert!(dir.join(file).is_file());
        }
        assert_eq!(fs::read_to_string(dir.join("mounts")).unwrap(), mounts);
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
        let mounts = format!("# <host-path><TAB><guest-name>\n{}\tworkspace\n", workspace.canonicalize().unwrap().display());
        let (local_env, global_env) = (
            env_from_input(workspace.clone(), home.clone(), false, None),
            env_from_input(workspace.clone(), home.clone(), true, None),
        );
        run_init(&local_env, false).unwrap();
        let local = workspace.join(LOCAL_CONFIG_DIR);
        run_init(&global_env, false).unwrap();
        for dir in [&local, &global] {
            assert_template(dir, &mounts);
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
        let data_root = env.data_root.clone();
        assert_dirs(&[&data_root]);
        assert_eq!(resolve_flake_dir(&env).unwrap(), flake_dir);
        fs::write(flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let machine_id = Sha256::digest(b"default").iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let existing = format!("renamed-default-0123456789abcdef01234567{}", &machine_id[..8]);
        fs::create_dir_all(data_root.join(&existing)).unwrap();
        assert_eq!(resolve_instance(&env, &flake_dir, "default").unwrap().id, existing);
        let other_flake_dir = root.join("other").join(LOCAL_CONFIG_DIR);
        assert_dirs(&[&other_flake_dir, &root.join("data"), &root.join("state"), &root.join("runtime")]);
        fs::write(other_flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let paths = resolve_instance(
            &env_from_input(
                "/target".into(),
                "/home".into(),
                false,
                Some(("/config".into(), "/data".into(), "/state".into(), "/runtime".into())),
            ),
            &other_flake_dir,
            "demo",
        )
        .unwrap();
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
        let original_path = with_fake_path(&root);
        run_init(&env_from_input(workspace.clone(), home.clone(), false, None), false).unwrap();
        let env = env_from_input(workspace.clone(), home.clone(), false, None);
        let flake_dir = workspace.join(LOCAL_CONFIG_DIR);
        fs::write(flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let paths = resolve_instance(&env, &flake_dir, "default").unwrap();

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

        restore_path(&original_path);
        fs::remove_dir_all(root).unwrap();
    }
}
