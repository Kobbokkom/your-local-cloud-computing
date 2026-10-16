use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn normalize_arch(s: &str) -> String {
    match s {
        "amd64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        _ => s,
    }
    .into()
}
pub fn safe_relative(s: &str) -> Result<()> {
    let normalized = Path::new(s)
        .components()
        .filter_map(|c| {
            if let Component::Normal(p) = c {
                p.to_str()
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    if s.is_empty()
        || s.contains(['\\', '\0'])
        || normalized != s
        || !Path::new(s)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        bail!("unsafe relative path: {s}")
    }
    Ok(())
}
pub fn validate_hash(s: &str) -> Result<()> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("invalid SHA-256")
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Requirements {
    pub os: Option<String>,
    pub arch: Option<String>,
    pub abi: Option<String>,
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    pub gpu: Option<String>,
    #[serde(default)]
    pub cpu_features: Vec<String>,
    pub build_target: Option<String>,
    #[serde(default)]
    pub emulation: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub os: String,
    pub arch: String,
    pub abi: String,
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    #[serde(default)]
    pub gpu: Vec<String>,
    #[serde(default)]
    pub cpu_features: Vec<String>,
    #[serde(default)]
    pub runtimes: Vec<String>,
    pub cpus: usize,
    pub memory_bytes: u64,
    #[serde(default)]
    pub containerized: bool,
}
impl Capabilities {
    pub fn mismatch(&self, r: &Requirements) -> Vec<String> {
        let mut reasons = Vec::new();
        if r.os.as_ref().is_some_and(|v| v != &self.os) {
            reasons.push(format!("OS is {}", self.os));
        }
        if r.arch
            .as_ref()
            .is_some_and(|v| normalize_arch(v) != normalize_arch(&self.arch))
        {
            reasons.push(format!(
                "architecture is {} (emulation is never implicit)",
                self.arch
            ));
        }
        if r.abi.as_ref().is_some_and(|v| v != &self.abi) {
            reasons.push(format!("ABI is {}", self.abi));
        }
        for (tool, version) in &r.tools {
            if !self
                .tools
                .get(tool)
                .is_some_and(|v| version == "*" || v == version)
            {
                reasons.push(format!("missing tool {tool} version {version}"));
            }
        }
        if r.gpu.as_ref().is_some_and(|v| !self.gpu.contains(v)) {
            reasons.push("GPU backend unavailable".into());
        }
        for f in &r.cpu_features {
            if !self.cpu_features.contains(f) {
                reasons.push(format!("missing CPU feature {f}"));
            }
        }
        reasons
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub mode: u32,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobSpec {
    pub command: Vec<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub inputs: Vec<FileEntry>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub requirements: Requirements,
    pub environment: Option<String>,
    pub container: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub timeout_secs: Option<u64>,
}
impl JobSpec {
    pub fn validate(&self) -> Result<()> {
        validate_priority(self.priority)?;
        if self.command.is_empty() || self.command[0].is_empty() {
            bail!("command is required")
        }
        let mut paths = std::collections::BTreeSet::new();
        for f in &self.inputs {
            safe_relative(&f.path)?;
            validate_hash(&f.hash)?;
            if !paths.insert(&f.path) {
                bail!("duplicate input path")
            }
        }
        for p in &self.outputs {
            safe_relative(p)?;
        }
        for key in self.env.keys() {
            if key.contains('=') || key.contains('\0') || key.starts_with("YLCC_") {
                bail!("invalid or reserved environment variable")
            }
        }
        if self.requirements.emulation {
            bail!("emulation is not implemented; use a native matching worker")
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub attempt_id: String,
    pub state: String,
    pub spec: JobSpec,
    pub worker_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    #[serde(default)]
    pub results: Vec<FileEntry>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInfo {
    pub id: String,
    pub capabilities: Capabilities,
    pub max_jobs: usize,
    #[serde(default)]
    pub paused: bool,
    pub online: bool,
    pub last_seen: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerControl {
    pub paused: Option<bool>,
    pub max_jobs: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriorityRequest {
    pub priority: i32,
}
pub fn validate_priority(priority: i32) -> Result<()> {
    if !(-100..=100).contains(&priority) {
        bail!("priority must be between -100 and 100")
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub job_id: String,
    pub attempt_id: String,
    pub seq: u64,
    pub stream: String,
    pub text: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub revision: String,
    #[serde(default)]
    pub requirements: Requirements,
    #[serde(default)]
    pub setup: Vec<String>,
    #[serde(default)]
    pub verify: Vec<String>,
    pub image: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentStatus {
    pub worker_id: String,
    pub environment_id: String,
    pub revision: String,
    pub state: String,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptReport {
    pub job_id: String,
    pub attempt_id: String,
    pub state: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    #[serde(default)]
    pub results: Vec<FileEntry>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerMessage {
    Hello {
        worker_id: String,
        capabilities: Capabilities,
        max_jobs: usize,
        attempts: Vec<AttemptReport>,
        environments: Vec<EnvironmentStatus>,
    },
    Started {
        job_id: String,
        attempt_id: String,
    },
    Log {
        entry: LogEntry,
    },
    Finished {
        report: AttemptReport,
    },
    Environment {
        status: EnvironmentStatus,
    },
    Heartbeat,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubMessage {
    Run {
        job: Box<Job>,
        environment: Option<Environment>,
    },
    Cancel {
        job_id: String,
        attempt_id: String,
    },
    Prepare {
        environment: Environment,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingBlobs {
    pub hashes: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrepareRequest {
    pub worker_id: Option<String>,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_paths() {
        for s in ["../x", "/tmp/x", "a/../b", "a\\b", ""] {
            assert!(safe_relative(s).is_err());
        }
        assert!(safe_relative("dir/a.txt").is_ok());
    }
    #[test]
    fn architecture_and_gpu_are_required() {
        let c = Capabilities {
            os: "linux".into(),
            arch: "x86_64".into(),
            ..Default::default()
        };
        assert!(
            c.mismatch(&Requirements {
                arch: Some("amd64".into()),
                ..Default::default()
            })
            .is_empty()
        );
        assert!(
            !c.mismatch(&Requirements {
                gpu: Some("cuda".into()),
                ..Default::default()
            })
            .is_empty()
        );
    }
    #[test]
    fn priorities_are_bounded_and_old_records_default_to_normal() {
        let mut spec: JobSpec = serde_json::from_value(serde_json::json!({
            "command":["true"],
            "environment":null,
            "container":null,
            "timeout_secs":null
        }))
        .unwrap();
        assert_eq!(spec.priority, 0);
        for priority in [-100, 0, 100] {
            spec.priority = priority;
            assert!(spec.validate().is_ok());
        }
        for priority in [-101, 101] {
            spec.priority = priority;
            assert!(spec.validate().is_err());
        }
        let worker: WorkerInfo = serde_json::from_value(serde_json::json!({
            "id":"old-worker",
            "capabilities":Capabilities::default(),
            "max_jobs":1,
            "online":false,
            "last_seen":0
        }))
        .unwrap();
        assert!(!worker.paused);
    }
}
