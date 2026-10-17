//! Workflow security admission and old-actionlint compatibility copies.
use crate::*;
#[path = "workflow_self_refs.rs"]
mod self_refs;

pub fn run(args: &Args) -> Result<Value> {
    require(
        args.positionals.get(1).map(String::as_str) == Some("self-ref"),
        "expected workflow-lint self-ref",
    )?;
    let output = args.values.get("--actionlint-copy").map(PathBuf::from);
    let count = self_refs::check(&args.root(), output.as_deref())?;
    Ok(
        json!({"status":"passed", "workflows":count, "actionlint_copy":output,
        "semantics":"GitHub.com immutable running-commit self references",
        "limit":"actionlint 1.7.12 requires the validated external compatibility copy"}),
    )
}
