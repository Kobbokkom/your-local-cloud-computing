use crate::model::*;
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{RwLock, Semaphore, broadcast, watch},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    report: AttemptReport,
    seq: u64,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    container: Option<String>,
}
struct Attempt {
    dir: PathBuf,
    journal: Mutex<Journal>,
    cancel: watch::Sender<bool>,
}
struct State {
    hub: String,
    token: String,
    data: PathBuf,
    id: String,
    client: reqwest::Client,
    capabilities: Capabilities,
    max_jobs: usize,
    slots: Arc<Semaphore>,
    drain: RwLock<()>,
    attempts: Mutex<HashMap<String, Arc<Attempt>>>,
    environments: Mutex<BTreeMap<String, EnvironmentStatus>>,
    events: broadcast::Sender<WorkerMessage>,
}

fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
        "invalid identifier"
    );
    Ok(())
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    serde_json::to_writer(&mut file, value)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
impl Attempt {
    fn report(&self) -> AttemptReport {
        self.journal.lock().unwrap().report.clone()
    }
    fn update(&self, change: impl FnOnce(&mut Journal)) -> Result<AttemptReport> {
        let mut journal = self.journal.lock().unwrap();
        change(&mut journal);
        atomic_json(&self.dir.join("journal.json"), &*journal)?;
        Ok(journal.report.clone())
    }
    fn log(&self, state: &State, stream: &str, text: String) -> Result<()> {
        let mut journal = self.journal.lock().unwrap();
        journal.seq += 1;
        let entry = LogEntry {
            job_id: journal.report.job_id.clone(),
            attempt_id: journal.report.attempt_id.clone(),
            seq: journal.seq,
            stream: stream.into(),
            text,
        };
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(self.dir.join("logs.jsonl"))?;
        serde_json::to_writer(&mut file, &entry)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        // The append-only log owns the sequence; journal need not be rewritten for every chunk.
        let _ = state.events.send(WorkerMessage::Log { entry });
        Ok(())
    }
}
fn env_key(id: &str, revision: &str) -> String {
    format!("{id}/{revision}")
}
impl State {
    fn environment_dir(&self, environment: &Environment) -> Result<PathBuf> {
        identifier(&environment.id)?;
        identifier(&environment.revision)?;
        Ok(self
            .data
            .join("environments")
            .join(&environment.id)
            .join(&environment.revision))
    }
    fn emit(&self, message: WorkerMessage) {
        let _ = self.events.send(message);
    }
    fn environment_status(
        &self,
        environment: &Environment,
        status: &str,
        error: Option<String>,
    ) -> Result<()> {
        let value = EnvironmentStatus {
            worker_id: self.id.clone(),
            environment_id: environment.id.clone(),
            revision: environment.revision.clone(),
            state: status.into(),
            error,
        };
        let dir = self.environment_dir(environment)?;
        private_dir(&dir)?;
        atomic_json(&dir.join("status.json"), &value)?;
        self.environments.lock().unwrap().insert(
            env_key(&environment.id, &environment.revision),
            value.clone(),
        );
        self.emit(WorkerMessage::Environment { status: value });
        Ok(())
    }
    fn hello(&self) -> WorkerMessage {
        WorkerMessage::Hello {
            worker_id: self.id.clone(),
            capabilities: self.capabilities.clone(),
            max_jobs: self.max_jobs,
            attempts: self
                .attempts
                .lock()
                .unwrap()
                .values()
                .map(|a| a.report())
                .collect(),
            environments: self
                .environments
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect(),
        }
    }
}

pub async fn run(
    hub: String,
    token_file: PathBuf,
    data_dir: PathBuf,
    id: String,
    max_jobs: usize,
) -> Result<()> {
    identifier(&id)?;
    ensure!(max_jobs > 0, "max-jobs must be positive");
    crate::client::validate_hub(&hub)?;
    let token = crate::client::read_token(&token_file)?;
    private_dir(&data_dir)?;
    let data = fs::canonicalize(data_dir)?;
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data.join("worker.lock"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        ensure!(
            unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another worker is using this data directory"
        );
    }
    for child in ["attempts", "blobs", "environments"] {
        private_dir(&data.join(child))?;
    }
    let capabilities = probe().await?;
    let (events, _) = broadcast::channel(4096);
    let state = Arc::new(State {
        hub: hub.trim_end_matches('/').into(),
        token,
        data,
        id,
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        capabilities,
        max_jobs,
        slots: Arc::new(Semaphore::new(max_jobs)),
        drain: RwLock::new(()),
        attempts: Mutex::new(HashMap::new()),
        environments: Mutex::new(BTreeMap::new()),
        events,
    });
    load_state(&state).await?;
    let mut retry = 1;
    loop {
        match connection(state.clone()).await {
            Ok(()) => retry = 1,
            Err(error) => eprintln!("worker connection: {error:#}; reconnecting in {retry}s"),
        }
        tokio::time::sleep(Duration::from_secs(retry)).await;
        retry = (retry * 2).min(30);
    }
}

async fn load_state(state: &Arc<State>) -> Result<()> {
    for entry in fs::read_dir(state.data.join("attempts"))? {
        let dir = entry?.path();
        if !dir.is_dir() {
            continue;
        }
        let path = dir.join("journal.json");
        if !path.exists() {
            continue;
        }
        let mut journal: Journal = serde_json::from_reader(File::open(&path)?)?;
        identifier(&journal.report.attempt_id)?;
        ensure!(
            dir.file_name().and_then(|s| s.to_str()) == Some(&journal.report.attempt_id),
            "attempt directory identity mismatch"
        );
        let logs = dir.join("logs.jsonl");
        if logs.exists() {
            let mut good_bytes = 0_u64;
            for line in BufReader::new(File::open(&logs)?).split(b'\n') {
                let line = line?;
                match serde_json::from_slice::<LogEntry>(&line) {
                    Ok(log) => {
                        ensure!(
                            log.attempt_id == journal.report.attempt_id
                                && log.job_id == journal.report.job_id,
                            "log identity mismatch"
                        );
                        journal.seq = journal.seq.max(log.seq);
                        good_bytes += line.len() as u64 + 1;
                    }
                    Err(_) => break,
                }
            }
            OpenOptions::new()
                .write(true)
                .open(&logs)?
                .set_len(good_bytes)?;
        }
        if !matches!(
            journal.report.state.as_str(),
            "succeeded" | "failed" | "cancelled" | "unknown"
        ) {
            // A process can outlive its worker. A PID is not enough proof to reattach or kill it safely.
            journal.report.state = "unknown".into();
            journal.report.error = Some("worker restarted before completion; inspect the original process before resubmitting".into());
            atomic_json(&path, &journal)?;
        }
        let (cancel, _) = watch::channel(false);
        state.attempts.lock().unwrap().insert(
            journal.report.attempt_id.clone(),
            Arc::new(Attempt {
                dir,
                journal: Mutex::new(journal),
                cancel,
            }),
        );
    }
    for env in fs::read_dir(state.data.join("environments"))? {
        let path = env?.path();
        if !path.is_dir() {
            continue;
        }
        for revision in fs::read_dir(path)? {
            let path = revision?.path().join("status.json");
            if !path.exists() {
                continue;
            }
            let mut status: EnvironmentStatus = serde_json::from_reader(File::open(&path)?)?;
            identifier(&status.environment_id)?;
            identifier(&status.revision)?;
            if status.state == "preparing" {
                status.state = "failed".into();
                status.error =
                    Some("worker restarted during preparation; explicitly prepare again".into());
                atomic_json(&path, &status)?;
            }
            if status.worker_id == state.id {
                state
                    .environments
                    .lock()
                    .unwrap()
                    .insert(env_key(&status.environment_id, &status.revision), status);
            }
        }
    }
    let statuses: Vec<_> = state
        .environments
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    for status in statuses
        .into_iter()
        .filter(|status| status.state == "ready")
    {
        let dir = state
            .data
            .join("environments")
            .join(&status.environment_id)
            .join(&status.revision);
        let result = async {
            let definition: Environment =
                serde_json::from_reader(File::open(dir.join("definition.json"))?)?;
            let image = if definition.image.is_some() {
                Some(resolved_image(state, &definition)?)
            } else {
                None
            };
            let logger = environment_logger(state, &definition)?;
            verify_environment(state, &definition, image.as_deref(), &logger).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            let mut status = status;
            status.state = "failed".into();
            status.error = Some(format!("startup environment verification: {error:#}"));
            atomic_json(&dir.join("status.json"), &status)?;
            state
                .environments
                .lock()
                .unwrap()
                .insert(env_key(&status.environment_id, &status.revision), status);
        }
    }
    Ok(())
}

