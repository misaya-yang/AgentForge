//! Workflow security admission and validated external actionlint input copies.
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
        "semantics":"standard GitHub local references to the running checkout",
        "limit":"external copies are validated inputs, not syntax translations"}),
    )
}
