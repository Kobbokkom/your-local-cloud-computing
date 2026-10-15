use anyhow::{Context, Result, bail};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn checked(value: &str) -> Result<&str> {
    if value.contains(['\0', '\n', '\r']) {
        bail!("service arguments cannot contain control characters")
    }
    Ok(value)
}

fn systemd_arg(value: &str) -> Result<String> {
    checked(value)?;
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}

fn xml(value: &str) -> Result<String> {
    checked(value)?;
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn role_name(role: &str) -> Result<&str> {
    match role {
        "hub" | "worker" => Ok(role),
        _ => bail!("service role must be hub or worker"),
    }
}

fn run(command: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(command)
        .args(args)
        .output()
        .with_context(|| format!("run {command}"))?;
    if !output.status.success() {
        bail!(
            "{command} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(())
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        env::var_os("HOME").context("HOME is required for user services")?,
    ))
}

fn systemd_unit(args: &[String], data_dir: &Path, path: &str, role: &str) -> Result<String> {
    let args = args
        .iter()
        .map(|s| systemd_arg(s))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    // Environment= does not expand dollar signs, but unit specifiers still apply.
    let path = checked(path)?
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    let directory = data_dir
        .to_str()
        .context("service data directory must be UTF-8")?;
    checked(directory)?;
    if !data_dir.is_absolute() || directory.trim_end() != directory {
        bail!("service data directory must be absolute with no trailing whitespace")
    }
    // WorkingDirectory takes the full value, not ExecStart-style quoted arguments.
    let directory = directory.replace('%', "%%");
    Ok(format!(
        "[Unit]\nDescription=YLCC {role}\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={args}\nWorkingDirectory={directory}\nEnvironment=\"PATH={path}\"\nUMask=0077\nRestart=on-failure\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n"
    ))
}

fn launch_agent(args: &[String], data_dir: &Path, path: &str, role: &str) -> Result<String> {
    let args = args
        .iter()
        .map(|s| Ok(format!("<string>{}</string>", xml(s)?)))
        .collect::<Result<Vec<_>>>()?
        .join("\n");
    let log = xml(&data_dir
        .join(format!("service-{role}.log"))
        .to_string_lossy())?;
    let err = xml(&data_dir
        .join(format!("service-{role}.err.log"))
        .to_string_lossy())?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>org.ylcc.{role}</string>\n<key>ProgramArguments</key><array>\n{args}\n</array>\n<key>WorkingDirectory</key><string>{}</string>\n<key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n<key>ThrottleInterval</key><integer>3</integer>\n<key>Umask</key><integer>63</integer>\n<key>StandardOutPath</key><string>{log}</string>\n<key>StandardErrorPath</key><string>{err}</string>\n</dict></plist>\n",
        xml(&data_dir.to_string_lossy())?,
        xml(path)?
    ))
}

fn service_arguments(
    role: &str,
    hub: &str,
    token_file: &Path,
    data_dir: &Path,
    id: &str,
    listen: &str,
    max_jobs: usize,
) -> Result<(Vec<String>, PathBuf)> {
    role_name(role)?;
    if max_jobs == 0 {
        bail!("max-jobs must be at least 1")
    }
    let executable = env::current_exe()?.canonicalize()?;
    let token_file = token_file
        .canonicalize()
        .context("token file must already exist")?;
    if !token_file.is_file() {
        bail!("token-file must be a regular file")
    }
    fs::File::open(&token_file).context("token file must be readable by the service user")?;
    fs::create_dir_all(data_dir)?;
    let data_dir = data_dir.canonicalize()?;
    let mut args = vec![
        executable.to_string_lossy().into_owned(),
        "--hub".into(),
        hub.into(),
        "--token-file".into(),
        token_file.to_string_lossy().into_owned(),
        role.into(),
        "--data-dir".into(),
        data_dir.to_string_lossy().into_owned(),
    ];
    if role == "hub" {
        args.extend(["--listen".into(), listen.into()]);
    } else {
        args.extend([
            "--id".into(),
            id.into(),
            "--max-jobs".into(),
            max_jobs.to_string(),
        ]);
    }
    for arg in &args {
        checked(arg)?;
    }
    Ok((args, data_dir))
}

