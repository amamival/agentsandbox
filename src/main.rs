use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use reqwest::{blocking::Client, header};
use rustix::process::getuid;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
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
        /// Permit removing the resolved global config dir
        #[arg(short = 'g', long)]
        global: bool,
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
    cwd: PathBuf,
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
    let env = resolve_env().unwrap_or_else(|err| {
        eprintln!("{err}");
        process::exit(1);
    });
    if let Err(err) = match Cli::parse().command {
        Some(Command::Version) => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Command::Init { global, force }) => run_init(&env, global, force),
        Some(Command::Build) => run_build(&env),
        Some(Command::Up { detach: _ }) => run_build(&env),
        Some(Command::Down) => run_down(&env),
        Some(Command::Kill) => run_kill(&env),
        Some(Command::Pause) => run_pause(&env),
        Some(Command::Unpause) => run_unpause(&env),
        Some(Command::Ps) => run_ps(&env),
        Some(Command::Destroy {
            system,
            data,
            logs,
            conf,
            global,
        }) => run_destroy(&env, system, data, logs, conf, global),
        None | Some(_) => Ok(()),
    } {
        eprintln!("{err}");
        process::exit(1);
    }
}

fn resolve_env() -> Result<Env, String> {
    let uid = getuid().as_raw();
    let home = (env::var_os("HOME").map(PathBuf::from)).unwrap_or_else(|| env::current_dir().unwrap());
    Ok(Env {
        cwd: env::current_dir().map_err(|err| err.to_string())?,
        config_root: (env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .unwrap_or_else(|| home.join(".config"))
            .join(APP_NAME),
        data_root: (env::var_os("XDG_DATA_HOME").map(PathBuf::from))
            .unwrap_or_else(|| home.join(".local/share"))
            .join(APP_NAME),
        state_root: (env::var_os("XDG_STATE_HOME").map(PathBuf::from))
            .unwrap_or_else(|| home.join(".local/state"))
            .join(APP_NAME),
        runtime_root: (env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
            .unwrap_or_else(|| if uid == 0 { "/run".into() } else { format!("/run/user/{uid}").into() })
            .join(APP_NAME),
    })
}

// Create the config dir and initial files for local/global init, or return a displayable error.
fn run_init(env: &Env, global: bool, force: bool) -> Result<(), String> {
    let target = if global { env.config_root.clone() } else { env.cwd.join(LOCAL_CONFIG_DIR) };
    if target.exists() && !force {
        return Err(format!("{} already exists", target.display()));
    }
    fs::create_dir_all(&target).map_err(|err| err.to_string())?;
    let cwd = env.cwd.canonicalize().map_err(|err| err.to_string())?;
    let workspace_name = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("failed to derive workspace name".to_owned())?;
    fs::create_dir_all(target.join("agentsandbox")).map_err(|err| err.to_string())?;
    for (name, contents) in [
        ("flake.nix", include_str!("../share/agentsandbox/template/flake.nix").to_owned()),
        ("configuration.nix", include_str!("../share/agentsandbox/template/configuration.nix").to_owned()),
        ("allowed_hosts", String::new()),
        ("mounts", format!("# <host-path><TAB><guest-name>\n{}\t{workspace_name}\n", cwd.display())),
        (
            "agentsandbox/flake.nix",
            include_str!("../share/agentsandbox/template/agentsandbox/flake.nix").to_owned(),
        ),
    ] {
        fs::write(target.join(name), contents).map_err(|err| err.to_string())?;
    }
    eprintln!("init: wrote template files to {}", target.display());
    Ok(())
}

fn run_build(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, "default")?;
    prepare(&instance)?;
    if !instance.sysroot.join("nix/store").is_dir() {
        fetch_nix_dockerhub(&instance.sysroot)?;
    }
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

fn run_down(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, "default")?;
    virsh(&["shutdown", instance.id.as_str()]);
    Ok(())
}

fn run_kill(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, "default")?;
    virsh(&["destroy", instance.id.as_str()]);
    Ok(())
}

fn run_pause(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, "default")?;
    virsh(&["suspend", instance.id.as_str()]);
    Ok(())
}

fn run_unpause(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, "default")?;
    virsh(&["resume", instance.id.as_str()]);
    Ok(())
}

