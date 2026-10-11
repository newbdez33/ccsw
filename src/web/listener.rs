//! Shared listener setup and shutdown for the CLI and TUI.

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpListener;

use super::Server;
use super::config::{Config, validate_bind};
use crate::paths::Paths;

pub async fn bind(
    paths: Paths,
    address: SocketAddr,
    external_origin: Option<&str>,
    read_only: bool,
) -> anyhow::Result<(Server, TcpListener)> {
    validate_bind(address.ip()).await?;
    let listener = TcpListener::bind(address).await?;
    let config = Config::new(listener.local_addr()?, external_origin, read_only)?;
    Ok((Server::new(paths, config), listener))
}

pub async fn serve(
    server: &Server,
    listener: TcpListener,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let ip = listener.local_addr()?.ip();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let stop = server.shutdown_handle();
    let serving = axum::serve(listener, server.router())
        .with_graceful_shutdown(async move {
            let _ = stop_rx.await;
            stop();
        })
        .into_future();
    tokio::pin!(serving);
    let result = tokio::select! {
        result = &mut serving => return Ok(result?),
        () = shutdown => Ok(()),
        result = async {
            if ip.is_loopback() {
                std::future::pending::<()>().await;
            }
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if validate_bind(ip).await.is_err() {
                    anyhow::bail!("The Tailscale address can no longer be verified. The console stopped.");
                }
            }
        } => result,
    };
    let _ = stop_tx.send(());
    serving.await?;
    result
}
