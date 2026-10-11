use std::net::SocketAddr;

use clap::Parser;

use crate::paths::Paths;
use crate::web::listener;

#[derive(Parser)]
#[command(
    name = "ccsw serve",
    no_binary_name = true,
    about = "Serve a paired remote account console."
)]
struct Args {
    #[arg(
        long,
        default_value = "127.0.0.1:3000",
        help = "Loopback or this host's Tailscale IP and port"
    )]
    bind: SocketAddr,
    #[arg(long, help = "Disable account switches and manual refresh requests")]
    read_only: bool,
    #[arg(
        long,
        help = "Exact HTTPS origin when a loopback listener is behind Tailscale Serve"
    )]
    external_origin: Option<String>,
}

pub fn run(argv: Vec<String>) -> i32 {
    let args = match Args::try_parse_from(argv) {
        Ok(args) => args,
        Err(error) => {
            let status = error.exit_code();
            let _ = error.print();
            return status;
        }
    };
    if let Some(status) = super::root_guard() {
        return status;
    }
    let result = (|| -> anyhow::Result<()> {
        let paths = Paths::from_env()?;
        crate::logging::init(&paths, false);
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?
            .block_on(serve(args, paths))
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error}");
            1
        }
    }
}

async fn serve(args: Args, paths: Paths) -> anyhow::Result<()> {
    let (server, socket) = listener::bind(
        paths,
        args.bind,
        args.external_origin.as_deref(),
        args.read_only,
    )
    .await?;
    println!("Console URL: {}/", server.origin());
    println!("Pairing code: {}", server.pairing_code());
    println!(
        "The code expires in five minutes and can be used once. Press Enter for a new code; Ctrl-C stops the server."
    );
    if args.read_only {
        println!("Access: read-only");
    }
    server.read_pairing_input();
    let result = listener::serve(&server, socket, shutdown_signal()).await;
    server.stop();
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
