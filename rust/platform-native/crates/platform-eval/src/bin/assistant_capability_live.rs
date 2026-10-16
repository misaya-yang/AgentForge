fn main(){match ai_platform_eval::assistant_capability::main(){Ok(c)=>std::process::exit(c),Err(e)=>{eprintln!("assistant_capability_live failed: {e}");std::process::exit(1)}}}
