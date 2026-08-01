use atlas_web::{RunOptions, run};

#[tokio::main]
async fn main() {
    if let Err(error) = run(RunOptions::from_env()).await {
        eprintln!("Atlas Reader could not start: {error}");
        std::process::exit(1);
    }
}
