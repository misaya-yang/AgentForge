//! rustc-only bootstrap: the same policy as platform-tools, without compiling its dependencies.
mod ci_capacity_policy;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let capacity = ci_capacity_policy::detect()?;
    match std::env::args().nth(1).as_deref() {
        Some("--build-jobs") => {
            println!("{}", capacity.build_jobs);
            return Ok(());
        }
        None => {}
        Some(_) => return Err("usage: ci-capacity [--build-jobs]".into()),
    }
    println!("CARGO_BUILD_JOBS={}", capacity.build_jobs);
    println!("NEXTEST_TEST_THREADS={}", capacity.test_threads);
    Ok(())
}
