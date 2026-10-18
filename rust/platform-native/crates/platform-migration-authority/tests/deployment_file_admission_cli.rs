//! Argument refusals must happen before source loading, secret reads or connection.
use std::process::Command;

#[test]
fn deployment_read_gate_refuses_mutating_admin_and_ambiguous_phase_arguments() {
    for args in [
        vec!["deployment-file-admission"],
        vec![
            "deployment-file-admission",
            "--expected-database",
            "gateway",
            "--phase",
            "bootstrap",
        ],
        vec![
            "deployment-file-admission",
            "--expected-database",
            "gateway",
            "--phase",
            "pre-migration",
            "--phase",
            "post-migration",
        ],
        vec![
            "deployment-file-admission",
            "--expected-database",
            "gateway",
            "--principal",
            "migrator",
        ],
        vec![
            "deployment-file-admission",
            "--expected-database",
            "gateway",
            "--expected-admin",
            "postgres",
        ],
        vec![
            "deployment-file-admission",
            "--expected-database",
            "gateway",
            "--no-adoption",
        ],
        vec![
            "deployment-file-admission",
            "--expected-database",
            "gateway",
            "--reconciliation-evidence-out",
            "/must-not-write",
        ],
        vec!["startup-check", "--phase", "post-migration"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_database-authority"))
            .args(args)
            .env_clear()
            .env(
                "AI_GATEWAY_DATABASE_ADMIN_DSN_FILE",
                "/must-not-read/private-admin-sentinel",
            )
            .env("AI_GATEWAY_DATABASE_DIR", "/must-not-read/source-sentinel")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!error.contains("private-admin-sentinel"));
        assert!(!error.contains("source-sentinel"));
        assert!(
            !error.contains("artifact could not be read"),
            "arguments must fail before source access"
        );
    }
}
