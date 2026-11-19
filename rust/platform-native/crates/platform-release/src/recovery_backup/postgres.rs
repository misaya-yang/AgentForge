use super::qdrant::{json as qjson, request as qdrant};
use super::*;
use sqlx::Row;
pub(super) fn validate_repository(c: &Config) -> Result<()> {
    let raw = c.secret("pgbackrest_config")?;
    let mut values = std::collections::BTreeMap::new();
    for line in raw
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#') && !s.starts_with('['))
    {
        let (k, v) = line
            .split_once('=')
            .ok_or("pgBackRest config must use key=value")?;
        need(
            values.insert(k.trim(), v.trim()).is_none(),
            "duplicate pgBackRest keys refused",
        )?;
    }
    need(
        !values
            .keys()
            .any(|key| key.starts_with("tablespace-map") || key.starts_with("link-map")),
        "source configuration cannot override isolated restore path mappings",
    )?;
    for (k, v) in [
        ("repo1-type", "s3"),
        ("repo1-cipher-type", "aes-256-cbc"),
        ("repo1-storage-verify-tls", "y"),
        ("archive-async", "n"),
    ] {
        need(
            values.get(k) == Some(&v),
            "encrypted synchronous S3 pgBackRest policy required",
        )?;
    }
    need(
        values.get("repo1-path") == Some(&"/pgbackrest"),
        "explicit /pgbackrest repository namespace required",
    )?;
    need(
        values.get("repo1-s3-bucket") == Some(&c.value("bucket")?),
        "pgBackRest bucket binding mismatch",
    )?;
    need(
        values.get("pg1-path") == Some(&c.value("pg_source_path")?),
        "pgBackRest source path mismatch",
    )?;
    need(
        values.get("pg1-port").and_then(|p| p.parse::<u64>().ok())
            == c.v["pg_source_port"].as_u64(),
        "pgBackRest source port mismatch",
    )?;
    let endpoint = local_url(c.value("s3_source")?)?;
    need(
        endpoint.scheme() == "https",
        "encrypted backup repository requires verified HTTPS",
    )?;
    need(
        values.get("repo1-s3-endpoint").copied() == endpoint.host_str()
            && values
                .get("repo1-storage-port")
                .and_then(|s| s.parse::<u16>().ok())
                == endpoint.port_or_known_default(),
        "repository endpoint differs from local allowlist",
    )?;
    need(
        values.get("repo1-s3-region") == Some(&c.value("s3_region")?)
            && values.get("repo1-s3-uri-style") == Some(&"path"),
        "repository region/path style mismatch",
    )?;
    need(
        !values.contains_key("pg1-host")
            && !values
                .keys()
                .any(|k| k.starts_with("pg2-") || k.starts_with("repo2-")),
        "remote/multiple PG repository configuration refused",
    )?;
    need(
        values
            .get("repo1-cipher-pass")
            .is_some_and(|p| p.len() >= 32 && !p.contains("REPLACE")),
        "external repository cipher password required",
    )?;
    let include = external(Path::new(c.value("pgbackrest_include_dir")?), &c.repo)?;
    need(
        fs::read_dir(include)
            .map_err(|_| "explicit empty pgBackRest include directory required")?
            .next()
            .is_none(),
        "pgBackRest include directory must be empty",
    )?;
    Ok(())
}
pub(super) async fn authority(c: &Config, drill: bool, evidence: &Path) -> Result<()> {
    use ai_platform_migration_authority::{
        commands,
        runner::{AuthorityPaths, MigrationAuthority},
    };
    let dsn = c.secret(if drill {
        "pg_drill_dsn_file"
    } else {
        "pg_source_dsn_file"
    })?;
    // Validate endpoint/database before authority opens its own connection.
    let conn = c.pg(drill).await?;
    tokio::time::timeout(Duration::from_secs(5), conn.close())
        .await
        .map_err(|_| "PG close timeout")?
        .map_err(|_| "PG close failed")?;
    let authority = MigrationAuthority::new(
        dsn,
        AuthorityPaths {
            database_dir: c.repo.join("database"),
        },
        c.value("role_prefix")?.into(),
    )
    .map_err(|_| "authority configuration refused")?;
    let start = Instant::now();
    tokio::time::timeout(
        c.timeout(),
        commands::verify(&authority, c.value("baseline")?),
    )
    .await
    .map_err(|_| "authority verify timeout")?
    .map_err(|_| "canonical authority verify refused")?;
    write_new(
        &evidence.join(if drill {
            "authority-restored.json"
        } else {
            "authority-source.json"
        }),
        &json!({"canonical_authority_verify":true,"baseline":c.value("baseline")?,"verified_unix":now(),"elapsed_ms":start.elapsed().as_millis(),"database":c.value("database")?,"restored":drill}),
    )
}
pub(super) async fn generations(conn: &mut PgConnection) -> Result<Value> {
    sqlx::query_scalar("SELECT COALESCE(jsonb_agg(jsonb_build_object('dataset_id',dataset_id,'tenant_id',tenant_id,'collection_name',collection_name,'content_revision',content_revision) ORDER BY dataset_id),'[]'::jsonb) FROM knowledge.datasets WHERE NOT is_deleted").fetch_one(conn).await.map_err(|_|"publication generation inventory failed".into())
}
pub(super) async fn backup(c: &Config, evidence: &Path, full: bool) -> Result<Value> {
    validate_repository(c)?;
    let store = objects::Store::new(c)?;
    store.policy(c).await?;
    authority(c, false, evidence).await?;
    let mut conn = c.pg(false).await?;
    let system_identifier: String =
        sqlx::query_scalar("SELECT system_identifier::text FROM pg_control_system()")
            .fetch_one(&mut conn)
            .await
            .map_err(|_| "source cluster lineage unavailable")?;
    let primary: bool = sqlx::query_scalar("SELECT NOT pg_is_in_recovery()")
        .fetch_one(&mut conn)
        .await
        .map_err(|_| "source recovery state unavailable")?;
    need(primary, "source must be the explicitly selected primary")?;
    let settings=sqlx::query("SELECT current_user::text AS principal,current_setting('archive_mode') AS archive_mode,current_setting('archive_timeout') AS archive_timeout,current_setting('archive_command') AS archive_command").fetch_one(&mut conn).await.map_err(|_|"archive policy query failed")?;
    need(
        settings.get::<String, _>("principal") == c.value("pg_backup_principal")?,
        "explicit backup principal required",
    )?;
    let expected_archive = format!(
        "{} --config={} --config-include-path={} --stanza={} archive-push %p",
        c.value("pgbackrest_bin")?,
        c.value("pgbackrest_config")?,
        c.value("pgbackrest_include_dir")?,
        c.value("stanza")?
    );
    need(
        settings.get::<String, _>("archive_mode") == "on"
            && settings.get::<String, _>("archive_command") == expected_archive,
        "exact configured pgBackRest archive-push command required",
    )?;
    let data: String = sqlx::query_scalar("SELECT current_setting('data_directory')")
        .fetch_one(&mut conn)
        .await
        .map_err(|_| "source data directory lookup failed")?;
    need(
        data == c.value("pg_source_path")?,
        "source cluster data directory mismatch",
    )?;
    let timeout: i64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM current_setting('archive_timeout')::interval)::bigint",
    )
    .fetch_one(&mut conn)
    .await
    .map_err(|_| "archive_timeout invalid")?;
    need(
        timeout > 0 && timeout <= 300,
        "archive_timeout must be 1..300 seconds",
    )?;
    pgbackrest(c, &["stanza-create"], false)?;
    pgbackrest(c, &["check"], false)?;
    let base = if full {
        pgbackrest(c, &["--type=full", "backup"], false)?
    } else {
        json!({"reused_existing_base_backup":true})
    };
    write_new(&evidence.join("base-backup.json"), &base)?;
    let info = pgbackrest(c, &["--output=json", "info"], true)?;
    write_new(&evidence.join("pgbackrest-info.json"), &info)?;
    let backup = info
        .as_array()
        .and_then(|a| a.first())
        .and_then(|s| s["backup"].as_array())
        .and_then(|a| a.last())
        .ok_or("completed base backup absent from pgBackRest info")?;
    let label = text(backup, "label")?.to_string();
    let stanza = info
        .as_array()
        .and_then(|a| a.first())
        .ok_or("stanza info missing")?;
    need(
        stanza["name"] == c.value("stanza")?,
        "backup stanza mismatch",
    )?;
    let dbid = &backup["database"]["id"];
    need(
        stanza["db"].as_array().is_some_and(|dbs| {
            dbs.iter().any(|db| {
                db["id"] == *dbid
                    && db["system-id"].as_u64().map(|n| n.to_string()).as_deref()
                        == Some(system_identifier.as_str())
            })
        }),
        "base backup belongs to another PG system identifier",
    )?;
    let physical_replica = if full {
        let files = store
            .inventory_prefix(
                c,
                &format!("pgbackrest/backup/{}/{label}/", c.value("stanza")?),
            )
            .await?;
        need(
            !files.is_empty(),
            "physical base backup replica inventory is empty",
        )?;
        files
    } else {
        Vec::new()
    };
    // Mutable repository index files are also required by an independent replica restore.
    let index_replica = store
        .inventory_prefix(
            c,
            &format!("pgbackrest/backup/{}/backup.info", c.value("stanza")?),
        )
        .await?;
    need(
        !index_replica.is_empty(),
        "replicated pgBackRest backup index missing",
    )?;
    let mut tx = conn
        .begin()
        .await
        .map_err(|_| "publication boundary transaction failed")?;
    sqlx::query("LOCK TABLE knowledge.datasets IN SHARE MODE")
        .execute(&mut *tx)
        .await
        .map_err(|_| "dataset inventory lock unavailable")?;
    let generations = generations(&mut tx).await?;
    for row in generations
        .as_array()
        .ok_or("generation inventory invalid")?
    {
        need(
            row["content_revision"].as_i64().is_some_and(|n| n >= 0),
            "active/incomplete publication cannot be backed up",
        )?;
        let key = format!("knowledge-dataset-index:{}", text(row, "dataset_id")?);
        let locked: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
                .bind(key)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| "publication lock query failed")?;
        need(
            locked,
            "active dataset publisher/index writer; retry backup later",
        )?;
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let point = format!("af_recovery_{id}");
    let collections = qjson(
        qdrant(
            c,
            &store.client,
            false,
            reqwest::Method::GET,
            &["collections"],
        )
        .await?,
    )
    .await?;
    let names = collections["result"]["collections"]
        .as_array()
        .ok_or("Qdrant collection inventory missing")?;
    let mut snapshots = Vec::new();
    for collection in names {
        let name = text(collection, "name")?;
        let result = qjson(
            qdrant(
                c,
                &store.client,
                false,
                reqwest::Method::POST,
                &["collections", name, "snapshots"],
            )
            .await?
            .query(&[("wait", "true")]),
        )
        .await?;
        let snap = text(&result["result"], "name")?;
        let size = result["result"]["size"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or("snapshot size missing")?;
        need(
            size <= c.v["max_snapshot_bytes"].as_u64().unwrap_or(0),
            "Qdrant snapshot exceeds explicit staging bound",
        )?;
        let response = qdrant(
            c,
            &store.client,
            false,
            reqwest::Method::GET,
            &["collections", name, "snapshots", snap],
        )
        .await?
        .send()
        .await
        .map_err(|_| "snapshot fetch failed")?
        .error_for_status()
        .map_err(|_| "snapshot fetch refused")?;
        let path = evidence.join(format!("snapshot-{}", uuid::Uuid::new_v4()));
        objects::download(response, &path, size).await?;
        let key = format!("recovery/{id}/qdrant/{name}/{snap}");
        let mut entry = store.upload(c, &path, &key).await?;
        entry["collection"] = json!(name);
        entry["snapshot"] = json!(snap);
        entry["sha256"] = json!(file_hash(&path)?);
        snapshots.push(entry);
        let removed = qjson(
            qdrant(
                c,
                &store.client,
                false,
                reqwest::Method::DELETE,
                &["collections", name, "snapshots", snap],
            )
            .await?
            .query(&[("wait", "true")]),
        )
        .await?;
        need(
            removed["result"] == true,
            "new Qdrant snapshot staging cleanup incomplete",
        )?;
        fs::remove_file(path).map_err(|_| "private staging cleanup failed")?;
    }
    let objects = store.inventory(c).await?;
    let lsn: String = sqlx::query_scalar("SELECT pg_create_restore_point($1)::text")
        .bind(&point)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| "named recovery point creation refused")?;
    let point_unix: i64 =
        sqlx::query_scalar("SELECT floor(extract(epoch FROM clock_timestamp()))::bigint")
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| "database clock query failed")?;
    let wal: String = sqlx::query_scalar("SELECT pg_walfile_name(pg_current_wal_lsn())")
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| "WAL segment lookup failed")?;
    sqlx::query("SELECT pg_switch_wal()")
        .execute(&mut *tx)
        .await
        .map_err(|_| "WAL switch failed")?;
    tx.commit()
        .await
        .map_err(|_| "publication boundary release failed")?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let archived: bool = sqlx::query_scalar(
            "SELECT COALESCE(last_archived_wal >= $1, false) FROM pg_stat_archiver",
        )
        .bind(&wal)
        .fetch_one(&mut conn)
        .await
        .map_err(|_| "WAL archive confirmation query failed")?;
        if archived {
            break;
        }
        need(
            Instant::now() < deadline,
            "named recovery point WAL not archived within RPO deadline",
        )?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    pgbackrest(c, &["check"], false)?;
    let db = stanza["db"]
        .as_array()
        .and_then(|dbs| dbs.iter().find(|db| db["id"] == *dbid))
        .ok_or("backup database lineage missing")?;
    let archive_id = format!(
        "{}-{}",
        text(db, "version")?,
        dbid.as_u64().ok_or("backup database ID invalid")?
    );
    let archive_index = store
        .inventory_prefix(
            c,
            &format!("pgbackrest/archive/{}/archive.info", c.value("stanza")?),
        )
        .await?;
    need(
        !archive_index.is_empty(),
        "replicated archive index missing",
    )?;
    let wal_files = store
        .inventory_prefix(
            c,
            &format!(
                "pgbackrest/archive/{}/{}/{}/",
                c.value("stanza")?,
                archive_id,
                wal.get(..16).ok_or("WAL identity invalid")?
            ),
        )
        .await?;
    let wal_replica: Vec<_> = wal_files
        .into_iter()
        .filter(|e| {
            e["key"].as_str().is_some_and(|key| {
                key.rsplit('/')
                    .next()
                    .is_some_and(|name| name.starts_with(&format!("{wal}-")))
            })
        })
        .collect();
    need(
        !wal_replica.is_empty(),
        "restore-point WAL has no verified replica version",
    )?;
    let manifest = json!({"schema":"agentforge-recovery-set/v1","target_id":c.value("target_id")?,"database":c.value("database")?,"baseline":c.value("baseline")?,"role_prefix":c.value("role_prefix")?,"backup_label":label,"system_identifier":system_identifier,"restore_point":point,"restore_lsn":lsn,"restore_point_unix":point_unix,"wal_segment":wal,"wal_replica":wal_replica,"base_replica_inventory":physical_replica,"repository_index_replica":index_replica,"archive_index_replica":archive_index,"wal_archive_confirmed_unix":now(),"generations":generations,"generation_sha256":hash(&serde_json::to_vec(&generations).map_err(|_|"generation encoding failed")?),"snapshots":snapshots,"objects":objects});
    write_new(&evidence.join("recovery-set.json"), &manifest)?;
    let mirror = store
        .upload(
            c,
            &evidence.join("recovery-set.json"),
            &format!("recovery/{id}/recovery-set.json"),
        )
        .await?;
    write_new(&evidence.join("manifest-replica.json"), &mirror)?;
    Ok(
        json!({"backup_completed":true,"recovery_set":evidence.join("recovery-set.json"),"restore_point_unix":point_unix,"wal_archived":true,"drill_accepted":false}),
    )
}
pub(super) fn file_hash(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = fs::File::open(path).map_err(|_| "backup artifact unreadable")?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b).map_err(|_| "backup artifact read failed")?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
