use crate::*;

pub fn run(args: &Args) -> Result<Value> {
    require(
        args.values
            .keys()
            .all(|k| ["--repo-root", "--evidence-out"].contains(&k.as_str()))
            && args.flags.is_empty(),
        "ci-capacity accepts only --repo-root and --evidence-out; resource facts cannot be overridden",
    )?;
    let capacity = ci_capacity_policy::detect()?;
    let value = json!({
        "schema_version":"ai-platform/ci-capacity/v1", "result":"pass",
        "policy":"standard-hosted-opt-in-max4-reserve2048-mib-per-job3072",
        "hosted":capacity.hosted, "cpus":capacity.cpus, "memory_mib":capacity.memory_mib,
        "cargo_build_jobs":capacity.build_jobs, "nextest_test_threads":capacity.test_threads,
        "ambient_cargo_build_jobs":std::env::var("CARGO_BUILD_JOBS").ok(),
        "ambient_nextest_test_threads":std::env::var("NEXTEST_TEST_THREADS").ok(),
        "cargo_incremental":0, "dev_debug":0, "test_debug":0,
    });
    if let Some(path) = args.values.get("--evidence-out") {
        write_json(Path::new(path), &value)?;
    }
    Ok(value)
}