async fn connection(state: Arc<State>) -> Result<()> {
    let mut url = reqwest::Url::parse(&state.hub)?;
    ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(
                    url.host_str(),
                    Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
                )),
        "use HTTPS for non-loopback hubs"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "hub URL must not contain credentials, query or fragment"
    );
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => bail!("hub URL must be HTTP(S)"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid hub URL"))?;
    url.set_path(&format!(
        "{}/v1/worker/ws",
        url.path().trim_end_matches('/')
    ));
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {}", state.token).parse()?);
    let mut events = state.events.subscribe();
    let (socket, _) = connect_async(request).await?;
    let (mut sink, mut source) = socket.split();
    sink.send(Message::Text(serde_json::to_string(&state.hello())?.into()))
        .await?;
    let attempts: Vec<_> = state.attempts.lock().unwrap().values().cloned().collect();
    for attempt in attempts {
        let logs = attempt.dir.join("logs.jsonl");
        if logs.exists() {
            let file = {
                let _journal = attempt.journal.lock().unwrap();
                let file = File::open(logs)?;
                let size = file.metadata()?.len();
                file.take(size)
            };
            for line in BufReader::new(file).lines() {
                let line = line?;
                let entry: LogEntry = serde_json::from_str(&line)?;
                sink.send(Message::Text(
                    serde_json::to_string(&WorkerMessage::Log { entry })?.into(),
                ))
                .await?;
            }
        }
        let report = attempt.report();
        if matches!(
            report.state.as_str(),
            "succeeded" | "failed" | "cancelled" | "unknown"
        ) {
            sink.send(Message::Text(
                serde_json::to_string(&WorkerMessage::Finished { report })?.into(),
            ))
            .await?;
        }
    }
    let statuses: Vec<_> = state
        .environments
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    for status in statuses {
        let logs = state
            .data
            .join("environments")
            .join(status.environment_id)
            .join(status.revision)
            .join("logs.jsonl");
        if !logs.exists() {
            continue;
        }
        let file = File::open(logs)?;
        let limit = file.metadata()?.len();
        for line in BufReader::new(file.take(limit)).lines() {
            let line = line?;
            // A live preparation may be in the middle of its final append; its event is already subscribed.
            if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
                sink.send(Message::Text(
                    serde_json::to_string(&WorkerMessage::Log { entry })?.into(),
                ))
                .await?;
            }
        }
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            value = source.next() => match value {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<HubMessage>(&text)? {
                    HubMessage::Run { job, environment } => accept(state.clone(), *job, environment)?,
                    HubMessage::Cancel { job_id, attempt_id } => {
                        if let Some(attempt) = state.attempts.lock().unwrap().get(&attempt_id) {
                            ensure!(attempt.report().job_id == job_id, "cancel job identity mismatch"); let _ = attempt.cancel.send(true);
                        }
                    }
                    HubMessage::Prepare { environment } => {
                        let state = state.clone(); tokio::spawn(async move { if let Err(e) = prepare(&state, &environment, true).await { eprintln!("environment preparation: {e:#}"); } });
                    }
                },
                Some(Ok(Message::Ping(bytes))) => sink.send(Message::Pong(bytes)).await?,
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Err(error)) => return Err(error.into()),
                _ => {},
            },
            event = events.recv() => match event {
                Ok(message) => sink.send(Message::Text(serde_json::to_string(&message)?.into())).await?,
                Err(broadcast::error::RecvError::Lagged(_)) => bail!("event stream lagged; reconnect to replay durable logs"),
                Err(_) => return Ok(()),
            },
            _ = heartbeat.tick() => sink.send(Message::Text(serde_json::to_string(&WorkerMessage::Heartbeat)?.into())).await?,
        }
    }
}

