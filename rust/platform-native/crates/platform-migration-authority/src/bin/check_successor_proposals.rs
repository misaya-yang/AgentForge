use ai_platform_migration_authority::{AuthorityError, Result, source_tools::successors};
use std::path::PathBuf;
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
fn run() -> Result<()> {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let mut output = None;
    let mut preview = None;
    let mut activation_bundle_output = None;
    let mut preimage_root = None;
    let mut next_021 = false;
    let mut next_022 = false;
    let mut measured = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "check-successor-proposals [--root SOURCE_DIR] [--preimage-root PRESERVED_CHECKOUT] [--preview-output NEW_EXTERNAL_DIRECTORY] [--next-successor-021|--next-successor-022 [--preview-root PREVIEW] [--measured] [--activation-bundle-output NEW_EXTERNAL_DIRECTORY]]"
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
    if next_021 && next_022 {
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
            if active_epoch.epoch == 22 {
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
            if active_epoch.epoch == 22 {
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
    if preview.is_some() || measured || activation_bundle_output.is_some() {
        return Err(AuthorityError::Refused(
            "preview and activation bundle options require a successor extension flag".into(),
        ));
    }
    let activated = successors::activation_receipt_present(&root)?;
    let result = if activated {
        if output.is_some() {
            return Err(AuthorityError::Refused(
                "canonical successor chain is already activated; preview materialization is closed"
                    .into(),
            ));
        }
        let (plan, manifest) = successors::verify_activated_source(&root)?;
        serde_json::json!({"status":"activated_source_chain_verified","canonical_epoch":manifest.epoch,"successor_plan_sha256":ai_platform_migration_authority::manifest::file_sha256(&root.join("docs/design/canonical-schema-successors.json"))?,"predecessor_epoch":plan["predecessor_epoch"],"database_actions":false})
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
