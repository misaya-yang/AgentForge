use super::*;
use std::net::{Ipv4Addr, TcpListener};

pub(super) fn validate_set(c: &Config, m: &Value) -> Result<()> {
    need(
        m["schema"] == "agentforge-recovery-set/v1",
        "unsupported recovery manifest",
    )?;
    for key in ["target_id", "database", "baseline", "role_prefix"] {
        need(
            m[key] == c.v[key],
            "recovery manifest belongs to another target/scope",
        )?;
    }
    for key in ["backup_label", "restore_point"] {
        need(identifier(text(m, key)?), "unsafe recovery selector")?;
    }
    need(
        text(m, "restore_point")?.starts_with("af_recovery_"),
        "foreign recovery point",
    )?;
    need(
        text(m, "system_identifier")?
            .bytes()
            .all(|b| b.is_ascii_digit())
            && regex::Regex::new(r"^[0-9A-F]+/[0-9A-F]+$")
                .map_err(|_| "LSN parser unavailable")?
                .is_match(text(m, "restore_lsn")?),
        "invalid PG lineage/LSN",
    )?;
    need(
        m["objects"].is_array() && m["snapshots"].is_array() && m["generations"].is_array(),
        "incomplete recovery manifest",
    )?;
    need(
        m["generation_sha256"]
            == hash(
                &serde_json::to_vec(&m["generations"]).map_err(|_| "generation encoding failed")?,
            ),
        "generation evidence tampered",
    )?;
    need(
        m["restore_point_unix"].as_u64().is_some_and(|t| t <= now()),
        "invalid/future recovery timestamp",
    )?;
    for e in m["objects"].as_array().unwrap() {
        let k = text(e, "key")?;
        need(
            k.starts_with(c.value("object_prefix")?)
                && !k.split('/').any(|p| p == ".." || p == "."),
            "foreign object prefix in manifest",
        )?;
    }
    for entry in m["objects"]
        .as_array()
        .unwrap()
        .iter()
        .chain(m["snapshots"].as_array().unwrap())
    {
        need(
            entry["size_bytes"]
                .as_u64()
                .is_some_and(|n| n <= 5 * 1024 * 1024 * 1024),
            "restore artifact exceeds streaming PUT bound",
        )?;
        need(
            !text(entry, "replica_version")?.eq("null")
                && !text(entry, "source_version")?.eq("null"),
            "unversioned restore artifact refused",
        )?;
        text(entry, "etag")?;
    }
    Ok(())
}
struct RunningCluster<'a> {
    c: &'a Config,
    path: PathBuf,
    started: bool,
}
impl Drop for RunningCluster<'_> {
    fn drop(&mut self) {
        if self.started {
            let _ = command(
                self.c,
                "pg_ctl_bin",
                &[
                    "-D".into(),
                    self.path.to_string_lossy().into(),
                    "-m".into(),
                    "fast".into(),
                    "-w".into(),
                    "-t".into(),
                    "30".into(),
                    "stop".into(),
                ],
                false,
            );
        }
    }
}
pub(super) async fn restore(c: &Config, evidence: &Path, manifest_path: &Path) -> Result<Value> {
    let started = Instant::now();
    let disaster_unix = now();
    let manifest_path = external(manifest_path, &c.repo)?;
    let meta = fs::symlink_metadata(&manifest_path).map_err(|_| "manifest metadata unavailable")?;
    need(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 128 * 1024 * 1024,
        "bounded regular recovery manifest required",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        need(
            meta.mode() & 0o077 == 0 && meta.uid() == unsafe { libc::geteuid() },
            "recovery manifest must be private and operator-owned",
        )?;
    }
    let m: Value = serde_json::from_slice(
        &fs::read(&manifest_path).map_err(|_| "external manifest unavailable")?,
    )
    .map_err(|_| "recovery manifest invalid")?;
    validate_set(c, &m)?;
    postgres::validate_repository(c)?;
    let rpo = disaster_unix
        - m["restore_point_unix"]
            .as_u64()
            .ok_or("recovery timestamp invalid")?;
    // A drill may restore an older set, but must report the real resulting RPO failure.
    let root = external(Path::new(c.value("drill_root")?), &c.repo)?;
    need(root.is_dir(), "operator must prepare external drill root")?;
    let destination = root.join(format!("drill-{}", uuid::Uuid::new_v4()));
    private_dir(&destination)?;
    write_new(
        &evidence.join("destination.json"),
        &json!({"path":destination,"port":c.v["pg_drill_port"],"disaster_unix":disaster_unix,"recovery_set_sha256":postgres::file_hash(&manifest_path)?}),
    )?;
    let port = c.v["pg_drill_port"].as_u64().ok_or("drill port missing")? as u16;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|_| "drill PG port must be unused")?;
    drop(listener);
    let store = objects::Store::new(c)?;
    store.policy(c).await?;
    let existing = qdrant::json(
        qdrant::request(
            c,
            &store.client,
            true,
            reqwest::Method::GET,
            &["collections"],
        )
        .await?,
    )
    .await?;
    need(
        existing["result"]["collections"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "drill Qdrant must contain no collections; overwrite forbidden",
    )?;
    let replica = local_url(c.value("s3_replica")?)?;
    need(
        replica.scheme() == "https",
        "pgBackRest replica transport must verify HTTPS",
    )?;
    let argv = [
        format!("--pg1-path={}", destination.display()),
        format!("--pg1-port={port}"),
        format!("--repo1-s3-bucket={}", c.value("replica_bucket")?),
        format!(
            "--repo1-s3-endpoint={}",
            replica.host_str().ok_or("replica host missing")?
        ),
        format!(
            "--repo1-storage-port={}",
            replica
                .port_or_known_default()
                .ok_or("replica port missing")?
        ),
        format!("--set={}", text(&m, "backup_label")?),
        format!("--tablespace-map-all={}/tablespaces", destination.display()),
        "--no-link-all".into(),
        "--type=name".into(),
        format!("--target={}", text(&m, "restore_point")?),
        "--target-action=promote".into(),
        "restore".into(),
    ];
    let refs: Vec<_> = argv.iter().map(String::as_str).collect();
    pgbackrest(c, &refs, false)?;
    need(
        destination.join("PG_VERSION").is_file(),
        "pgBackRest did not restore a cluster",
    )?;
    // Make archive-get's replica binding persistent for the spawned server too.
    let archive_get = format!(
        "{} --config={} --config-include-path={} --stanza={} --repo1-s3-bucket={} --repo1-s3-endpoint={} --repo1-storage-port={} archive-get %f %p",
        c.value("pgbackrest_bin")?,
        c.value("pgbackrest_config")?,
        c.value("pgbackrest_include_dir")?,
        c.value("stanza")?,
        c.value("replica_bucket")?,
        replica.host_str().ok_or("replica host missing")?,
        replica
            .port_or_known_default()
            .ok_or("replica port missing")?
    );
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(destination.join("postgresql.auto.conf"))
            .map_err(|_| "restored recovery configuration unavailable")?;
        writeln!(f, "\nrestore_command = '{archive_get}'")
            .map_err(|_| "replica archive-get binding failed")?;
        f.sync_all()
            .map_err(|_| "recovery configuration sync failed")?;
    }
    // Do not inherit source LDAP/PAM/HBA hooks or permit access beyond this drill identity.
    let hba = destination.join("drill.pg_hba.conf");
    {
        use std::io::Write;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&hba)
            .map_err(|_| "isolated HBA create failed")?;
        write!(file,"local all all reject\nhost {} {} 127.0.0.1/32 scram-sha-256\nhost all all 0.0.0.0/0 reject\nhost all all ::0/0 reject\n",c.value("database")?,c.value("pg_backup_principal")?).map_err(|_|"isolated HBA write failed")?;
        file.sync_all().map_err(|_| "isolated HBA sync failed")?;
    }
    let opts = format!(
        "-h 127.0.0.1 -p {port} -c data_directory={} -c unix_socket_directories={} -c hba_file={}/drill.pg_hba.conf -c ident_file=/dev/null -c max_logical_replication_workers=0 -c archive_cleanup_command='' -c recovery_end_command='' -c primary_conninfo='' -c primary_slot_name='' -c shared_preload_libraries='' -c archive_mode=off -c archive_command='' -c ssl=off -c external_pid_file='' -c log_directory=log",
        destination.display(),
        destination.display(),
        destination.display()
    );
    // Paths used in PostgreSQL's option string cannot contain shell-like token boundaries.
    need(
        !destination
            .to_string_lossy()
            .contains([' ', '\t', '\n', '\'', '"', '\\']),
        "drill root must not contain PostgreSQL option delimiters",
    )?;
    let mut cluster = RunningCluster {
        c,
        path: destination.clone(),
        started: true,
    };
    command(
        c,
        "pg_ctl_bin",
        &[
            "-D".into(),
            destination.to_string_lossy().into(),
            "-l".into(),
            destination
                .join("drill-postgres.log")
                .to_string_lossy()
                .into(),
            "-w".into(),
            "-t".into(),
            "60".into(),
            "-o".into(),
            opts,
            "start".into(),
        ],
        false,
    )?;
    cluster.started = true;
    let mut conn = c.pg(true).await?;
    let system: String =
        sqlx::query_scalar("SELECT system_identifier::text FROM pg_control_system()")
            .fetch_one(&mut conn)
            .await
            .map_err(|_| "restored cluster identity unavailable")?;
    need(
        m["system_identifier"] == system,
        "restored cluster lineage mismatch",
    )?;
    let promoted: bool =
        sqlx::query_scalar("SELECT NOT pg_is_in_recovery() AND pg_current_wal_lsn() >= $1::pg_lsn")
            .bind(text(&m, "restore_lsn")?)
            .fetch_one(&mut conn)
            .await
            .map_err(|_| "PITR completion check failed")?;
    need(promoted, "PITR target not reached/promoted")?;
    let generations = postgres::generations(&mut conn).await?;
    need(
        hash(&serde_json::to_vec(&generations).map_err(|_| "generation encoding failed")?)
            == m["generation_sha256"],
        "restored publication generations mismatch",
    )?;
    tokio::time::timeout(Duration::from_secs(5), conn.close())
        .await
        .map_err(|_| "restored PG close timeout")?
        .map_err(|_| "restored PG close failed")?;
    postgres::authority(c, true, evidence).await?;
    let object_count = store
        .restore_objects(c, m["objects"].as_array().ok_or("objects absent")?)
        .await?;
    for e in m["snapshots"].as_array().ok_or("snapshots absent")? {
        let path = destination.join(format!("snapshot-{}", uuid::Uuid::new_v4()));
        store.download(c, e, &path).await?;
        need(
            postgres::file_hash(&path)? == text(e, "sha256")?,
            "Qdrant snapshot checksum mismatch",
        )?;
        let collection = text(e, "collection")?;
        let file = tokio::fs::File::open(&path)
            .await
            .map_err(|_| "restored snapshot unavailable")?;
        let size = e["size_bytes"].as_u64().ok_or("snapshot size missing")?;
        let part = reqwest::multipart::Part::stream_with_length(reqwest::Body::from(file), size)
            .file_name("restore.snapshot");
        let request = qdrant::request(
            c,
            &store.client,
            true,
            reqwest::Method::POST,
            &["collections", collection, "snapshots", "upload"],
        )
        .await?
        .query(&[("wait", "true"), ("priority", "snapshot")])
        .multipart(reqwest::multipart::Form::new().part("snapshot", part));
        let result = qdrant::json(request).await?;
        need(
            result["result"] == true,
            "Qdrant snapshot restore did not complete",
        )?;
        fs::remove_file(path).map_err(|_| "private snapshot cleanup failed")?;
    }
    let elapsed = started.elapsed().as_secs();
    let report = json!({"schema":"agentforge-recovery-drill/v1","recovered":true,"rpo_seconds":rpo,"rto_seconds":elapsed,"rpo_target_seconds":300,"rto_target_seconds":1800,"slo_pass":rpo<=300&&elapsed<=1800,"authority_verify":true,"publication_generation_match":true,"objects_restored":object_count,"snapshots_restored":m["snapshots"].as_array().map(Vec::len),"disaster_observed_unix":disaster_unix,"completed_unix":now(),"data_path":destination,"production_touched":false,"remaining":"Application/provider retrieval and blob consumer smoke tests must be run against isolated restored endpoints before product acceptance"});
    // Stop only this newly created isolated cluster; keep its data and all evidence.
    command(
        c,
        "pg_ctl_bin",
        &[
            "-D".into(),
            destination.to_string_lossy().into(),
            "-m".into(),
            "fast".into(),
            "-w".into(),
            "-t".into(),
            "30".into(),
            "stop".into(),
        ],
        false,
    )?;
    cluster.started = false;
    write_new(&evidence.join("drill-measurements.json"), &report)?;
    need(
        rpo <= 300 && elapsed <= 1800,
        "restore completed but measured RPO/RTO failed; see drill-measurements.json",
    )?;
    Ok(report)
}
