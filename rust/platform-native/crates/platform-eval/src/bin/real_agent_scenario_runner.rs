#[tokio::main]
async fn main(){if let Err(error)=ai_platform_eval::scenario_runner::cli(&std::env::args().skip(1).collect::<Vec<_>>()).await{eprintln!("real scenario runner failed closed: {error}");std::process::exit(2)}}