fn run_ps(env: &Env) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, "default")?;
    let output = process::Command::new("virsh")
        .arg("domstate")
        .arg(instance.id.as_str())
        .output()
        .map_err(|err| err.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let state = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else if stderr.contains("failed to get domain") {
        "down".to_owned()
    } else {
        stderr
    };
    println!("{}\t{}", instance.id, state);
    Ok(())
}

fn run_destroy(env: &Env, system: bool, data: bool, logs: bool, conf: bool, global: bool) -> Result<(), String> {
    let flake_dir = resolve_flake_dir(env)?;
    let instance = resolve_instance(env, &flake_dir, "default")?;
    virsh(&["destroy", instance.id.as_str()]);
    if instance.is_global && !global {
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

fn resolve_instance(env: &Env, flake_dir: &Path, profile: &str) -> Result<Instance, String> {
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
        Sha256::digest(profile.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>()
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
        .unwrap_or_else(|| format!("{name}-{profile}-{machine_id}"));
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

fn resolve_flake_dir(env: &Env) -> Result<PathBuf, String> {
    let mut dir = env.cwd.canonicalize().map_err(|err| err.to_string())?;
    loop {
        if dir.join(format!("{LOCAL_CONFIG_DIR}/flake.nix")).is_file() {
            return Ok(dir.join(LOCAL_CONFIG_DIR));
        }
        if !dir.pop() {
            break;
        }
    }
    let flake_dir = env.config_root.clone();
    if flake_dir.join("flake.nix").is_file() {
        Ok(flake_dir)
    } else {
        Err(format!("{} not found", flake_dir.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::{APP_NAME, Env, Instance, LOCAL_CONFIG_DIR, prepare, remove_dir_all_if_exists, resolve_flake_dir, resolve_instance, run_destroy, run_init};
    use sha2::{Digest, Sha256};
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::{Mutex, OnceLock},
    };

    const TEST_ENV_DIRS: [&str; 3] = ["data", "state", "runtime"];

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn test_env(root: &std::path::Path, cwd: std::path::PathBuf, config_root: std::path::PathBuf) -> Env {
        Env {
            cwd,
            config_root,
            data_root: root.join(TEST_ENV_DIRS[0]),
            state_root: root.join(TEST_ENV_DIRS[1]),
            runtime_root: root.join(TEST_ENV_DIRS[2]),
        }
    }

    fn test_root(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let root = env::temp_dir().join(format!("{APP_NAME}-rs-{name}"));
        let home = root.join("home");
        let config = home.join(".config").join(APP_NAME);
        let _ = fs::remove_dir_all(&root);
        unsafe { env::set_var("HOME", &home) };
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
        write_executable(&bin_dir.join("kill"), "#!/bin/sh\nexit 0\n");
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

    fn write_runtime_helper_files(runtime_dir: &Path) {
        fs::create_dir_all(runtime_dir.join("virtiofs")).unwrap();
        for file in [
            runtime_dir.join("mount-helper.pid"),
            runtime_dir.join("proxy.pid"),
            runtime_dir.join("opensnitch-forward.pid"),
            runtime_dir.join("opensnitch-watchdog.pid"),
            runtime_dir.join("virtiofs/nix.pid"),
            runtime_dir.join("virtiofs/persistent.pid"),
            runtime_dir.join("proxy-addon.py"),
            runtime_dir.join("virtiofs/nix.sock"),
            runtime_dir.join("virtiofs/persistent.sock"),
        ] {
            fs::write(file, "123\n").unwrap();
        }
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

    #[test]
    fn init_writes_expected_files_and_honors_force() {
        let _guard = env_lock().lock().unwrap();
        let (root, home, global) = test_root("init");
        let workspace = root.join("workspace");
        assert_dirs(&[&workspace, &home]);
        let mounts = format!("# <host-path><TAB><guest-name>\n{}\tworkspace\n", workspace.canonicalize().unwrap().display());
        let env = test_env(&root, workspace.clone(), global.clone());
        run_init(&env, false, false).unwrap();
        let local = workspace.join(LOCAL_CONFIG_DIR);
        run_init(&env, true, false).unwrap();
        for dir in [&local, &global] {
            assert_template(dir, &mounts);
        }
        assert_eq!(run_init(&env, false, false).unwrap_err(), format!("{} already exists", local.display()));
        fs::write(global.join("allowed_hosts"), "stale\n").unwrap();
        run_init(&env, true, true).unwrap();
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
        let data_root = root.join("data");
        assert_dirs(&[&home, &workspace, &data_root]);
        run_init(&test_env(&root, workspace_root.clone(), global.clone()), false, false).unwrap();
        assert_dirs(&[&global]);
        fs::write(global.join("flake.nix"), "").unwrap();
        let env = test_env(&root, workspace.clone(), global.clone());
        assert_eq!(resolve_flake_dir(&env).unwrap(), flake_dir);
        fs::write(flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let machine_id = Sha256::digest(b"default").iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let existing = format!("renamed-default-0123456789abcdef01234567{}", &machine_id[..8]);
        fs::create_dir_all(data_root.join(&existing)).unwrap();
        assert_eq!(resolve_instance(&env, &flake_dir, "default").unwrap().id, existing);
        let other_flake_dir = root.join("other").join(LOCAL_CONFIG_DIR);
        assert_dirs(&[&other_flake_dir, &root.join("data"), &root.join("state"), &root.join("runtime")]);
        fs::write(other_flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let paths = resolve_instance(&test_env(std::path::Path::new("/"), "/cwd".into(), "/config".into()), &other_flake_dir, "demo").unwrap();
        let demo_machine_id = Sha256::digest(b"demo").iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        assert_eq!(paths.id, "other-demo-0123456789abcdef01234567".to_owned() + &demo_machine_id[..8]);
        assert_eq!(paths.data_dir, PathBuf::from("/data").join(&paths.id));
        assert_eq!(paths.state_dir, PathBuf::from("/state").join(&paths.id));
        assert_eq!(paths.runtime_dir, PathBuf::from("/runtime").join(&paths.id));
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
        assert_eq!(resolve_flake_dir(&env).unwrap(), global);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn destroy_respects_flag_combinations() {
        let _guard = env_lock().lock().unwrap();
        let (root, home, global) = test_root("destroy");
        let workspace = root.join("workspace");
        assert_dirs(&[&home, &workspace]);
        let original_path = with_fake_path(&root);
        run_init(&test_env(&root, workspace.clone(), global.clone()), false, false).unwrap();
        let env = test_env(&root, workspace.clone(), global.clone());
        let flake_dir = workspace.join(LOCAL_CONFIG_DIR);
        fs::write(flake_dir.join("machine-prefix"), "0123456789abcdef01234567").unwrap();
        let paths = resolve_instance(&env, &flake_dir, "default").unwrap();

        for (system, data, expect_sysroot, expect_persistent, expect_data_dir) in [
            (false, false, true, true, true),
            (true, false, false, true, true),
            (false, true, true, false, true),
            (true, true, false, false, false),
        ] {
            remove_dir_all_if_exists(&paths.data_dir).unwrap();
            remove_dir_all_if_exists(&paths.state_dir).unwrap();
            remove_dir_all_if_exists(&paths.runtime_dir).unwrap();
            fs::create_dir_all(&paths.sysroot).unwrap();
            fs::create_dir_all(&paths.persistent).unwrap();
            fs::create_dir_all(&paths.logs_dir).unwrap();
            write_runtime_helper_files(&paths.runtime_dir);

            run_destroy(&env, system, data, false, false, false).unwrap();

            assert_eq!(paths.sysroot.exists(), expect_sysroot);
            assert_eq!(paths.persistent.exists(), expect_persistent);
            assert_eq!(paths.data_dir.exists(), expect_data_dir);
            assert!(!paths.runtime_dir.join("mount-helper.pid").exists());
            assert!(!paths.runtime_dir.join("proxy-addon.py").exists());
        }

        fs::create_dir_all(&paths.sysroot).unwrap();
        fs::create_dir_all(&paths.persistent).unwrap();
        fs::create_dir_all(&paths.logs_dir).unwrap();
        write_runtime_helper_files(&paths.runtime_dir);
        run_destroy(&env, false, false, true, false, false).unwrap();
        assert!(!paths.state_dir.exists());
        assert!(flake_dir.exists());

        fs::create_dir_all(&paths.sysroot).unwrap();
        fs::create_dir_all(&paths.persistent).unwrap();
        fs::create_dir_all(&paths.logs_dir).unwrap();
        write_runtime_helper_files(&paths.runtime_dir);
        run_destroy(&env, false, false, false, true, false).unwrap();
        assert!(!flake_dir.exists());

        restore_path(&original_path);
        fs::remove_dir_all(root).unwrap();
    }
}
