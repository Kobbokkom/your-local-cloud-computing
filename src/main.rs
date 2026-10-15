use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};
use ylcc::{client::Client, model::*, snapshot};

#[derive(Parser)]
#[command(
    version,
    about = "Run workloads across your own Linux and macOS machines"
)]
struct Cli {
    #[arg(long, global = true, env = "YLCC_HUB_URL")]
    hub: Option<String>,
    #[arg(long, global = true, env = "YLCC_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    #[arg(long, global = true)]
    results_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
// CLI arguments are parsed once; keeping the argument structures direct simplifies dispatch.
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Generate a protected pool token (never prints its contents).
    Init,
    /// Save requester defaults, including token file reference.
    Configure,
    Hub {
        #[arg(long, default_value = "127.0.0.1:7443")]
        listen: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    Worker {
        #[arg(long)]
        id: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        max_jobs: usize,
    },
    /// Show the input snapshot manifest without submitting a job.
    Manifest(Selection),
    Run(RunArgs),
    Workers,
    Jobs {
        #[command(subcommand)]
        command: Jobs,
    },
    Env {
        #[command(subcommand)]
        command: Envs,
    },
    Doctor,
    Service {
        #[command(subcommand)]
        command: Services,
    },
}
#[derive(Args)]
struct Selection {
    #[arg(long)]
    input: Vec<PathBuf>,
    #[arg(long)]
    project: bool,
    #[arg(long,conflicts_with_all=["input","project"])]
    no_input: bool,
}
impl Selection {
    fn snapshot(&self) -> Result<snapshot::Snapshot> {
        if self.no_input {
            return Ok(snapshot::Snapshot {
                files: vec![],
                excluded: vec![],
                blobs: tempfile::tempdir()?,
            });
        }
        snapshot::create(&std::env::current_dir()?, &self.input, self.project)
    }
}
#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    selection: Selection,
    #[arg(long)]
    output: Vec<String>,
    #[arg(long)]
    os: Option<String>,
    #[arg(long)]
    arch: Option<String>,
    #[arg(long)]
    abi: Option<String>,
    #[arg(long)]
    gpu: Option<String>,
    #[arg(
        long,
        value_name = "NAME=VERSION",
        help = "Require an exact tool version, or NAME=* for any detected version"
    )]
    tool: Vec<String>,
    #[arg(long)]
    cpu_feature: Vec<String>,
    #[arg(long)]
    build_target: Option<String>,
    #[arg(long)]
    environment: Option<String>,
    #[arg(long)]
    container: Option<String>,
    #[arg(long)]
    timeout: Option<u64>,
    #[arg(long)]
    env: Vec<String>,
    #[arg(long)]
    detach: bool,
    #[arg(long)]
    no_download: bool,
    #[arg(last = true, required = true)]
    command: Vec<String>,
}
#[derive(Subcommand)]
enum Jobs {
    List,
    Show {
        id: String,
    },
    Logs {
        id: String,
    },
    Wait {
        id: String,
        #[arg(long)]
        no_download: bool,
    },
    Fetch {
        id: String,
    },
    Cancel {
        id: String,
    },
}
#[derive(Subcommand)]
enum Envs {
    List,
    Add {
        file: PathBuf,
    },
    Prepare {
        id: String,
        #[arg(long)]
        worker: Option<String>,
    },
    Logs {
        id: String,
    },
    Status,
}
#[derive(Subcommand)]
enum Services {
    Install {
        #[arg(long)]
        role: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long, default_value = "worker")]
        id: String,
        #[arg(long, default_value = "127.0.0.1:7443")]
        listen: String,
        #[arg(long, default_value_t = 1)]
        max_jobs: usize,
        #[arg(long)]
        system_user: Option<String>,
    },
    Uninstall {
        #[arg(long)]
        role: String,
        #[arg(long)]
        system_user: Option<String>,
    },
}
#[derive(Default, Serialize, Deserialize)]
struct Config {
    hub: Option<String>,
    token_file: Option<PathBuf>,
    results_dir: Option<PathBuf>,
}
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME must be set")
}
fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("ylcc/config.json")
}
fn data_path() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("ylcc")
}
fn pairs(values: Vec<String>) -> Result<BTreeMap<String, String>> {
    values
        .into_iter()
        .map(|s| {
            let (k, v) = s.split_once('=').context("expected NAME=VALUE")?;
            if k.is_empty() {
                bail!("empty name")
            };
            Ok((k.to_owned(), v.to_owned()))
        })
        .collect()
}
fn print<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = execute().await {
        eprintln!("{e:#}");
        std::process::exit(1)
    }
}
async fn execute() -> Result<()> {
    let cli = Cli::parse();
    let cfg_path = config_path();
    let cfg: Config = if cfg_path.exists() {
        serde_json::from_slice(&fs::read(&cfg_path)?)?
    } else {
        Config::default()
    };
    let hub = cli
        .hub
        .or(cfg.hub)
        .unwrap_or_else(|| "http://127.0.0.1:7443".into());
    let token = cli
        .token_file
        .or(cfg.token_file)
        .unwrap_or_else(|| cfg_path.parent().unwrap().join("token"));
    let results = cli
        .results_dir
        .or(cfg.results_dir)
        .unwrap_or_else(|| data_path().join("results"));
    match cli.command {
        Commands::Init => {
            let parent = token
                .parent()
                .context("token file needs a parent directory")?;
            fs::create_dir_all(parent)?;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&token)
                .context(
                    "token file exists or cannot be created; existing tokens are never replaced",
                )?;
            writeln!(
                f,
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )?;
            print(&serde_json::json!({"token_file":token}))
        }
        Commands::Configure => {
            ylcc::client::validate_hub(&hub)?;
            ylcc::client::read_token(&token)?;
            fs::create_dir_all(cfg_path.parent().unwrap())?;
            let mut temp = tempfile::NamedTempFile::new_in(cfg_path.parent().unwrap())?;
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
            temp.write_all(&serde_json::to_vec_pretty(&Config {
                hub: Some(hub),
                token_file: Some(token),
                results_dir: Some(results),
            })?)?;
            temp.persist(&cfg_path)?;
            print(&serde_json::json!({"config_file":cfg_path}))
        }
        Commands::Hub { listen, data_dir } => {
            ylcc::hub::run(
                listen,
                data_dir.unwrap_or_else(|| data_path().join("hub")),
                token,
            )
            .await
        }
        Commands::Worker {
            id,
            data_dir,
            max_jobs,
        } => {
            ylcc::worker::run(
                hub,
                token,
                data_dir.unwrap_or_else(|| data_path().join("worker")),
                id,
                max_jobs,
            )
            .await
        }
        Commands::Manifest(s) => {
            let snap = s.snapshot()?;
            print(&serde_json::json!({"files":snap.files,"excluded":snap.excluded}))
        }
        Commands::Run(r) => {
            let client = Client::new(&hub, &token)?;
            let snap = r.selection.snapshot()?;
            let spec = JobSpec {
                command: r.command,
                inputs: snap.files.clone(),
                outputs: r.output,
                requirements: Requirements {
                    os: r.os,
                    arch: r.arch.map(|v| normalize_arch(&v)),
                    abi: r.abi,
                    tools: pairs(r.tool)?,
                    gpu: r.gpu,
                    cpu_features: r.cpu_feature,
                    build_target: r.build_target,
                    emulation: false,
                },
                environment: r.environment,
                container: r.container,
                env: pairs(r.env)?,
                timeout_secs: r.timeout,
            };
            spec.validate()?;
            let (count, bytes) = client.upload(&snap).await?;
            eprintln!(
                "uploaded {count} missing blobs ({bytes} bytes); {} selected files",
                snap.files.len()
            );
            let job: Job = client.post("/v1/jobs", &spec).await?;
            if r.detach {
                print(&job)
            } else {
                eprintln!("job {} ({})", job.id, job.state);
                let j = client.wait(&job.id, &results, !r.no_download).await?;
                print(&j)?;
                if j.state != "succeeded" {
                    bail!("job {} ended {}", j.id, j.state)
                }
                Ok(())
            }
        }
        Commands::Workers => print(
            &Client::new(&hub, &token)?
                .get::<Vec<WorkerInfo>>("/v1/workers")
                .await?,
        ),
        Commands::Jobs { command } => {
            let c = Client::new(&hub, &token)?;
            match command {
                Jobs::List => print(&c.get::<Vec<Job>>("/v1/jobs").await?),
                Jobs::Show { id } => print(&c.job(&id).await?),
                Jobs::Logs { id } => {
                    ylcc::client::segment(&id)?;
                    print(
                        &c.get::<Vec<LogEntry>>(&format!("/v1/jobs/{id}/logs"))
                            .await?,
                    )
                }
                Jobs::Wait { id, no_download } => {
                    let j = c.wait(&id, &results, !no_download).await?;
                    print(&j)?;
                    if j.state != "succeeded" {
                        bail!("job {} ended {}", j.id, j.state)
                    }
                    Ok(())
                }
                Jobs::Fetch { id } => {
                    let j = c.job(&id).await?;
                    print(&serde_json::json!({"results_dir":c.fetch(&j,&results).await?}))
                }
                Jobs::Cancel { id } => {
                    ylcc::client::segment(&id)?;
                    print(
                        &c.post::<_, Job>(&format!("/v1/jobs/{id}/cancel"), &serde_json::json!({}))
                            .await?,
                    )
                }
            }
        }
        Commands::Env { command } => {
            let c = Client::new(&hub, &token)?;
            match command {
                Envs::List => print(&c.get::<Vec<Environment>>("/v1/environments").await?),
                Envs::Status => print(
                    &c.get::<Vec<EnvironmentStatus>>("/v1/environments/status")
                        .await?,
                ),
                Envs::Add { file } => {
                    let e: Environment = serde_json::from_slice(&fs::read(file)?)?;
                    print(&c.post::<_, Environment>("/v1/environments", &e).await?)
                }
                Envs::Logs { id } => {
                    ylcc::client::segment(&id)?;
                    print(
                        &c.get::<Vec<LogEntry>>(&format!("/v1/environments/{id}/logs"))
                            .await?,
                    )
                }
                Envs::Prepare { id, worker } => {
                    ylcc::client::segment(&id)?;
                    print(
                        &c.post::<_, Vec<EnvironmentStatus>>(
                            &format!("/v1/environments/{id}/prepare"),
                            &PrepareRequest { worker_id: worker },
                        )
                        .await?,
                    )
                }
            }
        }
        Commands::Doctor => {
            let mut v = serde_json::json!({"local":ylcc::worker::probe().await?});
            match Client::new(&hub, &token) {
                Ok(c) => {
                    v["workers"] =
                        serde_json::to_value(c.get::<Vec<WorkerInfo>>("/v1/workers").await?)?;
                }
                Err(e) => {
                    v["hub_error"] = serde_json::Value::String(e.to_string());
                }
            }
            print(&v)
        }
        Commands::Service { command } => match command {
            Services::Install {
                role,
                data_dir,
                id,
                listen,
                max_jobs,
                system_user,
            } => {
                let dir = data_dir.unwrap_or_else(|| data_path().join(&role));
                if let Some(user) = system_user {
                    ylcc::service::install_system_user(
                        &role, &hub, &token, &dir, &id, &listen, max_jobs, &user,
                    )
                } else {
                    ylcc::service::install(&role, &hub, &token, &dir, &id, &listen, max_jobs)
                }
            }
            Services::Uninstall { role, system_user } => {
                if let Some(user) = system_user {
                    ylcc::service::uninstall_system_user(&role, &user)
                } else {
                    ylcc::service::uninstall(&role)
                }
            }
        },
    }
}
