//! Read-only source export for Runtime/Worker startup admission.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().len() != 1 {
        return Err("schema-admission-contract accepts no arguments".into());
    }
    let snapshot =
        ai_platform_migration_authority::application_guard::compiled_admission_snapshot()?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}
