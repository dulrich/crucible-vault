//! `cv` binary: logging + CLI dispatch. `cv daemon` builds its own tokio runtime (see
//! `cv::cli`).

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(cv::cli::run(&args));
}
