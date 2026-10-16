use crate::{model::*, snapshot};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub version: u32,
    pub tasks: BTreeMap<String, Task>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub command: Vec<String>,
    #[serde(default)]
    pub inputs: Vec<PathBuf>,
    #[serde(default)]
    pub project: bool,
    #[serde(default)]
    pub no_input: bool,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default, deserialize_with = "strict_requirements")]
    pub requirements: Requirements,
    pub environment: Option<String>,
    pub container: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub priority: i32,
}

fn strict_requirements<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Requirements, D::Error> {
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    if let Some(fields) = value.as_object() {
        for field in fields.keys() {
            if ![
                "os",
                "arch",
                "abi",
                "tools",
                "gpu",
                "cpu_features",
                "build_target",
                "emulation",
            ]
            .contains(&field.as_str())
            {
                return Err(D::Error::custom(format!(
                    "unknown requirements field {field}"
                )));
            }
        }
    }
    serde_json::from_value(value).map_err(D::Error::custom)
}

impl Project {
    pub fn load(file: &Path) -> Result<(Self, PathBuf)> {
        let file = file
            .canonicalize()
            .with_context(|| format!("project file {}", file.display()))?;
        let project = Self::parse(&fs::read_to_string(&file)?)?;
        let root = file
            .parent()
            .context("project file has no parent")?
            .to_path_buf();
        Ok((project, root))
    }

    pub fn parse(yaml: &str) -> Result<Self> {
        let project: Self = yaml_serde::from_str(yaml).context("invalid YLCC project YAML")?;
        if project.version != 1 {
            bail!(
                "unsupported YLCC project version {}; expected 1",
                project.version
            )
        }
        for (name, task) in &project.tasks {
            if name.trim().is_empty() {
                bail!("task name must not be empty")
            }
            task.validate().with_context(|| format!("task {name}"))?;
        }
        Ok(project)
    }
}

impl Task {
    fn validate(&self) -> Result<()> {
        if self.no_input && (self.project || !self.inputs.is_empty()) {
            bail!("no_input cannot be combined with inputs or project")
        }
        if !self.no_input && !self.project && self.inputs.is_empty() {
            bail!("select inputs, project: true, or no_input: true")
        }
        for input in &self.inputs {
            let input = input.to_str().context("input path is not UTF-8")?;
            if input != "." {
                safe_relative(input)?;
            }
        }
        self.spec(Vec::new()).validate()
    }

    pub fn snapshot(&self, root: &Path) -> Result<snapshot::Snapshot> {
        self.validate()?;
        if self.no_input {
            return Ok(snapshot::Snapshot {
                files: Vec::new(),
                excluded: Vec::new(),
                blobs: tempfile::tempdir()?,
            });
        }
        snapshot::create(root, &self.inputs, self.project)
    }

    pub fn spec(&self, inputs: Vec<FileEntry>) -> JobSpec {
        JobSpec {
            command: self.command.clone(),
            inputs,
            outputs: self.outputs.clone(),
            requirements: self.requirements.clone(),
            environment: self.environment.clone(),
            container: self.container.clone(),
            env: self.env.clone(),
            timeout_secs: self.timeout_secs,
            priority: self.priority,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_task(fields: &str) -> Result<Project> {
        Project::parse(&format!(
            "version: 1\ntasks:\n  build:\n    command: [cargo, build, --locked]\n{fields}"
        ))
    }

    #[test]
    fn parses_named_tasks_without_changing_their_requirements() {
        let project = parse_task("    inputs: [Cargo.toml, src]\n    outputs: [target/app]\n    requirements: {os: linux, arch: x86_64, tools: {cargo: '*'}}\n    env: {RUST_BACKTRACE: '1'}\n    priority: 3\n    timeout_secs: 1800\n").unwrap();
        let spec = project.tasks["build"].spec(Vec::new());
        assert_eq!(spec.command, ["cargo", "build", "--locked"]);
        assert_eq!(spec.requirements.os.as_deref(), Some("linux"));
        assert_eq!(spec.requirements.tools["cargo"], "*");
        assert_eq!(spec.env["RUST_BACKTRACE"], "1");
        assert_eq!(spec.priority, 3);
        assert_eq!(spec.timeout_secs, Some(1800));
    }

    #[test]
    fn rejects_unknown_fields_versions_and_unsafe_or_conflicting_selections() {
        for fields in [
            "    no_input: true\n    cpu: '50%'\n",
            "    no_input: true\n    requirements: {memory: '50%'}\n",
            "    no_input: true\n    inputs: [src]\n",
            "    no_input: true\n    project: true\n",
            "    inputs: [../secret]\n",
            "    inputs: [/etc/passwd]\n",
            "    no_input: true\n    priority: 101\n",
            "    no_input: true\n    priority: -101\n",
            "    no_input: true\n    outputs: [../escape]\n",
            "",
        ] {
            assert!(parse_task(fields).is_err(), "accepted {fields:?}");
        }
        assert!(Project::parse("version: 2\ntasks: {}\n").is_err());
        assert!(Project::parse("version: 1\nhub: https://example.test\ntasks: {}\n").is_err());
        assert!(
            Project::parse("version: 1\ntasks: {bad: {command: [], no_input: true}}\n").is_err()
        );
        assert!(
            Project::parse("version: 1\ntasks: {bad: {command: [''], no_input: true}}\n").is_err()
        );
    }

    #[test]
    fn snapshots_relative_to_the_project_file_parent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source.txt"), "project input").unwrap();
        fs::write(
            root.join("ylcc.yaml"),
            "version: 1\ntasks: {check: {command: [cat, source.txt], inputs: [source.txt]}}\n",
        )
        .unwrap();
        let (project, resolved_root) = Project::load(&root.join("ylcc.yaml")).unwrap();
        assert_eq!(resolved_root, root.canonicalize().unwrap());
        let snapshot = project.tasks["check"].snapshot(&resolved_root).unwrap();
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].path, "source.txt");
        assert_eq!(
            fs::read_to_string(snapshot.blobs.path().join(&snapshot.files[0].hash)).unwrap(),
            "project input"
        );
    }

    #[test]
    fn empty_input_is_only_allowed_when_explicit() {
        let project = parse_task("    no_input: true\n").unwrap();
        let snap = project.tasks["build"]
            .snapshot(Path::new("unused"))
            .unwrap();
        assert!(snap.files.is_empty());
        assert!(parse_task("    project: true\n    inputs: [ignored-file]\n").is_ok());
    }
}