fn accept(state: Arc<State>, job: Job, environment: Option<Environment>) -> Result<()> {
    identifier(&job.id)?;
    identifier(&job.attempt_id)?;
    job.spec.validate()?;
    if let Some(environment) = &environment {
        state.environment_dir(environment)?;
    }
    let mut attempts = state.attempts.lock().unwrap();
    if let Some(existing) = attempts.get(&job.attempt_id) {
        ensure!(
            existing.report().job_id == job.id,
            "attempt belongs to another job"
        );
        let report = existing.report();
        if matches!(
            report.state.as_str(),
            "succeeded" | "failed" | "cancelled" | "unknown"
        ) {
            state.emit(WorkerMessage::Finished { report });
        }
        return Ok(());
    }
    let dir = state.data.join("attempts").join(&job.attempt_id);
    private_dir(&dir)?;
    let report = AttemptReport {
        job_id: job.id.clone(),
        attempt_id: job.attempt_id.clone(),
        state: "assigned".into(),
        exit_code: None,
        error: None,
        results: vec![],
    };
    let journal = Journal {
        report,
        seq: 0,
        pid: None,
        container: None,
    };
    atomic_json(&dir.join("journal.json"), &journal)?;
    atomic_json(&dir.join("job.json"), &job.spec)?;
    let (cancel, receiver) = watch::channel(false);
    let attempt = Arc::new(Attempt {
        dir,
        journal: Mutex::new(journal),
        cancel,
    });
    attempts.insert(job.attempt_id.clone(), attempt.clone());
    drop(attempts);
    tokio::spawn(async move {
        let result = run_attempt(
            &state,
            &attempt,
            &job,
            environment.as_ref(),
            receiver.clone(),
        )
        .await;
        let outcome = attempt.update(|journal| {
            journal.pid = None;
            journal.container = None;
            match result {
                Ok((exit, outputs)) => {
                    journal.report.exit_code = Some(exit);
                    journal.report.results = outputs;
                    journal.report.state = if exit == 0 { "succeeded" } else { "failed" }.into();
                }
                Err(error) => {
                    journal.report.state = if *receiver.borrow() {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    journal.report.error = Some(format!("{error:#}"));
                }
            }
        });
        match outcome {
            Ok(report) => state.emit(WorkerMessage::Finished { report }),
            Err(error) => eprintln!("cannot persist attempt outcome: {error:#}"),
        }
    });
    Ok(())
}

fn cancelled(cancel: &watch::Receiver<bool>) -> Result<()> {
    ensure!(!*cancel.borrow(), "job cancelled");
    Ok(())
}
fn expanded_env(
    environment: Option<&Environment>,
    job: Option<&JobSpec>,
    dir: &Path,
    image: bool,
) -> BTreeMap<String, String> {
    let env_dir = if image {
        "/ylcc-env".into()
    } else {
        dir.to_string_lossy().into_owned()
    };
    let base_path = if image {
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()
    } else {
        std::env::var("PATH").unwrap_or_default()
    };
    let mut values = BTreeMap::new();
    values.insert("PATH".into(), format!("{env_dir}/bin:{base_path}"));
    if image {
        values.insert("HOME".into(), env_dir.clone());
        values.insert("XDG_CACHE_HOME".into(), format!("{env_dir}/cache"));
    }
    for source in [environment.map(|e| &e.env), job.map(|j| &j.env)]
        .into_iter()
        .flatten()
    {
        for (key, value) in source {
            let path = values
                .get("PATH")
                .cloned()
                .unwrap_or_else(|| base_path.clone());
            values.insert(
                key.clone(),
                value
                    .replace("${YLCC_ENV_DIR}", &env_dir)
                    .replace("${PATH}", &path),
            );
        }
    }
    values.insert("YLCC_ENV_DIR".into(), env_dir);
    values
}
fn runtime(state: &State) -> Result<&str> {
    state.capabilities.runtimes.first().map(String::as_str).context("no usable Docker or Podman runtime; install/configure one under the worker service account")
}
fn image_arch(arch: &str) -> Result<&str> {
    match arch {
        "aarch64" | "arm64" => Ok("arm64"),
        "x86_64" | "amd64" => Ok("amd64"),
        _ => bail!("unsupported image architecture {arch}"),
    }
}
#[allow(clippy::too_many_arguments)]
fn image_command(
    state: &State,
    image: &str,
    command: &[String],
    dir: &Path,
    env_dir: &Path,
    values: &BTreeMap<String, String>,
    name: &str,
    gpu: Option<&str>,
) -> Result<Command> {
    ensure!(!command.is_empty(), "empty image command");
    let runtime = runtime(state)?;
    let mut process = Command::new(runtime);
    process.args([
        "run",
        "--rm",
        "--name",
        name,
        "--platform",
        &format!("linux/{}", image_arch(&state.capabilities.arch)?),
        "--workdir",
        "/workspace",
    ]);
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        if runtime == "podman" && uid != 0 {
            process.args(["--userns", "keep-id"]);
        }
        process.args(["--user", &format!("{uid}:{gid}")]);
    }
    for (source, target) in [(dir, "/workspace"), (env_dir, "/ylcc-env")] {
        let source = source.to_str().context("non-UTF8 mount path")?;
        ensure!(!source.contains(','), "container bind path contains comma");
        process.args(["--mount", &format!("type=bind,src={source},dst={target}")]);
    }
    if let Some(gpu) = gpu {
        match (runtime, gpu) {
            ("docker", "cuda") => {
                process.args(["--gpus", "all"]);
            }
            ("podman", "cuda") => {
                process.args(["--device", "nvidia.com/gpu=all"]);
            }
            (_, "rocm") => {
                process.args(["--device", "/dev/kfd", "--device", "/dev/dri"]);
            }
            _ => bail!("GPU backend {gpu} cannot be passed to a Linux container"),
        }
    }
    for (key, value) in values {
        process.args(["--env", &format!("{key}={value}")]);
    }
    process.args(["--entrypoint", &command[0], image]);
    process.args(&command[1..]);
    Ok(process)
}

async fn pipe_logs(
    mut reader: impl AsyncRead + Unpin,
    state: Arc<State>,
    attempt: Arc<Attempt>,
    stream: &'static str,
) -> Result<()> {
    let mut bytes = vec![0; 8192];
    loop {
        let n = reader.read(&mut bytes).await?;
        if n == 0 {
            break;
        }
        attempt.log(
            &state,
            stream,
            String::from_utf8_lossy(&bytes[..n]).into_owned(),
        )?;
    }
    Ok(())
}
async fn stop_process(child: &mut tokio::process::Child, container: Option<(&str, &str)>) {
    if let Some((runtime, name)) = container {
        let mut cleanup = Command::new(runtime);
        cleanup
            .args(["rm", "-f", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let _ = tokio::time::timeout(Duration::from_secs(15), cleanup.status()).await;
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}
async fn execute(
    state: &Arc<State>,
    attempt: &Arc<Attempt>,
    mut command: Command,
    mut cancel: watch::Receiver<bool>,
    timeout: u64,
    container: Option<(String, String)>,
) -> Result<i32> {
    cancelled(&cancel)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let mut child = command.spawn().context("start command")?;
    attempt.update(|journal| {
        journal.pid = child.id();
        journal.container = container.as_ref().map(|(_, name)| name.clone());
    })?;
    let stdout = tokio::spawn(pipe_logs(
        child.stdout.take().unwrap(),
        state.clone(),
        attempt.clone(),
        "stdout",
    ));
    let stderr = tokio::spawn(pipe_logs(
        child.stderr.take().unwrap(),
        state.clone(),
        attempt.clone(),
        "stderr",
    ));
    let outcome = tokio::select! {
        result = child.wait() => result.map_err(anyhow::Error::from),
        _ = cancel.changed() => { stop_process(&mut child, container.as_ref().map(|(r,n)| (r.as_str(),n.as_str()))).await; Err(anyhow::anyhow!("job cancelled")) },
        _ = tokio::time::sleep(Duration::from_secs(timeout)) => { stop_process(&mut child, container.as_ref().map(|(r,n)| (r.as_str(),n.as_str()))).await; Err(anyhow::anyhow!("command exceeded timeout of {timeout}s")) },
    };
    // Descendants may retain stdout after their parent exits. Close the group before waiting for pipes.
    #[cfg(unix)]
    if let Some(pid) = attempt.journal.lock().unwrap().pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    let logs = async {
        stdout.await??;
        stderr.await??;
        Ok::<_, anyhow::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(5), logs)
        .await
        .context("command descendants retained output pipes")??;
    let status = outcome?;
    Ok(status.code().unwrap_or(128 + signal_number(&status)))
}
fn signal_number(status: &std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        0
    }
}

async fn run_attempt(
    state: &Arc<State>,
    attempt: &Arc<Attempt>,
    job: &Job,
    environment: Option<&Environment>,
    mut cancel: watch::Receiver<bool>,
) -> Result<(i32, Vec<FileEntry>)> {
    cancelled(&cancel)?;
    if let Some(environment) = environment {
        ensure!(
            environment.image.is_some() || job.spec.container.is_none(),
            "native environments cannot be combined with an ad-hoc container; register an image environment instead"
        );
        let ready = state
            .environments
            .lock()
            .unwrap()
            .get(&env_key(&environment.id, &environment.revision))
            .is_some_and(|e| e.state == "ready");
        if !ready {
            prepare(state, environment, false).await?;
        }
    }
    let _slot = tokio::select! { slot = state.slots.acquire() => slot?, _ = cancel.changed() => bail!("job cancelled") };
    let _drain = tokio::select! { guard = state.drain.read() => guard, _ = cancel.changed() => bail!("job cancelled") };
    cancelled(&cancel)?;
    let env_dir = if let Some(environment) = environment {
        state.environment_dir(environment)?
    } else {
        attempt.dir.join("environment")
    };
    private_dir(&env_dir)?;
    let image = if let Some(environment) = environment.filter(|e| e.image.is_some()) {
        Some(resolved_image(state, environment)?)
    } else {
        job.spec.container.clone()
    };
    let workdir = attempt.dir.join("work");
    fs::create_dir(&workdir).context("create fresh attempt checkout")?;
    for input in &job.spec.inputs {
        cancelled(&cancel)?;
        let blob = ensure_blob(state, &input.hash, Some(input.size)).await?;
        let dest = workdir.join(&input.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(blob, &dest)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dest, fs::Permissions::from_mode(input.mode & 0o777))?;
        }
    }
    let mut values = expanded_env(environment, Some(&job.spec), &env_dir, image.is_some());
    values.insert("YLCC_JOB_ID".into(), job.id.clone());
    values.insert("YLCC_ATTEMPT_ID".into(), job.attempt_id.clone());
    if let Some(image) = image.as_deref() {
        ensure!(
            job.spec
                .requirements
                .os
                .as_deref()
                .is_none_or(|os| os == "linux"),
            "containers execute Linux, not the native host OS"
        );
        ensure!(
            job.spec
                .requirements
                .arch
                .as_deref()
                .is_none_or(|arch| normalize_arch(arch) == state.capabilities.arch),
            "image execution architecture mismatch"
        );
        if environment.is_none() {
            ensure!(
                job.spec.requirements.tools.is_empty()
                    && job.spec.requirements.abi.is_none()
                    && job.spec.requirements.build_target.is_none(),
                "image tool/ABI/target requirements need a verified environment"
            );
        }
        let context = Some((&**state, image, workdir.as_path(), env_dir.as_path()));
        verify_tools(&job.spec.requirements, &values, context).await?;
        if !job.spec.requirements.cpu_features.is_empty() {
            let command = image_command(
                state,
                image,
                &["cat".into(), "/proc/cpuinfo".into()],
                &workdir,
                &env_dir,
                &values,
                &format!("ylcc-cpu-{}", uuid::Uuid::new_v4()),
                None,
            )?;
            let text = capture(command).await?;
            let flags: Vec<_> = text
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("flags")
                        .or_else(|| line.strip_prefix("Features"))
                        .and_then(|line| line.split_once(':'))
                        .map(|(_, rest)| rest)
                })
                .flat_map(str::split_whitespace)
                .collect();
            ensure!(
                job.spec
                    .requirements
                    .cpu_features
                    .iter()
                    .all(|feature| flags.contains(&feature.as_str())),
                "CPU features unavailable in actual container runtime"
            );
        }
        verify_container_mount(state, image, &workdir, &env_dir, &values).await?;
        if let Some(gpu) = &job.spec.requirements.gpu {
            verify_image_gpu(state, image, &workdir, &env_dir, &values, gpu).await?;
        }
    } else {
        verify_tools(&job.spec.requirements, &values, None).await?;
        let mut base = job.spec.requirements.clone();
        base.tools.clear();
        base.build_target = None;
        ensure!(
            state.capabilities.mismatch(&base).is_empty(),
            "worker no longer satisfies requirements"
        );
    }
    cancelled(&cancel)?;
    let mut container = None;
    let command = if let Some(image) = image.as_ref() {
        let name = format!("ylcc-{}", job.attempt_id);
        let process = image_command(
            state,
            image,
            &job.spec.command,
            &workdir,
            &env_dir,
            &values,
            &name,
            job.spec.requirements.gpu.as_deref(),
        )?;
        container = Some((runtime(state)?.into(), name));
        process
    } else {
        let mut process = Command::new(&job.spec.command[0]);
        process
            .args(&job.spec.command[1..])
            .current_dir(&workdir)
            .envs(&values);
        process
    };
    attempt.update(|journal| journal.report.state = "running".into())?;
    state.emit(WorkerMessage::Started {
        job_id: job.id.clone(),
        attempt_id: job.attempt_id.clone(),
    });
    let exit = execute(
        state,
        attempt,
        command,
        cancel.clone(),
        job.spec.timeout_secs.unwrap_or(24 * 60 * 60),
        container,
    )
    .await?;
    cancelled(&cancel)?;
    let mut files = BTreeMap::new();
    for output in &job.spec.outputs {
        if exit != 0
            && fs::symlink_metadata(workdir.join(output))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        collect_output(&workdir, &workdir.join(output), &mut files)?;
    }
    let mut results = vec![];
    for (_, entry) in files {
        cancelled(&cancel)?;
        upload_blob(state, &entry.hash, &workdir.join(&entry.path)).await?;
        results.push(entry);
    }
    Ok((exit, results))
}

