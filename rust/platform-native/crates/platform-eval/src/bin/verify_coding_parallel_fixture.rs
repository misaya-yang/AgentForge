#[tokio::main]
async fn main(){match ai_platform_eval::coding_sandbox::cli(std::env::args().skip(1)).await{Ok((summary,code))=>{println!("{summary}");std::process::exit(code)},Err(error)=>{eprintln!("coding fixture failed closed: {error}");std::process::exit(2)}}}
