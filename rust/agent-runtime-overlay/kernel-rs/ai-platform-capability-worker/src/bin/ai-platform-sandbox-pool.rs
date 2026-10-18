//! Separate controller trust domain; never shares a process with user code.
fn main()->std::process::ExitCode {
    let result=(||->Result<(),&'static str>{
        let path=std::env::args_os().nth(1).ok_or("pool_config_required")?;
        let bytes=std::fs::read(path).map_err(|_|"pool_config_unavailable")?;
        if bytes.len()>32768{return Err("pool_config_too_large")}
        let config=serde_json::from_slice(&bytes).map_err(|_|"pool_config_invalid")?;
        tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|_|"pool_runtime_unavailable")?.block_on(ai_platform_capability_worker::sandbox_pool::serve(config))
    })();
    if let Err(error)=result{eprintln!("{error}");std::process::ExitCode::FAILURE}else{std::process::ExitCode::SUCCESS}
}