/// Install a service for the current user; no root identity or system service is used.
pub fn install(
    role: &str,
    hub: &str,
    token_file: &Path,
    data_dir: &Path,
    id: &str,
    listen: &str,
    max_jobs: usize,
) -> Result<()> {
    let (args, data_dir) =
        service_arguments(role, hub, token_file, data_dir, id, listen, max_jobs)?;
    let path = env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    match env::consts::OS {
        "linux" => {
            let directory = home()?.join(".config/systemd/user");
            fs::create_dir_all(&directory)?;
            let name = format!("ylcc-{role}.service");
            fs::write(
                directory.join(&name),
                systemd_unit(&args, &data_dir, &path, role)?,
            )?;
            run("systemctl", &["--user", "daemon-reload"])?;
            run("systemctl", &["--user", "enable", &name])?;
            run("systemctl", &["--user", "restart", &name])?;
            println!(
                "Installed {name}. Boot without login requires the administrator to enable linger for this user."
            );
        }
        "macos" => {
            let directory = home()?.join("Library/LaunchAgents");
            fs::create_dir_all(&directory)?;
            let label = format!("org.ylcc.{role}");
            let domain = format!("gui/{}", unsafe { libc::getuid() });
            let target = format!("{domain}/{label}");
            let file = directory.join(format!("{label}.plist"));
            if Command::new("launchctl")
                .args(["print", &target])
                .output()?
                .status
                .success()
            {
                run("launchctl", &["bootout", &target])?;
            }
            fs::write(&file, launch_agent(&args, &data_dir, &path, role)?)?;
            run("launchctl", &["enable", &target])?;
            run(
                "launchctl",
                &["bootstrap", &domain, &file.to_string_lossy()],
            )?;
            println!(
                "Installed {label}. This LaunchAgent runs in the current user's GUI login session; the external data volume must be mounted."
            );
        }
        os => bail!("native user services are unsupported on {os}"),
    }
    Ok(())
}

pub fn uninstall(role: &str) -> Result<()> {
    role_name(role)?;
    match env::consts::OS {
        "linux" => {
            let name = format!("ylcc-{role}.service");
            let file = home()?.join(".config/systemd/user").join(&name);
            if file.exists() {
                run("systemctl", &["--user", "disable", "--now", &name])?;
                fs::remove_file(file)?;
                run("systemctl", &["--user", "daemon-reload"])?;
            }
        }
        "macos" => {
            let label = format!("org.ylcc.{role}");
            let target = format!("gui/{}/{label}", unsafe { libc::getuid() });
            if Command::new("launchctl")
                .args(["print", &target])
                .output()?
                .status
                .success()
            {
                run("launchctl", &["bootout", &target])?;
            }
            let file = home()?
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"));
            if file.exists() {
                fs::remove_file(file)?;
            }
        }
        os => bail!("native user services are unsupported on {os}"),
    }
    println!("Removed {role} service; token, downloaded binary, and workload data are retained.");
    Ok(())
}

fn managed_label(role: &str, user: &str) -> Result<String> {
    role_name(role)?;
    if user.is_empty()
        || !user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        || user == "root"
    {
        bail!("system-user must name an existing non-root account")
    }
    Ok(format!("com.kobbokkom.ylcc-{role}"))
}

#[cfg(target_os = "macos")]
fn check_managed_owner(file: &Path, user: &str) -> Result<()> {
    if !file.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(file)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("refusing non-regular service definition")
    }
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "UserName", "raw", "-o", "-"])
        .arg(file)
        .output()?;
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != user {
        bail!(
            "existing system service belongs to another account; refusing to replace or remove it"
        )
    }
    Ok(())
}

/// A privileged supervisor installer; the workload itself always runs as the named account.
#[allow(clippy::too_many_arguments)]
pub fn install_system_user(
    role: &str,
    hub: &str,
    token_file: &Path,
    data_dir: &Path,
    id: &str,
    listen: &str,
    max_jobs: usize,
    service_user: &str,
) -> Result<()> {
    let label = managed_label(role, service_user)?;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (label, hub, token_file, data_dir, id, listen, max_jobs);
        bail!(
            "--system-user is supported on macOS; on Linux install a user service under the dedicated account and enable linger"
        )
    }
    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            bail!(
                "installing a system LaunchDaemon requires an administrator (sudo); the workload runs unprivileged"
            )
        }
        let account = Command::new("/usr/bin/id")
            .args(["-u", service_user])
            .output()?;
        if !account.status.success() || String::from_utf8_lossy(&account.stdout).trim() == "0" {
            bail!("system-user must be an existing non-root account")
        }
        let group = Command::new("/usr/bin/id")
            .args(["-gn", service_user])
            .output()?;
        if !group.status.success() {
            bail!("cannot resolve service primary group")
        }
        let group = String::from_utf8(group.stdout)?.trim().to_owned();
        let record = Command::new("/usr/bin/dscl")
            .args([
                ".",
                "-read",
                &format!("/Users/{service_user}"),
                "NFSHomeDirectory",
            ])
            .output()?;
        if !record.status.success() {
            bail!("cannot resolve service account home")
        }
        let record = String::from_utf8(record.stdout)?;
        let home = record
            .strip_prefix("NFSHomeDirectory: ")
            .context("unexpected account home record")?
            .trim();
        if !Path::new(home).is_absolute() {
            bail!("service account HOME must be absolute")
        }
        // Check real access as the target account before touching a running service.
        for (flag, path) in [
            ("-r", token_file),
            ("-w", data_dir),
            ("-x", env::current_exe()?.as_path()),
        ] {
            let status = Command::new("/usr/bin/sudo")
                .args(["-n", "-u", service_user, "--", "/bin/test", flag])
                .arg(path)
                .status()?;
            if !status.success() {
                bail!("service account lacks {flag} access to {}", path.display())
            }
        }
        let (args, data_dir) =
            service_arguments(role, hub, token_file, data_dir, id, listen, max_jobs)?;
        let path = format!(
            "{home}/.local/bin:{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        );
        let plist = launch_agent(&args, &data_dir, &path, role)?
            .replace(&format!("<string>org.ylcc.{role}</string>"), &format!("<string>{label}</string>\n<key>UserName</key><string>{}</string>\n<key>GroupName</key><string>{}</string>", xml(service_user)?, xml(&group)?))
            .replace("<key>EnvironmentVariables</key><dict>", &format!("<key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string>", xml(home)?));
        let file = PathBuf::from("/Library/LaunchDaemons").join(format!("{label}.plist"));
        check_managed_owner(&file, service_user)?;
        let target = format!("system/{label}");
        if Command::new("launchctl")
            .args(["print", &target])
            .output()?
            .status
            .success()
        {
            run("launchctl", &["bootout", &target])?;
        }
        fs::write(&file, plist)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644))?;
        run("launchctl", &["enable", &target])?;
        run(
            "launchctl",
            &["bootstrap", "system", &file.to_string_lossy()],
        )?;
        println!(
            "Installed {label} as {service_user}; workload data volumes must be mounted before startup."
        );
        Ok(())
    }
}

