use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use reqwest::{blocking::Client, header};
use rustix::{
    io::{read, write},
    mount::{MountFlags, UnmountFlags},
    mount::{mount, mount_bind_recursive, mount_remount, unmount},
    pipe::{PipeFlags, pipe_with},
    process::{WaitOptions, chdir, getuid, pivot_root, waitpid},
    runtime::{Fork, exit_group, kernel_fork},
    thread::{UnshareFlags, unshare_unsafe},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
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
    let cli = Cli::parse();
    let env = resolve_env(&cli).unwrap_or_else(|err| {
        eprintln!("{err}");
        process::exit(1);
    });
    if let Err(err) = match cli.command {
        Some(Command::Version) => Ok(println!("{}", env!("CARGO_PKG_VERSION"))),
        Some(Command::Init { force }) => run_init(&env, force),
        Some(Command::Build { bootstrap }) => run_build(&env, bootstrap).map(|p| println!("{}", p.display())),
        Some(Command::Up { detach }) => run_up(&env, detach),
        Some(Command::Down) => run_virsh_action(&env, "shutdown"),
        Some(Command::Kill) => run_virsh_action(&env, "destroy"),
        Some(Command::Pause) => run_virsh_action(&env, "suspend"),
        Some(Command::Unpause) => run_virsh_action(&env, "resume"),
        Some(Command::Ps) => run_ps(&env),
        Some(Command::Destroy { system, data, logs, conf }) => run_destroy(&env, system, data, logs, conf),
        Some(Command::Ssh { args }) => run_ssh(&env, &args),
        None | Some(_) => Ok(println!("Comming soon(tm)...")),
    } {
        eprintln!("{err}");
        process::exit(1);
    }
}