async fn verify_container_mount(
    state: &State,
    image: &str,
    workdir: &Path,
    envdir: &Path,
    values: &BTreeMap<String, String>,
) -> Result<()> {
    let sentinel = format!(".ylcc-mount-{}", uuid::Uuid::new_v4());
    let content = uuid::Uuid::new_v4().to_string();
    fs::write(workdir.join(&sentinel), &content)?;
    let result = async {
        let command = image_command(state, image, &["cat".into(), format!("/workspace/{sentinel}")], workdir, envdir, values, &format!("ylcc-mount-{}", uuid::Uuid::new_v4()), None)?;
        ensure!(capture(command).await? == content, "container bind does not reference the worker checkout; use identical absolute paths on worker and runtime host"); Ok::<_,anyhow::Error>(())
    }.await;
    fs::remove_file(workdir.join(sentinel))?;
    result
}

async fn ensure_blob(state: &State, hash: &str, size: Option<u64>) -> Result<PathBuf> {
    validate_hash(hash)?;
    let path = state.data.join("blobs").join(hash);
    if path.exists() {
        verify_file(&path, hash, size)?;
        return Ok(path);
    }
    let response = state
        .client
        .get(format!("{}/v1/blobs/{hash}", state.hub))
        .bearer_auth(&state.token)
        .send()
        .await?
        .error_for_status()?;
    let temp = path.with_extension(format!("{}.part", uuid::Uuid::new_v4()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await?;
        let mut stream = response.bytes_stream();
        let mut digest = Sha256::new();
        let mut written = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            written += chunk.len() as u64;
            ensure!(
                size.is_none_or(|size| written <= size),
                "blob exceeds declared size"
            );
            digest.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        ensure!(
            hex::encode(digest.finalize()) == hash,
            "download SHA-256 mismatch"
        );
        ensure!(
            size.is_none_or(|size| written == size),
            "download size mismatch"
        );
        tokio::fs::rename(&temp, &path).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result?;
    Ok(path)
}
fn verify_file(path: &Path, expected: &str, size: Option<u64>) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "blob is not a regular file"
    );
    ensure!(
        size.is_none_or(|size| size == metadata.len()),
        "blob size mismatch"
    );
    ensure!(file_hash(path)? == expected, "cached blob SHA-256 mismatch");
    Ok(())
}
fn file_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let size = file.read(&mut bytes)?;
        if size == 0 {
            break;
        }
        hash.update(&bytes[..size]);
    }
    Ok(hex::encode(hash.finalize()))
}
async fn upload_blob(state: &State, hash: &str, path: &Path) -> Result<()> {
    verify_file(path, hash, None)?;
    let file = tokio::fs::File::open(path).await?;
    let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
    state
        .client
        .put(format!("{}/v1/blobs/{hash}", state.hub))
        .bearer_auth(&state.token)
        .body(body)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
fn collect_output(
    root: &Path,
    path: &Path,
    result: &mut BTreeMap<String, FileEntry>,
) -> Result<()> {
    let relative = path.strip_prefix(root)?;
    safe_relative(relative.to_str().context("non-UTF8 output path")?)?;
    let mut cursor = root.to_path_buf();
    for part in relative.components() {
        cursor.push(part);
        ensure!(
            !fs::symlink_metadata(&cursor)?.file_type().is_symlink(),
            "output contains symlink: {}",
            relative.display()
        );
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        let mut children: Vec<_> = fs::read_dir(path)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        children.sort();
        for child in children {
            collect_output(root, &child, result)?;
        }
    } else {
        ensure!(metadata.is_file(), "output is not a regular file");
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = 0o644;
        let path_string = relative.to_str().unwrap().to_owned();
        result.insert(
            path_string.clone(),
            FileEntry {
                path: path_string,
                hash: file_hash(path)?,
                size: metadata.len(),
                mode,
            },
        );
    }
    Ok(())
}

async fn capture(mut command: Command) -> Result<String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .context("probe timed out")??;
    ensure!(
        output.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        Ok(String::from_utf8_lossy(&output.stderr).trim().into())
    } else {
        Ok(stdout.trim().into())
    }
}
fn version_token(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            word.trim_start_matches('v')
                .trim_matches(|c: char| c == '(' || c == ')' || c == ',')
        })
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()) && word.contains('.'))
        .unwrap_or(value.lines().next().unwrap_or(""))
        .into()
}
async fn tool_version(
    tool: &str,
    values: &BTreeMap<String, String>,
    image: Option<(&State, &str, &Path, &Path)>,
) -> Result<String> {
    if let Some(sdk) = tool.strip_prefix("sdk:") {
        ensure!(image.is_none(), "Apple SDK requirements need native macOS");
        ensure!(
            !sdk.is_empty()
                && sdk
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "invalid SDK name"
        );
        let mut command = Command::new("xcrun");
        command
            .args(["--sdk", sdk, "--show-sdk-version"])
            .envs(values);
        return capture(command).await;
    }
    ensure!(
        !tool.is_empty()
            && tool
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_+.-".contains(&b)),
        "invalid tool executable name"
    );
    let arg = if tool == "xcodebuild" {
        "-version"
    } else {
        "--version"
    };
    let command = if let Some((state, image, workdir, envdir)) = image {
        image_command(
            state,
            image,
            &[tool.into(), arg.into()],
            workdir,
            envdir,
            values,
            &format!("ylcc-probe-{}", uuid::Uuid::new_v4()),
            None,
        )?
    } else {
        let mut process = Command::new(tool);
        process.arg(arg).envs(values);
        process
    };
    let text = capture(command).await?;
    ensure!(!text.is_empty(), "{tool} returned no version");
    Ok(version_token(&text))
}
async fn verify_tools(
    requirements: &Requirements,
    values: &BTreeMap<String, String>,
    image: Option<(&State, &str, &Path, &Path)>,
) -> Result<()> {
    for (tool, expected) in &requirements.tools {
        let actual = tool_version(tool, values, image)
            .await
            .with_context(|| format!("required tool {tool}"))?;
        ensure!(
            expected == "*" || expected == &actual,
            "required {tool} {expected}, found {actual}"
        );
    }
    if let Some(target) = &requirements.build_target {
        let command = if let Some((state, image, workdir, envdir)) = image {
            image_command(
                state,
                image,
                &["rustc".into(), "-vV".into()],
                workdir,
                envdir,
                values,
                &format!("ylcc-target-{}", uuid::Uuid::new_v4()),
                None,
            )?
        } else {
            let mut process = Command::new("rustc");
            process.arg("-vV").envs(values);
            process
        };
        let text = capture(command).await?;
        ensure!(
            text.lines()
                .any(|line| line.strip_prefix("host: ") == Some(target.as_str())),
            "build target {target} lacks a verified native linker/SDK; cross-compilation is not advertised"
        );
        if let Some((state, image, workdir, envdir)) = image {
            let prefix = format!("/ylcc-env/.target-probe-{}", uuid::Uuid::new_v4());
            let script = format!(
                "trap 'rm -f {prefix}.rs {prefix}' EXIT; printf 'fn main() {{}}\\n' > {prefix}.rs; rustc --crate-name ylcc_probe {prefix}.rs -o {prefix}; {prefix}"
            );
            let command = image_command(
                state,
                image,
                &["sh".into(), "-eu".into(), "-c".into(), script],
                workdir,
                envdir,
                values,
                &format!("ylcc-link-{}", uuid::Uuid::new_v4()),
                None,
            )?;
            capture(command)
                .await
                .context("image target compiler/linker execution probe")?;
        } else {
            let dir = tempfile::tempdir()?;
            let source = dir.path().join("probe.rs");
            let binary = dir.path().join("probe");
            fs::write(&source, "fn main() {}\n")?;
            let mut compiler = Command::new("rustc");
            compiler.arg(source).arg("-o").arg(&binary).envs(values);
            capture(compiler)
                .await
                .context("target compiler/linker probe")?;
            let mut command = Command::new(binary);
            command.envs(values);
            capture(command)
                .await
                .context("native target execution probe")?;
        }
    }
    Ok(())
}
fn resolved_image(state: &State, environment: &Environment) -> Result<String> {
    serde_json::from_reader(File::open(
        state.environment_dir(environment)?.join("image.json"),
    )?)
    .context("environment has no verified local image; prepare it first")
}
fn environment_logger(state: &State, environment: &Environment) -> Result<Arc<Attempt>> {
    let dir = state.environment_dir(environment)?;
    private_dir(&dir)?;
    let seq = if dir.join("logs.jsonl").exists() {
        BufReader::new(File::open(dir.join("logs.jsonl"))?)
            .lines()
            .filter_map(|line| {
                line.ok()
                    .and_then(|line| serde_json::from_str::<LogEntry>(&line).ok())
            })
            .map(|entry| entry.seq)
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let id = format!("environment:{}:{}", environment.id, environment.revision);
    let attempt_id = format!(
        "environment:{}:{}:{}",
        state.id, environment.id, environment.revision
    );
    let (cancel, _) = watch::channel(false);
    Ok(Arc::new(Attempt {
        dir,
        cancel,
        journal: Mutex::new(Journal {
            report: AttemptReport {
                job_id: id,
                attempt_id,
                state: "preparing".into(),
                exit_code: None,
                error: None,
                results: vec![],
            },
            seq,
            pid: None,
            container: None,
        }),
    }))
}
async fn verify_environment(
    state: &Arc<State>,
    environment: &Environment,
    image: Option<&str>,
    logger: &Arc<Attempt>,
) -> Result<()> {
    let dir = state.environment_dir(environment)?;
    let values = expanded_env(Some(environment), None, &dir, image.is_some());
    let context = image.map(|image| (&**state, image, dir.as_path(), dir.as_path()));
    verify_tools(&environment.requirements, &values, context).await?;
    if let Some(image) = image {
        if let Some(expected) = &environment.requirements.abi {
            let script = "if getconf GNU_LIBC_VERSION >/dev/null 2>&1; then printf gnu; elif ldd --version 2>&1 | grep -qi musl; then printf musl; else exit 1; fi";
            let command = image_command(
                state,
                image,
                &["sh".into(), "-c".into(), script.into()],
                &dir,
                &dir,
                &values,
                &format!("ylcc-abi-{}", uuid::Uuid::new_v4()),
                None,
            )?;
            let actual = capture(command).await?;
            ensure!(
                &actual == expected,
                "image ABI is {actual}, expected {expected}"
            );
        }
        if let Some(gpu) = &environment.requirements.gpu {
            verify_image_gpu(state, image, &dir, &dir, &values, gpu).await?;
        }
    }
    for script in &environment.verify {
        logger.log(state, "system", "verifying environment\n".into())?;
        let (cancel, receiver) = watch::channel(false);
        let mut container = None;
        let command = if let Some(image) = image {
            let name = format!("ylcc-verify-{}", uuid::Uuid::new_v4());
            let command = image_command(
                state,
                image,
                &["sh".into(), "-eu".into(), "-c".into(), script.clone()],
                &dir,
                &dir,
                &values,
                &name,
                environment.requirements.gpu.as_deref(),
            )?;
            container = Some((runtime(state)?.into(), name));
            command
        } else {
            let mut command = Command::new("sh");
            command
                .args(["-eu", "-c", script])
                .current_dir(&dir)
                .envs(&values);
            command
        };
        let result = execute(state, logger, command, receiver, 600, container).await;
        drop(cancel);
        ensure!(result? == 0, "environment verification failed");
    }
    Ok(())
}

async fn verify_image_gpu(
    state: &State,
    image: &str,
    workdir: &Path,
    envdir: &Path,
    values: &BTreeMap<String, String>,
    backend: &str,
) -> Result<()> {
    let script = match backend {
        "cuda" => {
            r#"import ctypes as c
l=c.CDLL('libcuda.so.1')
assert l.cuInit(0)==0
n=c.c_int(); assert l.cuDeviceGetCount(c.byref(n))==0 and n.value>0
ctx=c.c_void_p(); l.cuCtxCreate_v2.argtypes=[c.POINTER(c.c_void_p),c.c_uint,c.c_int]
assert l.cuCtxCreate_v2(c.byref(ctx),0,0)==0
p=c.c_uint64(); l.cuMemAlloc_v2.argtypes=[c.POINTER(c.c_uint64),c.c_size_t]
assert l.cuMemAlloc_v2(c.byref(p),4)==0
l.cuMemFree_v2.argtypes=[c.c_uint64]; assert l.cuMemFree_v2(p)==0
l.cuCtxDestroy_v2.argtypes=[c.c_void_p]; assert l.cuCtxDestroy_v2(ctx)==0
print('ready')"#
        }
        "rocm" => {
            r#"import ctypes as c
l=c.CDLL('libamdhip64.so')
assert l.hipInit(0)==0
n=c.c_int(); assert l.hipGetDeviceCount(c.byref(n))==0 and n.value>0
p=c.c_void_p(); l.hipMalloc.argtypes=[c.POINTER(c.c_void_p),c.c_size_t]
assert l.hipMalloc(c.byref(p),4)==0
l.hipFree.argtypes=[c.c_void_p]; assert l.hipFree(p)==0
print('ready')"#
        }
        _ => bail!("GPU backend {backend} is unavailable in Linux containers"),
    };
    let command = image_command(
        state,
        image,
        &["python3".into(), "-c".into(), script.into()],
        workdir,
        envdir,
        values,
        &format!("ylcc-gpu-{}", uuid::Uuid::new_v4()),
        Some(backend),
    )?;
    ensure!(capture(command).await.context("image GPU access probe requires python3 and usable driver libraries/devices in the actual image")? == "ready", "image GPU access verification failed");
    Ok(())
}
async fn prepare(state: &Arc<State>, environment: &Environment, explicit: bool) -> Result<()> {
    let dir = state.environment_dir(environment)?;
    let _drain = state.drain.write().await;
    if !explicit
        && let Some(status) = state
            .environments
            .lock()
            .unwrap()
            .get(&env_key(&environment.id, &environment.revision))
    {
        if status.state == "ready" {
            return Ok(());
        }
        ensure!(
            status.state != "failed",
            "environment preparation failed; inspect environment logs and explicitly prepare again"
        );
    }
    private_dir(&dir)?;
    private_dir(&dir.join("bin"))?;
    let definition = dir.join("definition.json");
    if definition.exists() {
        let existing: Environment = serde_json::from_reader(File::open(&definition)?)?;
        ensure!(
            serde_json::to_value(existing)? == serde_json::to_value(environment)?,
            "environment revision is immutable; choose a new revision"
        );
    } else {
        atomic_json(&definition, environment)?;
    }
    let logger = environment_logger(state, environment)?;
    state.environment_status(environment, "preparing", None)?;
    let result = async {
        let mut requirements = environment.requirements.clone(); requirements.tools.clear(); requirements.build_target = None;
        if environment.image.is_some() { requirements.os = None; requirements.abi = None; }
        ensure!(state.capabilities.mismatch(&requirements).is_empty(), "worker does not satisfy environment platform requirements");
        let mut image_id = None;
        if let Some(image) = &environment.image {
            ensure!(environment.setup.is_empty(), "image setup scripts are not persistent; build a custom image externally or use a native setup recipe");
            ensure!(environment.requirements.os.as_deref().is_none_or(|os| os == "linux"), "image environments execute Linux");
            let runtime = runtime(state)?; let (cancel, receiver) = watch::channel(false);
            let mut command = Command::new(runtime); command.args(["pull", "--platform", &format!("linux/{}", image_arch(&state.capabilities.arch)?), image]);
            let exit = execute(state, &logger, command, receiver, 3600, None).await?; drop(cancel); ensure!(exit == 0, "image pull failed");
            let mut inspect = Command::new(runtime); inspect.args(["image", "inspect", "--format", "{{.Id}}", image]);
            let id = capture(inspect).await?; ensure!(id.starts_with("sha256:") && id.len() == 71, "runtime returned invalid image ID"); image_id = Some(id);
        } else {
            let values = expanded_env(Some(environment), None, &dir, false);
            for script in &environment.setup {
                logger.log(state, "system", "preparing environment\n".into())?;
                let mut command = Command::new("sh"); command.args(["-eu", "-c", script]).current_dir(&dir).envs(&values);
                let (cancel, receiver) = watch::channel(false);
                let exit = execute(state, &logger, command, receiver, 3600, None).await?; drop(cancel); ensure!(exit == 0, "environment setup failed");
            }
        }
        verify_environment(state, environment, image_id.as_deref(), &logger).await?;
        if let Some(image) = image_id { atomic_json(&dir.join("image.json"), &image)?; }
        Ok::<_, anyhow::Error>(())
    }.await;
    match &result {
        Ok(()) => state.environment_status(environment, "ready", None)?,
        Err(error) => {
            state.environment_status(environment, "failed", Some(format!("{error:#}")))?
        }
    }
    result
}

pub async fn probe() -> Result<Capabilities> {
    let os = std::env::consts::OS.to_owned();
    let arch = normalize_arch(std::env::consts::ARCH);
    let mut uname = Command::new("uname");
    uname.arg("-m");
    if let Ok(kernel_arch) = capture(uname).await {
        ensure!(
            normalize_arch(&kernel_arch) == arch,
            "worker binary runs under architecture translation; install the native {} binary",
            normalize_arch(&kernel_arch)
        );
    }
    if os == "macos" {
        let mut translated = Command::new("sysctl");
        translated.args(["-n", "sysctl.proc_translated"]);
        ensure!(
            capture(translated).await.unwrap_or_default() != "1",
            "worker binary runs under Rosetta; install the native arm64 binary"
        );
    }
    let abi = if os == "macos" {
        "darwin"
    } else if cfg!(target_env = "musl") {
        "musl"
    } else {
        "gnu"
    }
    .into();
    let mut tools = BTreeMap::new();
    let values = BTreeMap::new();
    for name in [
        "git",
        "rustc",
        "cargo",
        "node",
        "npm",
        "python3",
        "uv",
        "mise",
        "cc",
        "clang",
        "cmake",
        "make",
        "swift",
        "xcodebuild",
        "docker",
        "podman",
        "nvcc",
    ] {
        if let Ok(version) = tool_version(name, &values, None).await {
            tools.insert(name.into(), version);
        }
    }
    let mut rust = Command::new("rustc");
    rust.arg("-vV");
    if let Ok(version) = capture(rust).await
        && let Some(host) = version.lines().find_map(|line| line.strip_prefix("host: "))
        && let Ok(dir) = tempfile::tempdir()
    {
        let source = dir.path().join("probe.rs");
        let binary = dir.path().join("probe");
        if fs::write(&source, "fn main() {}\n").is_ok() {
            let mut compiler = Command::new("rustc");
            compiler.arg(&source).arg("-o").arg(&binary);
            if capture(compiler).await.is_ok() && capture(Command::new(binary)).await.is_ok() {
                tools.insert(format!("target:{host}"), "ready".into());
            }
        }
    }
    if os == "macos" {
        for sdk in ["macosx", "iphoneos", "iphonesimulator"] {
            let mut command = Command::new("xcrun");
            command.args(["--sdk", sdk, "--show-sdk-version"]);
            if let Ok(version) = capture(command).await {
                tools.insert(format!("sdk:{sdk}"), version);
            }
        }
    }
    let mut runtimes = vec![];
    for runtime in ["podman", "docker"] {
        let mut command = Command::new(runtime);
        command.arg("info");
        if capture(command).await.is_ok() {
            runtimes.push(runtime.into());
        }
    }
    let mut cpu_features = vec![];
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo")
        && let Some(flags) = cpuinfo.lines().find_map(|line| {
            line.strip_prefix("flags")
                .or_else(|| line.strip_prefix("Features"))
                .and_then(|rest| rest.split_once(':'))
                .map(|(_, flags)| flags)
        })
    {
        cpu_features.extend(flags.split_whitespace().map(String::from));
    }
    #[cfg(target_arch = "x86_64")]
    {
        for (name, present) in [
            ("sse2", std::is_x86_feature_detected!("sse2")),
            ("sse4_2", std::is_x86_feature_detected!("sse4.2")),
            ("avx", std::is_x86_feature_detected!("avx")),
            ("avx2", std::is_x86_feature_detected!("avx2")),
            ("avx512f", std::is_x86_feature_detected!("avx512f")),
            ("aes", std::is_x86_feature_detected!("aes")),
            ("fma", std::is_x86_feature_detected!("fma")),
        ] {
            if present {
                cpu_features.push(name.into());
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        for (name, present) in [
            ("neon", std::arch::is_aarch64_feature_detected!("neon")),
            ("aes", std::arch::is_aarch64_feature_detected!("aes")),
            ("sha2", std::arch::is_aarch64_feature_detected!("sha2")),
            ("crc", std::arch::is_aarch64_feature_detected!("crc")),
            ("lse", std::arch::is_aarch64_feature_detected!("lse")),
            ("fp16", std::arch::is_aarch64_feature_detected!("fp16")),
            (
                "dotprod",
                std::arch::is_aarch64_feature_detected!("dotprod"),
            ),
        ] {
            if present {
                cpu_features.push(name.into());
            }
        }
    }
    cpu_features.sort();
    cpu_features.dedup();
    let memory_bytes = if os == "linux" {
        fs::read_to_string("/proc/meminfo")
            .unwrap_or_default()
            .lines()
            .find_map(|line| {
                line.strip_prefix("MemTotal:")
                    .and_then(|line| line.split_whitespace().next())
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .unwrap_or(0)
            * 1024
    } else {
        let mut command = Command::new("sysctl");
        command.args(["-n", "hw.memsize"]);
        capture(command)
            .await
            .unwrap_or_default()
            .parse()
            .unwrap_or(0)
    };
    let containerized = std::env::var("YLCC_CONTAINERIZED").is_ok_and(|value| value == "1")
        || Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists();
    let mut gpu = vec![];
    #[cfg(target_os = "linux")]
    {
        if tokio::task::spawn_blocking(cuda_accessible)
            .await
            .unwrap_or(false)
        {
            gpu.push("cuda".into());
        }
        if tokio::task::spawn_blocking(rocm_accessible)
            .await
            .unwrap_or(false)
        {
            gpu.push("rocm".into());
        }
    }
    if os == "macos" && !containerized {
        let script = r##"import Metal
import MetalPerformanceShaders
guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue(), let buffer = device.makeBuffer(length: 4), let library = try? device.makeLibrary(source: "#include <metal_stdlib>\nusing namespace metal; kernel void probe(device uint *p [[buffer(0)]]) { p[0] = 42; }", options: nil), let function = library.makeFunction(name: "probe"), let pipeline = try? device.makeComputePipelineState(function: function), let command = queue.makeCommandBuffer(), let encoder = command.makeComputeCommandEncoder() else { fatalError("Metal unavailable") }
encoder.setComputePipelineState(pipeline); encoder.setBuffer(buffer, offset: 0, index: 0); encoder.dispatchThreads(MTLSize(width: 1, height: 1, depth: 1), threadsPerThreadgroup: MTLSize(width: 1, height: 1, depth: 1)); encoder.endEncoding(); command.commit(); command.waitUntilCompleted()
guard command.status == .completed && buffer.contents().load(as: UInt32.self) == 42 else { fatalError("Metal command failed") }
print(MPSSupportsMTLDevice(device) ? "metal mps" : "metal")"##;
        let mut command = Command::new("xcrun");
        command.args(["swift", "-e", script]);
        if let Ok(backends) = capture(command).await {
            gpu.extend(backends.split_whitespace().map(String::from));
        }
    }
    Ok(Capabilities {
        os,
        arch,
        abi,
        tools,
        gpu,
        cpu_features,
        runtimes,
        cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        memory_bytes,
        containerized,
    })
}

#[cfg(target_os = "linux")]
unsafe fn symbol<T: Copy>(library: *mut libc::c_void, name: &std::ffi::CStr) -> Option<T> {
    let pointer = unsafe { libc::dlsym(library, name.as_ptr()) };
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute_copy(&pointer) })
    }
}
#[cfg(target_os = "linux")]
fn cuda_accessible() -> bool {
    unsafe {
        let library = libc::dlopen(c"libcuda.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if library.is_null() {
            return false;
        }
        let result = (|| {
            let init: unsafe extern "C" fn(u32) -> i32 = symbol(library, c"cuInit")?;
            let count: unsafe extern "C" fn(*mut i32) -> i32 =
                symbol(library, c"cuDeviceGetCount")?;
            let create: unsafe extern "C" fn(*mut *mut libc::c_void, u32, i32) -> i32 =
                symbol(library, c"cuCtxCreate_v2")?;
            let allocate: unsafe extern "C" fn(*mut u64, usize) -> i32 =
                symbol(library, c"cuMemAlloc_v2")?;
            let free: unsafe extern "C" fn(u64) -> i32 = symbol(library, c"cuMemFree_v2")?;
            let destroy: unsafe extern "C" fn(*mut libc::c_void) -> i32 =
                symbol(library, c"cuCtxDestroy_v2")?;
            let mut devices = 0;
            if init(0) != 0 || count(&mut devices) != 0 || devices < 1 {
                return None;
            }
            let mut context = std::ptr::null_mut();
            if create(&mut context, 0, 0) != 0 {
                return None;
            }
            let mut memory = 0;
            let allocated = allocate(&mut memory, 4) == 0;
            if allocated {
                free(memory);
            }
            destroy(context);
            Some(allocated)
        })()
        .unwrap_or(false);
        libc::dlclose(library);
        result
    }
}
#[cfg(target_os = "linux")]
fn rocm_accessible() -> bool {
    unsafe {
        let library = libc::dlopen(
            c"libamdhip64.so".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if library.is_null() {
            return false;
        }
        let result = (|| {
            let init: unsafe extern "C" fn(u32) -> i32 = symbol(library, c"hipInit")?;
            let count: unsafe extern "C" fn(*mut i32) -> i32 =
                symbol(library, c"hipGetDeviceCount")?;
            let allocate: unsafe extern "C" fn(*mut *mut libc::c_void, usize) -> i32 =
                symbol(library, c"hipMalloc")?;
            let free: unsafe extern "C" fn(*mut libc::c_void) -> i32 = symbol(library, c"hipFree")?;
            let mut devices = 0;
            if init(0) != 0 || count(&mut devices) != 0 || devices < 1 {
                return None;
            }
            let mut memory = std::ptr::null_mut();
            let allocated = allocate(&mut memory, 4) == 0;
            if allocated {
                free(memory);
            }
            Some(allocated)
        })()
        .unwrap_or(false);
        libc::dlclose(library);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state_at(data: &Path) -> Arc<State> {
        for path in [
            data.to_path_buf(),
            data.join("attempts"),
            data.join("blobs"),
            data.join("environments"),
        ] {
            private_dir(&path).unwrap();
        }
        let (events, _) = broadcast::channel(100);
        Arc::new(State {
            hub: "http://127.0.0.1:1".into(),
            token: "test-token".into(),
            data: data.into(),
            id: "test".into(),
            client: reqwest::Client::new(),
            capabilities: Capabilities {
                os: std::env::consts::OS.into(),
                arch: normalize_arch(std::env::consts::ARCH),
                ..Default::default()
            },
            max_jobs: 1,
            slots: Arc::new(Semaphore::new(1)),
            drain: RwLock::new(()),
            attempts: Mutex::new(HashMap::new()),
            environments: Mutex::new(BTreeMap::new()),
            events,
        })
    }
    fn attempt_at(data: &Path, id: &str) -> Arc<Attempt> {
        let dir = data.join("attempts").join(id);
        private_dir(&dir).unwrap();
        let (cancel, _) = watch::channel(false);
        let journal = Journal {
            report: AttemptReport {
                job_id: "job".into(),
                attempt_id: id.into(),
                state: "assigned".into(),
                exit_code: None,
                error: None,
                results: vec![],
            },
            seq: 0,
            pid: None,
            container: None,
        };
        atomic_json(&dir.join("journal.json"), &journal).unwrap();
        Arc::new(Attempt {
            dir,
            journal: Mutex::new(journal),
            cancel,
        })
    }
    #[tokio::test]
    async fn restart_marks_unfinished_unknown_and_duplicate_does_not_run() {
        let temp = tempfile::tempdir().unwrap();
        let state = state_at(temp.path());
        let attempt = attempt_at(temp.path(), "attempt");
        attempt.log(&state, "stdout", "durable\n".into()).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(attempt.dir.join("logs.jsonl"))
            .unwrap();
        file.write_all(b"{partial").unwrap();
        drop(file);
        load_state(&state).await.unwrap();
        assert_eq!(
            state.attempts.lock().unwrap()["attempt"].report().state,
            "unknown"
        );
        let job = Job {
            id: "job".into(),
            attempt_id: "attempt".into(),
            state: "assigned".into(),
            spec: JobSpec {
                command: vec!["sh".into(), "-c".into(), "touch should-not-run".into()],
                ..Default::default()
            },
            worker_id: Some("test".into()),
            created_at: 0,
            updated_at: 0,
            exit_code: None,
            error: None,
            results: vec![],
        };
        accept(state.clone(), job, None).unwrap();
        assert!(!attempt.dir.join("work").exists());
        assert_eq!(
            state.attempts.lock().unwrap()["attempt"]
                .journal
                .lock()
                .unwrap()
                .seq,
            1
        );
        assert!(
            fs::read_to_string(attempt.dir.join("logs.jsonl"))
                .unwrap()
                .ends_with('\n')
        );
    }
    #[tokio::test]
    async fn writable_checkout_cannot_mutate_content_cache() {
        let temp = tempfile::tempdir().unwrap();
        let state = state_at(temp.path());
        let attempt = attempt_at(temp.path(), "attempt");
        let hash = hex::encode(Sha256::digest(b"original"));
        fs::write(temp.path().join("blobs").join(&hash), b"original").unwrap();
        let job = Job {
            id: "job".into(),
            attempt_id: "attempt".into(),
            state: "assigned".into(),
            spec: JobSpec {
                command: vec!["sh".into(), "-c".into(), "printf edited > input".into()],
                inputs: vec![FileEntry {
                    path: "input".into(),
                    hash: hash.clone(),
                    size: 8,
                    mode: 0o644,
                }],
                ..Default::default()
            },
            worker_id: None,
            created_at: 0,
            updated_at: 0,
            exit_code: None,
            error: None,
            results: vec![],
        };
        let (cancel, receiver) = watch::channel(false);
        let (exit, _) = run_attempt(&state, &attempt, &job, None, receiver)
            .await
            .unwrap();
        drop(cancel);
        assert_eq!(exit, 0);
        assert_eq!(fs::read(attempt.dir.join("work/input")).unwrap(), b"edited");
        assert_eq!(
            fs::read(temp.path().join("blobs").join(hash)).unwrap(),
            b"original"
        );
    }
    #[tokio::test]
    async fn cancellation_stops_descendants_and_preserves_logs() {
        let temp = tempfile::tempdir().unwrap();
        let state = state_at(temp.path());
        let attempt = attempt_at(temp.path(), "attempt");
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "printf before; printf error >&2; (sleep 1; touch leaked) & wait",
            ])
            .current_dir(temp.path());
        let (cancel, receiver) = watch::channel(false);
        let cancellation = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.send(true).unwrap();
        };
        let (result, _) = tokio::join!(
            execute(&state, &attempt, command, receiver, 10, None),
            cancellation
        );
        assert!(result.is_err());
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!temp.path().join("leaked").exists());
        let logs = fs::read_to_string(attempt.dir.join("logs.jsonl")).unwrap();
        assert!(logs.contains("before"));
        assert!(logs.contains("error"));
    }
    #[test]
    fn identifiers_cannot_escape() {
        for value in ["", ".", "..", "a/b", "a\\b", "-x", "a b"] {
            assert!(identifier(value).is_err());
        }
        assert!(identifier("worker-1_a").is_ok());
    }
    #[test]
    fn version_parsing() {
        assert_eq!(version_token("rustc 1.96.1 (hash)"), "1.96.1");
        assert_eq!(version_token("v22.19.0"), "22.19.0");
        assert_eq!(version_token("Python 3.13.7"), "3.13.7");
        assert_eq!(
            version_token("Apple clang version 17.0.0 (clang)"),
            "17.0.0"
        );
    }
    #[test]
    fn environment_expands_only_known_variables() {
        let mut environment = Environment::default();
        environment
            .env
            .insert("PATH".into(), "${YLCC_ENV_DIR}/custom:${PATH}".into());
        environment
            .env
            .insert("OTHER".into(), "$(echo bad):${UNKNOWN}".into());
        let values = expanded_env(Some(&environment), None, Path::new("/tmp/tools"), false);
        assert!(values["PATH"].starts_with("/tmp/tools/custom:/tmp/tools/bin:"));
        assert_eq!(values["OTHER"], "$(echo bad):${UNKNOWN}");
    }
    #[test]
    fn outputs_reject_symlinks_and_include_mode_hash() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("out")).unwrap();
        fs::write(temp.path().join("out/a"), b"hello").unwrap();
        let mut files = BTreeMap::new();
        collect_output(temp.path(), &temp.path().join("out"), &mut files).unwrap();
        assert_eq!(files["out/a"].size, 5);
        assert_eq!(files["out/a"].hash, hex::encode(Sha256::digest(b"hello")));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", temp.path().join("out/escape")).unwrap();
            assert!(collect_output(temp.path(), &temp.path().join("out"), &mut files).is_err());
        }
    }
    #[test]
    fn corrupt_cache_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("blob");
        fs::write(&path, b"changed").unwrap();
        assert!(verify_file(&path, &hex::encode(Sha256::digest(b"original")), None).is_err());
    }
}