pub fn uninstall_system_user(role: &str, service_user: &str) -> Result<()> {
    let label = managed_label(role, service_user)?;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
        bail!("--system-user is supported on macOS only")
    }
    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            bail!("removing a system LaunchDaemon requires an administrator (sudo)")
        }
        let file = PathBuf::from("/Library/LaunchDaemons").join(format!("{label}.plist"));
        if !file.exists() {
            return Ok(());
        }
        check_managed_owner(&file, service_user)?;
        let target = format!("system/{label}");
        if Command::new("launchctl")
            .args(["print", &target])
            .output()?
            .status
            .success()
        {
            run("launchctl", &["bootout", &target])?;
        }
        fs::remove_file(file)?;
        println!("Removed {label}; token, binary, and workload data are retained.");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(target_os = "macos")]
    fn managed_access_probe_uses_the_macos_test_binary() {
        assert!(
            Command::new("/bin/test")
                .args(["-x", "/bin/test"])
                .status()
                .unwrap()
                .success()
        );
    }
    #[test]
    fn systemd_does_not_expand_paths_or_inject_lines() {
        let unit = systemd_unit(
            &["/a b/ylcc".into(), "a\";$HOME%h\\x".into()],
            Path::new("/data dir"),
            "/bin:$HOME/%h",
            "worker",
        )
        .unwrap();
        assert!(unit.contains("ExecStart=\"/a b/ylcc\" \"a\\\";$$HOME%%h\\\\x\""));
        assert!(unit.contains("Environment=\"PATH=/bin:$HOME/%%h\""));
        assert!(systemd_arg("x\nExecStart=/evil").is_err());
    }
    #[test]
    fn systemd_working_directory_is_an_unquoted_absolute_value() {
        let unit = systemd_unit(
            &["/usr/bin/true".into()],
            Path::new("/data dir/$cache%h"),
            "/usr/bin:/bin",
            "worker",
        )
        .unwrap();
        assert!(unit.contains("\nWorkingDirectory=/data dir/$cache%%h\n"));
        assert!(!unit.contains("WorkingDirectory=\""));
        assert!(
            systemd_unit(
                &["/usr/bin/true".into()],
                Path::new("relative"),
                "/bin",
                "worker"
            )
            .is_err()
        );
        if Command::new("systemd-analyze")
            .arg("--version")
            .output()
            .is_ok()
        {
            let directory = tempfile::Builder::new()
                .prefix("ylcc service % ")
                .tempdir()
                .unwrap();
            let path = directory.path().join("ylcc-worker.service");
            fs::write(
                &path,
                systemd_unit(
                    &["/usr/bin/true".into()],
                    directory.path(),
                    "/usr/bin:/bin",
                    "worker",
                )
                .unwrap(),
            )
            .unwrap();
            let output = Command::new("systemd-analyze")
                .arg("verify")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "systemd unit rejected: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    #[test]
    fn system_supervisor_requires_an_explicit_nonroot_account() {
        assert!(managed_label("worker", "root").is_err());
        assert!(managed_label("worker", "-bad;name").is_err());
        assert!(managed_label("hub", "").is_err());
        assert_eq!(
            managed_label("worker", "_ylcc").unwrap(),
            "com.kobbokkom.ylcc-worker"
        );
    }
    #[test]
    fn launchd_escapes_untrusted_xml() {
        let plist = launch_agent(
            &["/bin/ylcc".into(), "<&\"' >".into()],
            Path::new("/data&cache"),
            "/bin",
            "worker",
        )
        .unwrap();
        assert!(plist.contains("&lt;&amp;&quot;&apos; &gt;"));
        assert!(plist.contains("/data&amp;cache"));
        assert!(!plist.contains("<string><&"));
        assert!(role_name("worker\nanything").is_err());
        #[cfg(target_os = "macos")]
        {
            let file = tempfile::NamedTempFile::new().unwrap();
            fs::write(file.path(), &plist).unwrap();
            assert!(
                Command::new("/usr/bin/plutil")
                    .arg("-lint")
                    .arg(file.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
}
