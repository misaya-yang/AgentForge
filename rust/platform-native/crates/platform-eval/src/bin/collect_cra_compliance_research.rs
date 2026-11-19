#[tokio::main]async fn main(){match ai_platform_eval::cra::main().await{Ok(c)=>std::process::exit(c),Err(e)=>{eprintln!("CRA fixture command failed closed: {e}");std::process::exit(1)}}}
