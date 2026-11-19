use super::*;
use tokio::process::Command;
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Deployment {
    Compose {
        project: String,
        files: Vec<PathBuf>,
        env_file: PathBuf,
        source_service: String,
        canary_service: String,
        rollback_service: String,
        source_volume: String,
        canary_volume: String,
        rollback_volume: String,
    },
    Helm {
        context: String,
        namespace: String,
        release: String,
        chart: PathBuf,
        values: PathBuf,
        canary_statefulset: String,
        rollback_statefulset: String,
        source_pvc: String,
        canary_pvc: String,
        rollback_pvc: String,
    },
}
impl Deployment {
    pub(super) fn validate(&self, root: &Path, run: &str) -> Result<()> {
        if std::fs::canonicalize(root).ok().as_deref() != Some(root) {
            return fail("repository must be its canonical existing absolute path");
        }
        match self {
            Self::Compose {
                project,
                files,
                env_file,
                source_service,
                canary_service,
                rollback_service,
                source_volume,
                canary_volume,
                rollback_volume,
            } => {
                if ![
                    project,
                    source_service,
                    canary_service,
                    rollback_service,
                    source_volume,
                    canary_volume,
                    rollback_volume,
                ]
                .iter()
                .all(|s| component(s))
                    || files.is_empty()
                    || files.len() > 8
                    || !files
                        .iter()
                        .all(|p| p.is_absolute() && p.starts_with(root) && p.is_file())
                    || !env_file.is_absolute()
                    || !env_file.is_file()
                    || source_service == canary_service
                    || source_service == rollback_service
                    || canary_service == rollback_service
                    || source_volume == canary_volume
                    || source_volume == rollback_volume
                    || canary_volume == rollback_volume
                    || !canary_volume.contains(run)
                    || !rollback_volume.contains(run)
                {
                    return fail("Compose upgrade ownership/volume contract invalid");
                }
            }
            Self::Helm {
                context,
                namespace,
                release,
                chart,
                values,
                canary_statefulset,
                rollback_statefulset,
                source_pvc,
                canary_pvc,
                rollback_pvc,
            } => {
                if ![
                    context,
                    namespace,
                    release,
                    canary_statefulset,
                    rollback_statefulset,
                    source_pvc,
                    canary_pvc,
                    rollback_pvc,
                ]
                .iter()
                .all(|s| component(s))
                    || !chart.is_absolute()
                    || !chart.starts_with(root)
                    || !chart.join("Chart.yaml").is_file()
                    || !values.is_absolute()
                    || !values.is_file()
                    || source_pvc == canary_pvc
                    || source_pvc == rollback_pvc
                    || canary_pvc == rollback_pvc
                    || !canary_pvc.contains(run)
                    || !rollback_pvc.contains(run)
                {
                    return fail("Helm upgrade ownership/PVC contract invalid");
                }
            }
        }
        Ok(())
    }
}
impl Deployment {
    pub(super) fn fingerprints(&self) -> Result<BTreeMap<String, String>> {
        let mut files = Vec::new();
        fn chart_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
            if files.len() > 256 {
                return fail("chart input inventory too large");
            }
            let metadata = std::fs::symlink_metadata(root)?;
            if metadata.file_type().is_symlink() {
                return fail("chart symlink inputs are refused");
            }
            if metadata.is_dir() {
                for entry in std::fs::read_dir(root)? {
                    chart_files(&entry?.path(), files)?;
                }
            } else if metadata.is_file() {
                files.push(root.to_path_buf());
            } else {
                return fail("chart input type invalid");
            }
            Ok(())
        }
        match self {
            Self::Compose {
                files: inputs,
                env_file,
                ..
            } => {
                files.extend(inputs.iter().cloned());
                files.push(env_file.clone());
            }
            Self::Helm { chart, values, .. } => {
                chart_files(chart, &mut files)?;
                files.push(values.clone());
            }
        }
        let mut hashes = BTreeMap::new();
        for file in files {
            hashes.insert(
                file.to_string_lossy().into_owned(),
                sha(&read_file(&file, 2 * 1024 * 1024, false)?),
            );
        }
        Ok(hashes)
    }
}

