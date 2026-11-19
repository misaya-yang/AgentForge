#[tokio::main]
async fn main(){match ai_platform_eval::resource_profile::main().await{Ok(c)=>std::process::exit(c),Err(_)=>{eprintln!("PPR-00 infrastructure error: check options and runtime configuration");std::process::exit(2)}}}
