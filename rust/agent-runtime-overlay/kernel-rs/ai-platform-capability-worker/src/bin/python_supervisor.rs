fn main() {
    if ai_platform_capability_worker::python_code_execution::supervisor_main().is_err() {
        std::process::exit(1);
    }
}