async fn output(program: &str, args: &[String]) -> Result<Vec<u8>> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| error("upgrade ownership command timed out"))?
    .map_err(|_| error("upgrade ownership tool unavailable"))?;
    if !output.status.success() || output.stdout.len() > 4 * 1024 * 1024 {
        return fail("upgrade ownership command failed");
    }
    Ok(output.stdout)
}
async fn container(project: &str, service: &str, root: &Path, volume: &str) -> Result<()> {
    let raw = output(
        "docker",
        &[
            "ps".into(),
            "-q".into(),
            "--filter".into(),
            format!("label=com.docker.compose.project={project}"),
            "--filter".into(),
            format!("label=com.docker.compose.service={service}"),
        ],
    )
    .await?;
    let ids = std::str::from_utf8(&raw)?
        .split_whitespace()
        .collect::<Vec<_>>();
    if ids.len() != 1 {
        return fail("expected one existing owned Qdrant container");
    }
    let labels: Value = serde_json::from_slice(
        &output(
            "docker",
            &[
                "inspect".into(),
                "--format".into(),
                "{{json .Config.Labels}}".into(),
                ids[0].into(),
            ],
        )
        .await?,
    )?;
    if labels["com.docker.compose.project"] != project
        || labels["com.docker.compose.service"] != service
        || labels["com.docker.compose.project.working_dir"] != root.to_string_lossy().as_ref()
    {
        return fail("Qdrant container belongs to another checkout/project");
    }
    let mounts: Value = serde_json::from_slice(
        &output(
            "docker",
            &[
                "inspect".into(),
                "--format".into(),
                "{{json .Mounts}}".into(),
                ids[0].into(),
            ],
        )
        .await?,
    )?;
    if !mounts.as_array().is_some_and(|m| {
        m.iter()
            .filter(|m| m["Destination"] == "/qdrant/storage")
            .count()
            == 1
            && m.iter().any(|m| {
                m["Destination"] == "/qdrant/storage"
                    && m["Type"] == "volume"
                    && m["Name"] == volume
            })
    }) {
        return fail("Qdrant storage volume identity mismatch");
    }
    Ok(())
}
async fn kube(
    context: &str,
    namespace: &str,
    name: &str,
    release: &str,
    pvc: &str,
    run: &str,
) -> Result<()> {
    let value: Value = serde_json::from_slice(
        &output(
            "kubectl",
            &[
                "--context".into(),
                context.into(),
                "--namespace".into(),
                namespace.into(),
                "get".into(),
                "statefulset".into(),
                name.into(),
                "-o".into(),
                "json".into(),
            ],
        )
        .await?,
    )?;
    if value["metadata"]["labels"]["app.kubernetes.io/instance"] != release
        || value["metadata"]["labels"]["agentforge.io/qdrant-upgrade-run"] != run
        || !value["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .is_some_and(|v| {
                v.iter().any(|v| {
                    v["name"] == "storage" && v["persistentVolumeClaim"]["claimName"] == pvc
                })
            })
        || !value["spec"]["template"]["spec"]["containers"]
            .as_array()
            .is_some_and(|v| {
                v.len() == 1
                    && v[0]["name"] == "qdrant"
                    && v[0]["volumeMounts"].as_array().is_some_and(|v| {
                        v.iter()
                            .any(|v| v["name"] == "storage" && v["mountPath"] == "/qdrant/storage")
                    })
            })
    {
        return fail("Kubernetes rehearsal release/run/PVC identity mismatch");
    }
    Ok(())
}
async fn distinct_claims(context: &str, namespace: &str, claims: [&String; 3]) -> Result<()> {
    let mut volumes = BTreeSet::new();
    for name in claims {
        let claim: Value = serde_json::from_slice(
            &output(
                "kubectl",
                &[
                    "--context".into(),
                    context.into(),
                    "--namespace".into(),
                    namespace.into(),
                    "get".into(),
                    "pvc".into(),
                    name.clone(),
                    "-o".into(),
                    "json".into(),
                ],
            )
            .await?,
        )?;
        let volume = claim["spec"]["volumeName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Qdrant claim is not bound to a volume"))?;
        if claim["metadata"]["name"] != name.as_str()
            || claim["metadata"]["namespace"] != namespace
            || claim["status"]["phase"] != "Bound"
            || !volumes.insert(volume.to_owned())
        {
            return fail("source/canary/rollback claims must have distinct bound storage");
        }
    }
    Ok(())
}

