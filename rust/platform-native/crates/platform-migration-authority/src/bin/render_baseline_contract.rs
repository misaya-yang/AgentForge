use ai_platform_migration_authority::{
    AuthorityError, Result, source_tools::render_contract as render,
};
use std::{collections::BTreeMap, path::PathBuf};
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
fn run() -> Result<()> {
    let mut paths = BTreeMap::new();
    let mut write = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--write" {
            write = true;
        } else if [
            "--inventory",
            "--ownership-policy",
            "--grants-policy",
            "--cutover",
            "--verify",
        ]
        .contains(&arg.as_str())
        {
            paths.insert(
                arg,
                PathBuf::from(args.next().ok_or_else(|| {
                    AuthorityError::Refused("missing render path argument".into())
                })?),
            );
        } else {
            return Err(AuthorityError::Refused("unknown render argument".into()));
        }
    }
    for key in [
        "--inventory",
        "--ownership-policy",
        "--grants-policy",
        "--cutover",
        "--verify",
    ] {
        if !paths.contains_key(key) {
            return Err(AuthorityError::Refused(format!("{key} is required")));
        }
    }
    let inventory = std::fs::read(&paths["--inventory"])?;
    let objects =
        render::load_policy_bytes(&inventory, &std::fs::read(&paths["--ownership-policy"])?)?;
    let matrix = render::load_grant_matrix(&inventory, &std::fs::read(&paths["--grants-policy"])?)?;
    let prior = std::fs::read_to_string(&paths["--cutover"])?;
    let cutover = render::replace_relocation(&prior, &render::render_relocation(&objects))?;
    let verify = render::render_verify(&objects, &matrix)?;
    if write {
        std::fs::write(&paths["--cutover"], cutover)?;
        std::fs::write(&paths["--verify"], verify)?;
    } else if cutover != prior
        || std::fs::read_to_string(&paths["--verify"]).ok().as_deref() != Some(&verify)
    {
        return Err(AuthorityError::Refused("DRIFT generated ARC-03 SQL".into()));
    }
    Ok(())
}
