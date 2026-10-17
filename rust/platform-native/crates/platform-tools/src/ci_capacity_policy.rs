//! Shared, dependency-free policy used before Cargo is built and by the hosted gate.
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
pub struct Capacity {
    pub hosted: bool,
    pub cpus: usize,
    pub memory_mib: u64,
    pub build_jobs: usize,
    pub test_threads: usize,
}

pub fn plan(hosted: bool, cpus: usize, memory_mib: u64) -> Result<Capacity, String> {
    if cpus == 0 || (hosted && memory_mib < 4096) {
        return Err("invalid CPU count or hosted runner has less than 4 GiB memory".into());
    }
    // Leave 2 GiB for the runner/linker; budget 3 GiB per Rust compilation job.
    let build_jobs = if hosted {
        cpus.min(4)
            .min(((memory_mib - 2048) / 3072) as usize)
            .max(1)
    } else {
        1
    };
    Ok(Capacity {
        hosted,
        cpus,
        memory_mib,
        build_jobs,
        test_threads: build_jobs.min(2),
    })
}

pub fn detect_from(
    env: &BTreeMap<String, String>,
    cpus: usize,
    meminfo: &str,
) -> Result<Capacity, String> {
    let is_true = |name: &str| {
        env.get(name)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    };
    // Local/self-hosted builds never inherit the public runner allowance.
    let hosted = is_true("CI")
        && is_true("GITHUB_ACTIONS")
        && env.get("RUNNER_ENVIRONMENT").map(String::as_str) == Some("github-hosted")
        && env.get("RUNNER_OS").map(String::as_str) == Some("Linux")
        && env
            .get("AI_PLATFORM_CI_CAPACITY_PROFILE")
            .map(String::as_str)
            == Some("hosted");
    let memory_mib = meminfo
        .lines()
        .find_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() == 3 && fields[0] == "MemTotal:" && fields[2] == "kB")
                .then(|| fields[1].parse::<u64>().ok().map(|kb| kb / 1024))
                .flatten()
        })
        .unwrap_or(0);
    plan(hosted, cpus, memory_mib)
}

pub fn detect() -> Result<Capacity, String> {
    detect_from(
        &std::env::vars().collect(),
        std::thread::available_parallelism()
            .map_err(|e| e.to_string())?
            .get(),
        &std::fs::read_to_string("/proc/meminfo").unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_cpu_memory_and_test_parallelism() {
        assert_eq!(plan(true, 4, 16384).unwrap().build_jobs, 4);
        assert_eq!(plan(true, 2, 7168).unwrap().build_jobs, 1);
        assert_eq!(plan(true, 64, 8192).unwrap().build_jobs, 2);
        assert_eq!(plan(true, 64, 262144).unwrap().build_jobs, 4);
        assert_eq!(plan(true, 64, 262144).unwrap().test_threads, 2);
        assert_eq!(plan(false, 64, 262144).unwrap().build_jobs, 1);
        assert!(plan(true, 0, 16384).is_err());
        assert!(plan(true, 4, 4095).is_err());
    }

    #[test]
    fn requires_all_standard_hosted_opt_in_markers() {
        let env: BTreeMap<String, String> = [
            ("CI", "true"),
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
            ("RUNNER_OS", "Linux"),
            ("AI_PLATFORM_CI_CAPACITY_PROFILE", "hosted"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let mem = "MemTotal: 16777216 kB\n";
        assert_eq!(detect_from(&env, 4, mem).unwrap().build_jobs, 4);
        for key in env.keys() {
            let mut missing = env.clone();
            missing.remove(key);
            assert_eq!(
                detect_from(&missing, 4, mem).unwrap().build_jobs,
                1,
                "{key}"
            );
        }
        assert!(detect_from(&env, 4, "MemTotal: invalid kB").is_err());
        let mut self_hosted = env;
        self_hosted.insert("RUNNER_ENVIRONMENT".into(), "self-hosted".into());
        assert_eq!(detect_from(&self_hosted, 4, mem).unwrap().build_jobs, 1);
    }
}
