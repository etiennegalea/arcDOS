use std::{env, error::Error, future::Future, io, net::SocketAddr, path::PathBuf};

use tokio::{net::TcpListener, signal};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt().json().init();

    let bind = match env::var("ARCDOS_BIND") {
        Ok(value) => value.parse::<SocketAddr>()?,
        Err(env::VarError::NotPresent) => SocketAddr::from(([127, 0, 0, 1], 8080)),
        Err(error) => return Err(error.into()),
    };
    let web_dir = env::var_os("ARCDOS_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("apps/web/build"));
    let listener = TcpListener::bind(bind).await?;
    let shutdown = shutdown_signal()?;

    tracing::info!(address = %listener.local_addr()?, "ArcDOS listening");
    axum::serve(listener, arcdos_server::app(&web_dir))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

fn shutdown_signal() -> io::Result<impl Future<Output = ()>> {
    #[cfg(unix)]
    let mut terminate = signal::unix::signal(signal::unix::SignalKind::terminate())?;

    Ok(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = ctrl_c() => {},
            _ = terminate.recv() => {},
        }

        #[cfg(not(unix))]
        ctrl_c().await;

        tracing::info!("ArcDOS shutting down");
    })
}

async fn ctrl_c() {
    if let Err(error) = signal::ctrl_c().await {
        tracing::error!(%error, "Could not wait for Ctrl+C");
    }
}