#[inline(never)]
fn resolve_env(cli: &Cli) -> Result<Env, String> {
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
fn run_init(env: &Env, force: bool) -> Result<(), String> {
    let target = if env.is_global {
        env.config_root.clone()
    } else {
        env.workspace.join(LOCAL_CONFIG_DIR)
    };
    write_template_config(&target, &env.workspace, force)?;
    eprintln!("init: wrote template files to {}", target.display());
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

#[inline(never)]
fn run_build(env: &Env, bootstrap: bool) -> Result<PathBuf, String> {
    let instance = resolve_instance(env, &resolve_flake_dir(env)?)?;
    prepare(&instance)?;
    if !instance.sysroot.join("nix/var/nix/profiles/default").is_symlink() {
        fetch_nix_dockerhub(&instance.sysroot)?;
    }
    if bootstrap || !instance.sysroot.join("nix/var/nix/profiles/system").is_symlink() {
        install_initial_nixos_profile(&env.workspace, &instance.sysroot, &env.hostname)?;
    }
    // TODO: launch VM with special target (build-system-only.target), force re-build in guest.
    //start_vm(&env, &instance);
    // loop { wait for ssh to be ready }
    //run_ssh(&instance, build /etc/nixos config and finally poweroff);
    // TODO: Wait for guest to finish build, down the VM, and read the system profile path.
    //virsh(&instance.id, "destroy");
    //match run_wait(&instance, &["crashed", "shut off"]) {
    //    Ok("shut off") => { ... system profile path ... },
    //    Ok(other) => Err(format!("VM is not shut off: {other}")),
    //    Err(err) => err,
    //}
    let system_profile = fs::read_link(instance.sysroot.join("nix/var/nix/profiles/system")).map_err(|err| err.to_string())?;
    Ok(instance.sysroot.join(system_profile))

    // if "build" and not running,
    // *start_vm to build-system-only.target*.
    // wait ssh ready, run ssh to build config *then shutdown*.
    // *destroy vm*.
    // print profile path.

    // if "build" and running,
    // wait ssh ready. run ssh to build config *then report if xml hash changed (you need to shutdown and restart to fully take effect)*.
    // print profile path.

    // if "up" and not running,
    // *start_vm to build-system-only.target*.
    // wait ssh ready, run ssh to build config *then report and shutdown if xml hash changed (restarting because of VM config change), or start multi-user.target if hash not changed*.
    // *destroy vm if hash changed, start_vm to multi-user.target*.

    // if "up" and running,
    // wait ssh ready. run ssh to build config *then report if xml hash changed (you need to shutdown and restart to fully take effect)*.
    // apply switch to new profile if user flagged --switch to do so.

    // if "up" and --no-build,
    // start_vm to multi-user.target.
}

#[inline(never)]
fn resolve_flake_dir(env: &Env) -> Result<PathBuf, String> {
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
        Err(format!("{} not found", env.config_root.display()))
    }
}

#[inline(never)]
fn resolve_instance(env: &Env, flake_dir: &Path) -> Result<Instance, String> {
    let prefix_file = flake_dir.join("machine-prefix");
    let mut prefix = fs::read_to_string(&prefix_file).unwrap_or_default();
    if prefix.is_empty() {
        prefix = Sha256::digest(fs::canonicalize(flake_dir).map_err(|err| err.to_string())?.display().to_string().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()[..24]
            .into();
        fs::write(&prefix_file, &prefix).map_err(|err| err.to_string())?;
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
fn prepare(instance: &Instance) -> Result<(), String> {
    for dir in [&instance.sysroot, &instance.persistent, &instance.logs_dir, &instance.runtime_dir] {
        fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[inline(never)]
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
        let response = client.get(format!("{registry}/{repo}/blobs/{blob}")).header(header::AUTHORIZATION, &auth);
        let response = response.send().map_err(|err| err.to_string())?;
        let response = response.error_for_status().map_err(|err| err.to_string())?;
        tar::Archive::new(GzDecoder::new(response)).unpack(sysroot).map_err(|err| err.to_string())?;
    }
    eprintln!("fetch: completed docker image extraction");
    Ok(())
}

// The initial profile is assumed safe, so building it in a simple container is acceptable.
#[inline(never)]
fn install_initial_nixos_profile(workspace: &Path, sysroot: &Path, hostname: &str) -> Result<(), String> {
    let config_target = sysroot.join("etc/nixos");
    eprintln!("install: writing template config into {}", config_target.display());
    write_template_config(&config_target, workspace, true)?;

    // Remove previous out-link so `nix build --out-link /nix/var/nix/profiles/system` can overwrite it.
    let _ = fs::remove_file(sysroot.join("nix/var/nix/profiles/system"));

    // Get compatible uid/gid maps from host.
    let capture_map = |label: &str, path: &str| -> Result<String, String> {
        eprintln!("install: capturing {label} map via unshare");
        let output = process::Command::new("unshare")
            .args(["--user", "--map-root-user", "--map-auto", "cat", path])
            .output()
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    };
    let uid_map = capture_map("uid", "/proc/self/uid_map")?;
    let gid_map = capture_map("gid", "/proc/self/gid_map")?;

    // Create pipe for child/parent communication.
    let (child_ready_read, child_ready_write) = pipe_with(PipeFlags::CLOEXEC).map_err(|err| err.to_string())?;
    let (parent_done_read, parent_done_write) = pipe_with(PipeFlags::CLOEXEC).map_err(|err| err.to_string())?;
    match unsafe { kernel_fork() }.map_err(|err| err.to_string())? {
        Fork::Child(_) => {
            drop(child_ready_read);
            drop(parent_done_write);
            let status = match (|| {
                eprintln!("install: entering user+mount namespace");
                // Unshare to new mount+user(less privileged) namespace.
                // All previous mounts are inherited. All shared mounts are reduced to slave.
                unsafe { unshare_unsafe(UnshareFlags::NEWUSER | UnshareFlags::NEWNS) }.map_err(|err| err.to_string())?;
                write_fd(&child_ready_write)?; // I'm ready in new namespace.
                wait_for_pipe_signal(&parent_done_read, "parent uid/gid map install")?; // Parent wrote uid/gid maps.

                // Prepare new file system hierarchy.
                let oldroot = Path::new("/");
                eprintln!("install: creating mountpoint dir sysroot/dev");
                fs::create_dir_all(sysroot.join("dev")).map_err(|err| err.to_string())?;
                eprintln!("install: mounting tmpfs to sysroot/dev");
                mount("tmpfs", sysroot.join("dev"), "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, c"mode=0755").map_err(|err| err.to_string())?;
                for dir in ["dev/shm", "dev/pts", "proc", "tmp"] {
                    eprintln!("install: creating mountpoint dir sysroot/{dir}");
                    fs::create_dir_all(sysroot.join(dir)).map_err(|err| err.to_string())?;
                }
                // Python's _multiprocessing.SemLock expects a writable 1777 /dev/shm.
                eprintln!("install: mounting tmpfs to sysroot/dev/shm");
                mount("tmpfs", sysroot.join("dev/shm"), "tmpfs", MountFlags::NODEV | MountFlags::NOSUID, c"mode=01777").map_err(|err| err.to_string())?;
                eprintln!("install: mounting devpts to sysroot/dev/pts");
                let opts = c"newinstance,ptmxmode=0666,mode=620";
                mount("devpts", sysroot.join("dev/pts"), "devpts", MountFlags::NOSUID | MountFlags::NOEXEC, opts).map_err(|err| err.to_string())?;
                eprintln!("install: binding host /proc to sysroot/proc");
                mount_bind_recursive(oldroot.join("proc"), sysroot.join("proc")).map_err(|err| err.to_string())?;
                // Bind host devices etc to new root's /dev.
                for file in ["dev/null", "dev/zero", "dev/full", "dev/random", "dev/urandom", "dev/tty", "etc/resolv.conf"] {
                    eprintln!("install: touching sysroot/{file} and binding host /{file}");
                    fs::write(sysroot.join(file), "").map_err(|err| err.to_string())?;
                    mount_bind_recursive(oldroot.join(file), sysroot.join(file)).map_err(|err| err.to_string())?;
                }
                eprintln!("install: remounting sysroot/etc/resolv.conf read-only");
                mount_remount(sysroot.join("etc/resolv.conf"), MountFlags::BIND | MountFlags::RDONLY, c"").map_err(|err| err.to_string())?;
                eprintln!("install: creating symlinks for /dev/{{stdin,stdout,stderr,fd,core,ptmx}}");
                for (fd, file) in [(0, "dev/stdin"), (1, "dev/stdout"), (2, "dev/stderr")] {
                    symlink(format!("/proc/self/fd/{fd}"), sysroot.join(file)).map_err(|err| err.to_string())?;
                }
                symlink("/proc/self/fd", sysroot.join("dev/fd")).map_err(|err| err.to_string())?;
                symlink("/proc/kcore", sysroot.join("dev/core")).map_err(|err| err.to_string())?;
                symlink("pts/ptmx", sysroot.join("dev/ptmx")).map_err(|err| err.to_string())?;

                eprintln!("install: pivoting root: / => (sysroot)/tmp, sysroot => /");
                // pivot_root() new_root must be a mountpoint. Bind sysroot to itself.
                mount_bind_recursive(sysroot, sysroot).map_err(|err| err.to_string())?;
                // Pivot root: / => (sysroot)/tmp, sysroot => /.
                pivot_root(sysroot, sysroot.join("tmp")).map_err(|err| err.to_string())?;
                eprintln!("install: detaching host / (currently pivoted to /tmp)");
                unmount("/tmp", UnmountFlags::DETACH).map_err(|err| err.to_string())?; // Unmount old root.
                eprintln!("install: cd to the brand new root / (sysroot)");
                chdir("/").map_err(|err| err.to_string())?;
                // Err(process::Command::new("cat").args(["/proc/self/mountinfo"]).exec().to_string())

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
                wait_for_pipe_signal(&child_ready_read, "child namespace setup")?;
                let set_idmap = |kind: &str, id_map: &str| -> Result<(), String> {
                    eprintln!("install: assigning /etc/sub{kind} ranges to child {child_pid} via new{kind}map");
                    let status = process::Command::new(format!("new{kind}map"))
                        .arg(child_pid.as_raw_pid().to_string())
                        .args(id_map.split_whitespace().map(str::to_owned))
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .status()
                        .map_err(|err| err.to_string())?;
                    (status.success().then_some(())).ok_or_else(|| {
                        format!(
                            "new{kind}map failed with status {}",
                            status.code().map_or("signal".to_owned(), |c| c.to_string())
                        )
                    })
                };
                set_idmap("uid", &uid_map)?;
                eprintln!("install: writing setgroups deny for child {child_pid}");
                fs::write(format!("/proc/{}/setgroups", child_pid.as_raw_pid()), "deny\n").map_err(|err| err.to_string())?;
                set_idmap("gid", &gid_map)?;
                write_fd(&parent_done_write)
            })();
            drop(parent_done_write); // Child will see EOF.
            let status = waitpid(Some(child_pid), WaitOptions::empty()) // Reap zombie process.
                .map_err(|err| err.to_string())?
                .ok_or("install: child disappeared before waitpid reported status".to_owned())?
                .1;
            match (parent_error, status.exit_status(), status.terminating_signal()) {
                (Err(err), _, _) => Err(err),
                (_, Some(0), _) => Ok(()),
                (_, Some(code), _) => Err(format!("install: child exited with status {code}")),
                (_, None, Some(signal)) => Err(format!("install: child terminated by signal {signal}")),
                (_, None, None) => Err(format!("install: child ended unexpectedly: {status:?}")),
            }
        }
    }
}

fn write_fd(fd: &std::os::fd::OwnedFd) -> Result<(), String> {
    if write(fd, &[1]).map_err(|err| err.to_string())? == 1 {
        Ok(())
    } else {
        Err("short write to pipe".to_owned())
    }
}

fn wait_for_pipe_signal(fd: &std::os::fd::OwnedFd, stage: &str) -> Result<(), String> {
    let mut byte = [0_u8; 1];
    if read(fd, &mut byte).map_err(|err| err.to_string())? == 1 {
        Ok(())
    } else {
        Err(format!("{stage} failed before signaling readiness"))
    }
}
#[inline(never)]
fn run_up(env: &Env, _detach: bool) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
    prepare(&instance)?;
    if let Some(state) = domstate(&instance.id)? {
        return Err(format!("VM is already {state}"));
    }
    let system_profile = run_build(env, false)?;
    let capture_map = |label: &str, path: &str| -> Result<String, String> {
        eprintln!("up: capturing {label} map via unshare");
        let output = process::Command::new("unshare")
            .args(["--user", "--map-root-user", "--map-auto", "cat", path])
            .output()
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    };
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
        .env("DOMAIN_UUID", &domain_uuid)
        .env("GID_MAP", capture_map("gid", "/proc/self/gid_map")?)
        .env("INSTANCE_ID", &instance.id)
        .env("MACHINE_ID", machine_id)
        .env("NIX_DIR", instance.sysroot.join("nix"))
        .env("PERSISTENT_DIR", &instance.persistent)
        .env("RUNTIME_DIR", &instance.runtime_dir)
        .env("UID_MAP", capture_map("uid", "/proc/self/uid_map")?)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    fs::write(&xml_path, output.stdout).map_err(|err| err.to_string())?;
    let status = process::Command::new("virsh")
        .arg("create")
        .arg(&xml_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|err| err.to_string())?;
    status.success().then_some(()).ok_or_else(|| {
        format!(
            "virsh create failed with status {}",
            status.code().map_or("signal".to_owned(), |code| code.to_string())
        )
    })
}

#[inline(never)]
fn run_virsh_action(env: &Env, action: &str) -> Result<(), String> {
    let instance = resolve_instance(env, &resolve_flake_dir(env)?)?;
    virsh(&[action, &instance.id]);
    Ok(())
}

#[inline(never)]
fn run_ps(env: &Env) -> Result<(), String> {
    let instance = resolve_instance(env, &resolve_flake_dir(env)?)?;
    println!("{}\t{}", instance.id, domstate(&instance.id)?.unwrap_or_else(|| "down".to_owned()));
    Ok(())
}

#[inline(never)]
fn run_destroy(env: &Env, system: bool, data: bool, logs: bool, conf: bool) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
    virsh(&["destroy", &instance.id]);
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

#[inline(never)]
fn domstate(instance_id: &str) -> Result<Option<String>, String> {
    let output = process::Command::new("virsh")
        .arg("domstate")
        .arg(instance_id)
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("failed to get domain") {
        Ok(None)
    } else {
        Err(stderr.trim().to_owned())
    }
}

fn remove_dir_all_if_exists(path: &Path) -> Result<(), String> {
    if path.exists() {
        eprintln!("remove: removing {}", path.display());
        fs::remove_dir_all(path).map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[inline(never)]
fn virsh(args: &[&str]) {
    let _ = process::Command::new("virsh")
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
}

#[inline(never)]
fn run_ssh(env: &Env, args: &[String]) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir)?;
    if domstate(&instance.id)?.as_deref() != Some("running") {
        return Err("VM is not running".to_owned());
    }
    let output = process::Command::new("nix")
        .arg("eval")
        .arg("--json")
        .arg(format!(
            "{}#nixosConfigurations.{hostname}.config.agentsandbox.portForwards",
            flake_dir.display(),
            hostname = env.hostname
        ))
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let port_forwards: Value = serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())?;
    let ssh_port = port_forwards
        .as_object()
        .and_then(|forwards| {
            for forward in forwards.values() {
                if forward.get("proto")?.as_str()? != "tcp" {
                    continue;
                }
                let host = forward.get("host")?;
                let (start, end) = if let Some(host_port) = host.as_u64() {
                    (host_port, host_port)
                } else {
                    (host.get("start")?.as_u64()?, host.get("end")?.as_u64()?)
                };
                let guest = forward.get("guest")?.as_u64()?;
                if (22_u64 >= guest) && (22_u64 <= guest + (end - start)) {
                    return u16::try_from(start + 22_u64 - guest).ok();
                }
            }
            None
        })
        .ok_or("ssh port forward for guest tcp/22 is not configured".to_owned())?;
    Err(process::Command::new("ssh")
        .args(["vscode@127.0.0.1", "-p"])
        .arg(ssh_port.to_string())
        .args(["-o", "LogLevel=ERROR", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null"])
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .exec()
        .to_string())
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