pub(super) async fn assert_isolated(spec: &Spec, rollback: bool) -> Result<()> {
    match &spec.deployment {
        Deployment::Compose {
            project,
            source_service,
            canary_service,
            rollback_service,
            source_volume,
            canary_volume,
            rollback_volume,
            ..
        } => {
            container(project, source_service, &spec.repository, source_volume).await?;
            container(
                project,
                if rollback {
                    rollback_service
                } else {
                    canary_service
                },
                &spec.repository,
                if rollback {
                    rollback_volume
                } else {
                    canary_volume
                },
            )
            .await
        }
        Deployment::Helm {
            context,
            namespace,
            release,
            canary_statefulset,
            rollback_statefulset,
            canary_pvc,
            rollback_pvc,
            source_pvc,
            ..
        } => {
            distinct_claims(context, namespace, [source_pvc, canary_pvc, rollback_pvc]).await?;
            kube(
                context,
                namespace,
                if rollback {
                    rollback_statefulset
                } else {
                    canary_statefulset
                },
                release,
                if rollback { rollback_pvc } else { canary_pvc },
                &spec.run_id,
            )
            .await
        }
    }
}
pub(super) async fn upgrade(spec: &Spec) -> Result<()> {
    assert_isolated(spec, false).await?;
    match &spec.deployment {
        Deployment::Compose {
            project,
            files,
            env_file,
            canary_service,
            canary_volume,
            rollback_volume,
            ..
        } => {
            let digests: Value = serde_json::from_slice(
                &output(
                    "docker",
                    &[
                        "image".into(),
                        "inspect".into(),
                        "--format".into(),
                        "{{json .RepoDigests}}".into(),
                        NEW_IMAGE.into(),
                    ],
                )
                .await?,
            )?;
            if !digests
                .as_array()
                .is_some_and(|v| v.iter().any(|v| v == NEW_IMAGE))
            {
                return fail(
                    "reviewed candidate image must be preloaded by execution owner; no implicit pull",
                );
            }
            let mut cmd = Command::new("docker");
            cmd.arg("compose")
                .arg("--project-directory")
                .arg(&spec.repository)
                .arg("--project-name")
                .arg(project)
                .arg("--env-file")
                .arg(env_file);
            for file in files {
                cmd.arg("--file").arg(file);
            }
            cmd.args([
                "up",
                "-d",
                "--no-deps",
                "--no-build",
                "--pull",
                "never",
                "--wait",
            ])
            .arg(canary_service)
            .env("QDRANT_UPGRADE_CANARY_IMAGE", NEW_IMAGE)
            .env("QDRANT_UPGRADE_RUN_ID", &spec.run_id)
            .env("QDRANT_UPGRADE_CANARY_VOLUME", canary_volume)
            .env("QDRANT_UPGRADE_ROLLBACK_VOLUME", rollback_volume)
            .env("COMPOSE_PARALLEL_LIMIT", "1")
            .kill_on_drop(true);
            let status = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                cmd.stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
            )
            .await
            .map_err(|_| error("Qdrant canary upgrade timed out"))??;
            if !status.success() {
                return fail("Qdrant canary upgrade failed; source remains untouched");
            }
        }
        Deployment::Helm {
            context,
            namespace,
            release,
            chart,
            values,
            canary_pvc,
            rollback_pvc,
            ..
        } => {
            let status = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                Command::new("helm")
                    .args([
                        "upgrade",
                        release,
                        "--kube-context",
                        context,
                        "--namespace",
                        namespace,
                    ])
                    .arg(chart)
                    .arg("--values")
                    .arg(values)
                    .args([
                        "--set-string",
                        &format!("canaryImage={NEW_IMAGE}"),
                        "--set-string",
                        &format!("canaryClaim={canary_pvc}"),
                        "--set-string",
                        &format!("rollbackClaim={rollback_pvc}"),
                        "--set",
                        "enabled=true",
                        "--set-string",
                        &format!("runId={}", spec.run_id),
                        "--wait",
                        "--timeout",
                        "150s",
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .status(),
            )
            .await
            .map_err(|_| error("Qdrant Helm canary upgrade timed out"))??;
            if !status.success() {
                return fail("Qdrant Helm canary upgrade failed; source remains untouched");
            }
        }
    }
    assert_isolated(spec, false).await
}
pub(super) fn render(spec: &Spec, receipt: &str) -> Result<()> {
    if spec.corpus_kind != "production_export" {
        return fail("fixture cannot emit deployment admission/configuration");
    }
    // Only emits a reviewable candidate artifact. Never switches traffic or
    // edits the main chart/base pin. Root's release owner executes cutover.
    let value = json!({"schema":"qdrant-upgrade-admission/v1","run_id":spec.run_id,"candidate_image":NEW_IMAGE,"rollback_image":OLD_IMAGE,"rollback_receipt_sha256":receipt,"spec_sha256":spec.fingerprint()?,"writes_must_remain_quiesced":true,"automatic_cutover":false,"acceptance":"rehearsal_passed_production_cutover_not_run"});
    write_new(&spec.artifacts.join("admission.json"), &value)?;
    match &spec.deployment {
        Deployment::Compose { canary_volume, .. } => {
            let value = json!({"services":{"qdrant":{"image":NEW_IMAGE,"volumes":[{"type":"volume","source":"qdrant-upgrade-verified","target":"/qdrant/storage"}]}},"volumes":{"qdrant-upgrade-verified":{"external":true,"name":canary_volume}}});
            write_new(&spec.artifacts.join("compose.verified.json"), &value)
        }
        Deployment::Helm { release, .. } => write_new(
            &spec.artifacts.join("helm.verified.values.json"),
            &json!({"data":{"qdrant":{"url":spec.canary.url,"port":6333,"peer":{"podSelector":{"matchLabels":{"app.kubernetes.io/instance":release,"agentforge.io/qdrant-upgrade-run":spec.run_id,"agentforge.io/qdrant-upgrade-role":"canary"}}}}}}),
        ),
    }
}

