use ai_platform_migration_authority::{AuthorityError, Result, source_tools::successors};
use std::path::PathBuf;
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
fn run() -> Result<()> {
    run_with_args(std::env::args().skip(1))
}
fn run_with_args(arguments: impl IntoIterator<Item = String>) -> Result<()> {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()?;
    let mut output = None;
    let mut preview = None;
    let mut activation_bundle_output = None;
    let mut preimage_root = None;
    let mut next_021 = false;
    let mut next_022 = false;
    let mut next_023 = false;
    let mut measured = false;
    let mut args = arguments.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "check-successor-proposals [--root SOURCE_DIR] [--preimage-root PRESERVED_CHECKOUT] [--preview-output NEW_EXTERNAL_DIRECTORY] [--next-successor-021|--next-successor-022|--next-successor-023 [--preview-root PREVIEW] [--measured] [--activation-bundle-output NEW_EXTERNAL_DIRECTORY]]"
                );
                return Ok(());
            }
            "--root" => {
                root = PathBuf::from(
                    args.next()
                        .ok_or_else(|| AuthorityError::Refused("--root requires a path".into()))?,
                )
            }
            "--preview-output" => {
                output = Some(PathBuf::from(args.next().ok_or_else(|| {
                    AuthorityError::Refused("--preview-output requires a path".into())
                })?))
            }
            "--preview-root" => {
                preview = Some(PathBuf::from(args.next().ok_or_else(|| {
                    AuthorityError::Refused("--preview-root requires a path".into())
                })?))
            }
            "--activation-bundle-output" => {
                activation_bundle_output = Some(PathBuf::from(args.next().ok_or_else(|| {
                    AuthorityError::Refused("--activation-bundle-output requires a path".into())
                })?))
            }
            "--next-successor-021" => {
                if next_021 {
                    return Err(AuthorityError::Refused(
                        "duplicate --next-successor-021 flag".into(),
                    ));
                }
                next_021 = true;
            }
            "--next-successor-022" => {
                if next_022 {
                    return Err(AuthorityError::Refused(
                        "duplicate --next-successor-022 flag".into(),
                    ));
                }
                next_022 = true;
            }
            "--next-successor-023" => {
                if next_023 {
                    return Err(AuthorityError::Refused(
                        "duplicate --next-successor-023 flag".into(),
                    ));
                }
                next_023 = true;
            }
            "--measured" => {
                if measured {
                    return Err(AuthorityError::Refused("duplicate --measured flag".into()));
                }
                measured = true;
            }
            "--preimage-root" => {
                preimage_root = Some(PathBuf::from(args.next().ok_or_else(|| {
                    AuthorityError::Refused("--preimage-root requires a path".into())
                })?))
            }
            _ => {
                return Err(AuthorityError::Refused(
                    "unknown successor checker argument".into(),
                ));
            }
        }
    }
    if [next_021, next_022, next_023]
        .into_iter()
        .filter(|v| *v)
        .count()
        > 1
    {
        return Err(AuthorityError::Refused(
            "choose only one successor extension flag".into(),
        ));
    }
    if next_021 {
        if preimage_root.is_some()
            || (preview.is_some() && output.is_some())
            || (activation_bundle_output.is_some() && (preview.is_none() || output.is_some()))
            || (activation_bundle_output.is_some() && !measured)
        {
            return Err(AuthorityError::Refused(
                "021 bundle preparation requires a measured preview and cannot be combined with source materialization or --preimage-root".into(),
            ));
        }
        if measured && preview.is_none() {
            return Err(AuthorityError::Refused(
                "--measured requires --preview-root".into(),
            ));
        }
        let result = if let Some(bundle_output) = activation_bundle_output {
            let preview = preview.as_deref().ok_or_else(|| {
                AuthorityError::Refused("activation bundle requires --preview-root".into())
            })?;
            ai_platform_migration_authority::source_tools::next_successor::activation_bundle(
                &root,
                preview,
                &bundle_output,
            )?
        } else if let Some(output) = output {
            if preview.is_some() || measured {
                return Err(AuthorityError::Refused(
                    "021 materialization cannot be combined with preview verification".into(),
                ));
            }
            ai_platform_migration_authority::source_tools::next_successor::materialize(
                &root, &output,
            )?
        } else if let Some(preview) = preview {
            if measured {
                let (candidate, receipt) = ai_platform_migration_authority::source_tools::next_successor::verify_measured_preview(
                    &root,
                    &preview,
                )?;
                serde_json::json!({
                    "status":"measured_021_preview_verified",
                    "predecessor_epoch":20,
                    "candidate_epoch":21,
                    "candidate_manifest_sha256":receipt["candidate_manifest_sha256"],
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            } else {
                let (candidate, _) =
                    ai_platform_migration_authority::source_tools::next_successor::verify_preview(
                        &root, &preview,
                    )?;
                serde_json::json!({
                    "status":"inactive_021_preview_verified",
                    "predecessor_epoch":20,
                    "candidate_epoch":21,
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            }
        } else {
            let active_epoch = ai_platform_migration_authority::manifest::load_epoch_manifest(
                &root.join("database/migrations/2026_08_post_kb_v1/manifest.yml"),
            )?;
            if matches!(
                active_epoch.epoch,
                23 | 24 | 25 | 26 | 27 | 28 | 29 | 30 | 31 | 32 | 33 | 34 | 35
            ) {
                let (chain, _) = successors::verify_activated_source(&root)?;
                serde_json::json!({
                    "status":"historical_021_prefix_verified_through_current",
                    "canonical_epoch":active_epoch.epoch,
                    "candidate_epoch":21,
                    "database_actions":false,
                    "verified_chain":chain
                })
            } else if active_epoch.epoch == 22 {
                let (plan, manifest) = ai_platform_migration_authority::source_tools::next_successor_022::verify_activated_source(&root)?;
                serde_json::json!({
                    "status":"historic_021_prefix_verified_through_022",
                    "canonical_epoch":manifest.epoch,
                    "candidate_epoch":21,
                    "successor_plan_sha256":ai_platform_migration_authority::manifest::file_sha256(&root.join("docs/design/canonical-schema-successor-022.json"))?,
                    "database_actions":false,
                    "plan":plan["status"]
                })
            } else if active_epoch.epoch == 21 {
                let (plan, manifest) = ai_platform_migration_authority::source_tools::next_successor::verify_activated_source(&root)?;
                serde_json::json!({
                    "status":"activated_021_source_chain_verified",
                    "canonical_epoch":manifest.epoch,
                    "candidate_epoch":21,
                    "successor_plan_sha256":ai_platform_migration_authority::manifest::file_sha256(&root.join("docs/design/canonical-schema-successor-021.json"))?,
                    "database_actions":false,
                    "plan":plan["status"]
                })
            } else {
                let candidate =
                    ai_platform_migration_authority::source_tools::next_successor::validate_source(
                        &root,
                    )?;
                serde_json::json!({
                    "status":"source_021_invariants_passed",
                    "canonical_epoch":20,
                    "candidate_epoch":21,
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            }
        };
        println!("{result}");
        return Ok(());
    }
    if next_022 {
        if preimage_root.is_some()
            || (preview.is_some() && output.is_some())
            || (activation_bundle_output.is_some() && (preview.is_none() || output.is_some()))
            || (activation_bundle_output.is_some() && !measured)
        {
            return Err(AuthorityError::Refused(
                "022 bundle preparation requires a measured preview and cannot be combined with source materialization or --preimage-root".into(),
            ));
        }
        if measured && preview.is_none() {
            return Err(AuthorityError::Refused(
                "--measured requires --preview-root".into(),
            ));
        }
        let result = if let Some(bundle_output) = activation_bundle_output {
            let preview = preview.as_deref().ok_or_else(|| {
                AuthorityError::Refused("activation bundle requires --preview-root".into())
            })?;
            ai_platform_migration_authority::source_tools::next_successor_022::activation_bundle(
                &root,
                preview,
                &bundle_output,
            )?
        } else if let Some(output) = output {
            if preview.is_some() || measured {
                return Err(AuthorityError::Refused(
                    "022 materialization cannot be combined with preview verification".into(),
                ));
            }
            ai_platform_migration_authority::source_tools::next_successor_022::materialize(
                &root, &output,
            )?
        } else if let Some(preview) = preview {
            if measured {
                let (candidate, receipt) = ai_platform_migration_authority::source_tools::next_successor_022::verify_measured_preview(
                    &root,
                    &preview,
                )?;
                serde_json::json!({
                    "status":"measured_022_preview_verified",
                    "predecessor_epoch":21,
                    "candidate_epoch":22,
                    "candidate_manifest_sha256":receipt["candidate_manifest_sha256"],
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            } else {
                let (candidate, _) =
                    ai_platform_migration_authority::source_tools::next_successor_022::verify_preview(
                        &root, &preview,
                    )?;
                serde_json::json!({
                    "status":"inactive_022_preview_verified",
                    "predecessor_epoch":21,
                    "candidate_epoch":22,
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            }
        } else {
            let active_epoch = ai_platform_migration_authority::manifest::load_epoch_manifest(
                &root.join("database/migrations/2026_08_post_kb_v1/manifest.yml"),
            )?;
            if matches!(
                active_epoch.epoch,
                23 | 24 | 25 | 26 | 27 | 28 | 29 | 30 | 31 | 32 | 33 | 34 | 35
            ) {
                let (chain, _) = successors::verify_activated_source(&root)?;
                serde_json::json!({
                    "status":"historical_022_prefix_verified_through_current",
                    "canonical_epoch":active_epoch.epoch,
                    "candidate_epoch":22,
                    "database_actions":false,
                    "verified_chain":chain
                })
            } else if active_epoch.epoch == 22 {
                let (plan, manifest) = ai_platform_migration_authority::source_tools::next_successor_022::verify_activated_source(&root)?;
                serde_json::json!({
                    "status":"activated_022_source_chain_verified",
                    "canonical_epoch":manifest.epoch,
                    "candidate_epoch":22,
                    "successor_plan_sha256":ai_platform_migration_authority::manifest::file_sha256(&root.join("docs/design/canonical-schema-successor-022.json"))?,
                    "database_actions":false,
                    "plan":plan["status"]
                })
            } else {
                let candidate = ai_platform_migration_authority::source_tools::next_successor_022::validate_source(&root)?;
                serde_json::json!({
                    "status":"source_022_invariants_passed",
                    "canonical_epoch":21,
                    "candidate_epoch":22,
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            }
        };
        println!("{result}");
        return Ok(());
    }
    if next_023 {
        if preimage_root.is_some()
            || (preview.is_some() && output.is_some())
            || (activation_bundle_output.is_some() && (preview.is_none() || output.is_some()))
            || (activation_bundle_output.is_some() && !measured)
        {
            return Err(AuthorityError::Refused(
                "023 bundle preparation requires a measured preview and cannot be combined with source materialization or --preimage-root".into(),
            ));
        }
        if measured && preview.is_none() {
            return Err(AuthorityError::Refused(
                "--measured requires --preview-root".into(),
            ));
        }
        let result = if let Some(bundle_output) = activation_bundle_output {
            let preview = preview.as_deref().ok_or_else(|| {
                AuthorityError::Refused("activation bundle requires --preview-root".into())
            })?;
            ai_platform_migration_authority::source_tools::next_successor_023::activation_bundle(
                &root,
                preview,
                &bundle_output,
            )?
        } else if let Some(output) = output {
            if preview.is_some() || measured {
                return Err(AuthorityError::Refused(
                    "023 materialization cannot be combined with preview verification".into(),
                ));
            }
            ai_platform_migration_authority::source_tools::next_successor_023::materialize(
                &root, &output,
            )?
        } else if let Some(preview) = preview {
            if measured {
                let (candidate, receipt) = ai_platform_migration_authority::source_tools::next_successor_023::verify_measured_preview(
                    &root,
                    &preview,
                )?;
                serde_json::json!({
                    "status":"measured_023_preview_verified",
                    "predecessor_epoch":22,
                    "candidate_epoch":23,
                    "candidate_manifest_sha256":receipt["candidate_manifest_sha256"],
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            } else {
                let (candidate, _) =
                    ai_platform_migration_authority::source_tools::next_successor_023::verify_preview(
                        &root, &preview,
                    )?;
                serde_json::json!({
                    "status":"inactive_023_preview_verified",
                    "predecessor_epoch":22,
                    "candidate_epoch":23,
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            }
        } else {
            let active_epoch = ai_platform_migration_authority::manifest::load_epoch_manifest(
                &root.join("database/migrations/2026_08_post_kb_v1/manifest.yml"),
            )?;
            if matches!(
                active_epoch.epoch,
                24 | 25 | 26 | 27 | 28 | 29 | 30 | 31 | 32 | 33 | 34 | 35
            ) {
                let (chain, _) = successors::verify_activated_source(&root)?;
                serde_json::json!({
                    "status":"historical_023_prefix_verified_through_current",
                    "canonical_epoch":active_epoch.epoch,
                    "candidate_epoch":23,
                    "database_actions":false,
                    "verified_chain":chain
                })
            } else if active_epoch.epoch == 23 {
                let (plan, manifest) = ai_platform_migration_authority::source_tools::next_successor_023::verify_activated_source(&root)?;
                serde_json::json!({
                    "status":"activated_023_source_chain_verified",
                    "canonical_epoch":manifest.epoch,
                    "candidate_epoch":23,
                    "successor_plan_sha256":ai_platform_migration_authority::manifest::file_sha256(&root.join("docs/design/canonical-schema-successor-023.json"))?,
                    "database_actions":false,
                    "plan":plan["status"]
                })
            } else {
                let candidate = ai_platform_migration_authority::source_tools::next_successor_023::validate_source(&root)?;
                serde_json::json!({
                    "status":"source_023_invariants_passed",
                    "canonical_epoch":22,
                    "candidate_epoch":23,
                    "successor_plan_sha256":candidate.plan_sha256,
                    "database_actions":false
                })
            }
        };
        println!("{result}");
        return Ok(());
    }
    if preview.is_some() || measured || activation_bundle_output.is_some() {
        return Err(AuthorityError::Refused(
            "preview and activation bundle options require a successor extension flag".into(),
        ));
    }
    let active_manifest = ai_platform_migration_authority::manifest::load_epoch_manifest(
        &root.join("database/migrations/2026_08_post_kb_v1/manifest.yml"),
    )?;
    let activated = matches!(
        active_manifest.epoch,
        27 | 28 | 29 | 30 | 31 | 32 | 33 | 34 | 35
    ) || successors::activation_receipt_present(&root)?;
    let result = if activated {
        if output.is_some() {
            return Err(AuthorityError::Refused(
                "canonical successor chain is already activated; preview materialization is closed"
                    .into(),
            ));
        }
        let (plan, manifest) = if (30..=35).contains(&active_manifest.epoch) {
            (ai_platform_migration_authority::source_tools::service_successors::verify_activated_source(&root)?,active_manifest)
        } else if active_manifest.epoch == 29 {
            (
                ai_platform_migration_authority::source_tools::next_successor_029::verify_activated_source(&root)?,
                active_manifest,
            )
        } else if active_manifest.epoch == 28 {
            (
                ai_platform_migration_authority::source_tools::next_successor_028::verify_activated_source(&root)?,
                active_manifest,
            )
        } else if active_manifest.epoch == 27 {
            (
                ai_platform_migration_authority::source_tools::next_successor_027::verify_activated_source(&root)?,
                active_manifest,
            )
        } else if active_manifest.epoch == 23 {
            ai_platform_migration_authority::source_tools::next_successor_023::verify_activated_source(&root)?
        } else {
            successors::verify_activated_source(&root)?
        };
        let plan_sha256 = if matches!(manifest.epoch, 27 | 28 | 29 | 30 | 31 | 32 | 33 | 34 | 35) {
            plan["source_plan_sha256"]
                .as_str()
                .ok_or_else(|| {
                    AuthorityError::Refused(format!(
                        "{} activation omits its source plan digest",
                        manifest.epoch
                    ))
                })?
                .to_owned()
        } else {
            let plan_path = if manifest.epoch == 23 {
                "docs/design/canonical-schema-successor-023.json"
            } else {
                "docs/design/canonical-schema-successors.json"
            };
            ai_platform_migration_authority::manifest::file_sha256(&root.join(plan_path))?
        };
        serde_json::json!({"status":"activated_source_chain_verified","canonical_epoch":manifest.epoch,"successor_plan_sha256":plan_sha256,"predecessor_epoch":plan["predecessor_epoch"],"database_actions":false})
    } else if let Some(output) = output {
        successors::materialize_with_preimage_root(&root, &output, preimage_root.as_deref())?
    } else {
        let (plan, manifest) =
            successors::validate_with_preimage_root(&root, preimage_root.as_deref())?;
        serde_json::json!({"status":"source_invariants_passed","canonical_epoch":plan["predecessor_epoch"],"candidate_epoch":manifest["epoch"],"database_actions":false})
    };
    println!("{result}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_029_source_check_uses_its_028_activation_receipt() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        let manifest = ai_platform_migration_authority::manifest::load_epoch_manifest(
            &root.join("database/migrations/2026_08_post_kb_v1/manifest.yml"),
        )
        .unwrap();
        assert!((29..=35).contains(&manifest.epoch));
        let receipt =
            ai_platform_migration_authority::source_tools::service_successors::verify_anchor_029(
                &root,
            )
            .unwrap();
        assert_eq!(receipt["candidate_epoch"], 29);
        assert_eq!(receipt["predecessor_epoch"], 28);
        let predecessor_activation_path =
            root.join("database/migrations/2026_08_post_kb_v1/SUCCESSOR_028_ACTIVATION.json");
        let predecessor_activation_sha256 =
            ai_platform_migration_authority::manifest::file_sha256(&predecessor_activation_path)
                .unwrap();
        assert_eq!(
            receipt["predecessor_activation_sha256"].as_str(),
            Some(predecessor_activation_sha256.as_str())
        );
        assert_eq!(
            Some(predecessor_activation_sha256.as_str()),
            ai_platform_migration_authority::source_tools::next_successor_029::APPROVED_028_ACTIVATION_SHA256
        );
        let predecessor_activation: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&predecessor_activation_path).unwrap()).unwrap();
        assert_eq!(
            predecessor_activation["source_plan_sha256"].as_str(),
            ai_platform_migration_authority::source_tools::next_successor_029::APPROVED_028_SOURCE_PLAN_SHA256
        );
        assert!(receipt["source_plan_sha256"].as_str().is_some());
        run_with_args(["--root".to_owned(), root.to_string_lossy().into_owned()]).unwrap();
    }

    #[test]
    fn reviewed_023_source_check_dispatches_through_the_current_029_prefix() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        run_with_args([
            "--next-successor-023".to_owned(),
            "--root".to_owned(),
            root.to_string_lossy().into_owned(),
        ])
        .unwrap();
    }
    #[test]
    fn invalid_023_cli_combinations_fail_before_source_materialization() {
        for arguments in [
            vec!["--next-successor-023", "--next-successor-023"],
            vec!["--next-successor-023", "--next-successor-022"],
            vec!["--next-successor-023", "--next-successor-021"],
            vec!["--next-successor-023", "--measured"],
            vec![
                "--next-successor-023",
                "--preimage-root",
                "/unavailable-old-import",
            ],
            vec![
                "--next-successor-023",
                "--activation-bundle-output",
                "/unused-output",
            ],
            vec![
                "--next-successor-023",
                "--preview-root",
                "/unused-preview",
                "--preview-output",
                "/unused-output",
            ],
            vec![
                "--next-successor-023",
                "--preview-root",
                "/unused-preview",
                "--activation-bundle-output",
                "/unused-output",
            ],
        ] {
            let error = run_with_args(arguments.into_iter().map(str::to_owned)).unwrap_err();
            assert!(
                matches!(error, AuthorityError::Refused(_)),
                "must reject options before reading source: {error}"
            );
        }
    }
}