pub(super) async fn prepare(spec: &Spec) -> Result<()> {
    match &spec.deployment {
        Deployment::Compose {
            project,
            files,
            env_file,
            source_service,
            source_volume,
            canary_service,
            rollback_service,
            canary_volume,
            rollback_volume,
        } => {
            container(project, source_service, &spec.repository, source_volume).await?;
            let digests: Value = serde_json::from_slice(
                &output(
                    "docker",
                    &[
                        "image".into(),
                        "inspect".into(),
                        "--format".into(),
                        "{{json .RepoDigests}}".into(),
                        OLD_IMAGE.into(),
                    ],
                )
                .await?,
            )?;
            if !digests
                .as_array()
                .is_some_and(|v| v.iter().any(|v| v == OLD_IMAGE))
            {
                return fail("reviewed old image must be preloaded; no implicit pull");
            }
            for volume in [canary_volume, rollback_volume] {
                let attached = output(
                    "docker",
                    &[
                        "ps".into(),
                        "-a".into(),
                        "-q".into(),
                        "--filter".into(),
                        format!("volume={volume}"),
                    ],
                )
                .await?;
                if !attached.iter().all(u8::is_ascii_whitespace) {
                    return fail("rehearsal volume is already attached; use a fresh run ID");
                }
            }
            // Fresh named volumes only. Initialize their root ownership using
            // the already reviewed/preloaded image, no network or credentials.
            for (volume, label) in [
                (canary_volume, "qdrant-upgrade-canary"),
                (rollback_volume, "qdrant-upgrade-rollback"),
            ] {
                let existing = output(
                    "docker",
                    &[
                        "volume".into(),
                        "ls".into(),
                        "--format".into(),
                        "{{.Name}}".into(),
                        "--filter".into(),
                        format!("name=^{volume}$"),
                    ],
                )
                .await?;
                if !existing.iter().all(u8::is_ascii_whitespace) {
                    return fail("rehearsal volume already exists; choose a fresh run ID");
                }
                output(
                    "docker",
                    &[
                        "volume".into(),
                        "create".into(),
                        "--label".into(),
                        format!("com.docker.compose.project={project}"),
                        "--label".into(),
                        format!("com.docker.compose.volume={label}"),
                        "--label".into(),
                        format!("agentforge.io/qdrant-upgrade-run={}", spec.run_id),
                        volume.clone(),
                    ],
                )
                .await?;
                output(
                    "docker",
                    &[
                        "run".into(),
                        "--rm".into(),
                        "--pull=never".into(),
                        "--network=none".into(),
                        "--read-only".into(),
                        "--cap-drop=ALL".into(),
                        "--cap-add=CHOWN".into(),
                        "--security-opt=no-new-privileges:true".into(),
                        "--user=0:0".into(),
                        "--mount".into(),
                        format!("type=volume,source={volume},target=/qdrant/initialize"),
                        "--entrypoint=/bin/chown".into(),
                        OLD_IMAGE.into(),
                        "1000:1000".into(),
                        "/qdrant/initialize".into(),
                    ],
                )
                .await?;
            }
            for service in [canary_service, rollback_service] {
                let mut cmd = Command::new("docker");
                cmd.arg("compose")
                    .arg("--project-directory")
                    .arg(&spec.repository)
                    .arg("--project-name")
                    .arg(project)
                    .arg("--env-file")
                    .arg(env_file);
                for file in files {
                    cmd.arg("--file").arg(file);
                }
                let status = tokio::time::timeout(
                    std::time::Duration::from_secs(180),
                    cmd.args([
                        "up",
                        "-d",
                        "--no-deps",
                        "--no-build",
                        "--pull",
                        "never",
                        "--wait",
                    ])
                    .arg(service)
                    .env("QDRANT_UPGRADE_CANARY_IMAGE", OLD_IMAGE)
                    .env("QDRANT_UPGRADE_RUN_ID", &spec.run_id)
                    .env("QDRANT_UPGRADE_CANARY_VOLUME", canary_volume)
                    .env("QDRANT_UPGRADE_ROLLBACK_VOLUME", rollback_volume)
                    .env("COMPOSE_PARALLEL_LIMIT", "1")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .status(),
                )
                .await
                .map_err(|_| error("Qdrant rehearsal preparation timed out"))??;
                if !status.success() {
                    return fail(
                        "Qdrant rehearsal preparation failed; artifacts/resources retained",
                    );
                }
            }
        }
        Deployment::Helm {
            context,
            namespace,
            release,
            chart,
            values,
            canary_pvc,
            rollback_pvc,
            source_pvc,
            ..
        } => {
            distinct_claims(context, namespace, [source_pvc, canary_pvc, rollback_pvc]).await?;
            let releases: Value = serde_json::from_slice(
                &output(
                    "helm",
                    &[
                        "list".into(),
                        "--kube-context".into(),
                        context.into(),
                        "--namespace".into(),
                        namespace.into(),
                        "--filter".into(),
                        format!("^{release}$"),
                        "--output".into(),
                        "json".into(),
                    ],
                )
                .await?,
            )?;
            if !releases.as_array().is_some_and(Vec::is_empty) {
                return fail("Helm rehearsal release already exists; use a fresh run ID/release");
            }
            let status = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                Command::new("helm")
                    .args([
                        "install",
                        release,
                        "--kube-context",
                        context,
                        "--namespace",
                        namespace,
                    ])
                    .arg(chart)
                    .arg("--values")
                    .arg(values)
                    .args([
                        "--set-string",
                        &format!("canaryImage={OLD_IMAGE}"),
                        "--set",
                        "enabled=true",
                        "--set-string",
                        &format!("runId={}", spec.run_id),
                        "--set-string",
                        &format!("canaryClaim={canary_pvc}"),
                        "--set-string",
                        &format!("rollbackClaim={rollback_pvc}"),
                        "--wait",
                        "--timeout",
                        "150s",
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .status(),
            )
            .await
            .map_err(|_| error("Qdrant Helm rehearsal preparation timed out"))??;
            if !status.success() {
                return fail("Qdrant Helm rehearsal preparation failed; no automatic cleanup");
            }
        }
    }
    assert_isolated(spec, false).await?;
    assert_isolated(spec, true).await?;
    let canary = Node::new(&spec.canary)?;
    let rollback = Node::new(&spec.rollback)?;
    for node in [&canary, &rollback] {
        node.wait_version(OLD_VERSION).await?;
        if !node.collections().await?.is_empty() {
            return fail("rehearsal storage must be empty; never delete existing data");
        }
    }
    write_new(
        &spec.artifacts.join("prepare.json"),
        &json!({"schema":"qdrant-upgrade-prepare/v1","run_id":spec.run_id,"spec_sha256":spec.fingerprint()?,"source_mutated":false,"acceptance":"not_run"}),
    )
}
