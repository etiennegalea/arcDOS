use std::{
    env,
    error::Error,
    future::Future,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use tokio::{net::TcpListener, signal};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt().json().init();

    let mut arguments = env::args_os().skip(1);
    if let Some(command) = arguments.next() {
        if command != "rebuild-search" || arguments.next().is_some() {
            return Err(io::Error::other("expected no arguments or rebuild-search").into());
        }
        return rebuild_search();
    }

    let bind = match env::var("ARCDOS_BIND") {
        Ok(value) => value.parse::<SocketAddr>()?,
        Err(env::VarError::NotPresent) => SocketAddr::from(([127, 0, 0, 1], 8080)),
        Err(error) => return Err(error.into()),
    };
    let web_dir = env::var_os("ARCDOS_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("apps/web/build"));
    let app = configured_app(&web_dir).await?;
    let listener = TcpListener::bind(bind).await?;
    let shutdown = shutdown_signal()?;

    tracing::info!(address = %listener.local_addr()?, "ArcDOS listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn rebuild_search() -> Result<(), Box<dyn Error + Send + Sync>> {
    let data = env::var_os("ARCDOS_DATA_DIR")
        .ok_or_else(|| io::Error::other("ARCDOS_DATA_DIR must be set"))?;
    arcdos_server::rebuild_search(Path::new(&data))?;
    tracing::info!("Search index rebuild scheduled");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn rebuild_search() -> Result<(), Box<dyn Error + Send + Sync>> {
    Err(io::Error::other("search indexing requires Linux").into())
}

async fn configured_app(web: &Path) -> Result<axum::Router, Box<dyn Error + Send + Sync>> {
    const OIDC_SETTINGS: &[&str] = &[
        "ARCDOS_PUBLIC_ORIGIN",
        "ARCDOS_OIDC_ISSUER",
        "ARCDOS_OIDC_CLIENT_ID",
        "ARCDOS_OIDC_CLIENT_SECRET_FILE",
        "ARCDOS_ANDROID_ISSUER",
        "ARCDOS_ANDROID_CLIENT_ID",
    ];
    let data = env::var_os("ARCDOS_DATA_DIR");
    let dev = env::var_os("ARCDOS_DEV_TOKEN_FILE");
    let oidc = OIDC_SETTINGS.iter().any(|name| env::var_os(name).is_some());
    match (data, dev, oidc) {
        (None, None, false) => Ok(arcdos_server::app(web)),
        (Some(_), Some(_), true) => {
            Err(io::Error::other("development access and Authentik are mutually exclusive").into())
        }
        #[cfg(target_os = "linux")]
        (Some(data), Some(token), false) => {
            arcdos_server::app_with_files(web, Path::new(&data), Path::new(&token))
        }
        #[cfg(target_os = "linux")]
        (Some(data), None, true) => {
            arcdos_server::app_with_authentik(web, Path::new(&data), authentik_config()?).await
        }
        #[cfg(not(target_os = "linux"))]
        (Some(_), Some(_), false) | (Some(_), None, true) => {
            Err(io::Error::other("the file API requires Linux").into())
        }
        _ => Err(io::Error::other(
            "the file API requires ARCDOS_DATA_DIR and one complete access configuration",
        )
        .into()),
    }
}

#[cfg(target_os = "linux")]
fn authentik_config() -> io::Result<arcdos_server::AuthentikConfig> {
    use arcdos_auth::AndroidClientConfig;

    fn required(name: &str) -> io::Result<String> {
        env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| io::Error::other(format!("{name} must be set")))
    }

    let android = if env::var_os("ARCDOS_ANDROID_ISSUER").is_some()
        || env::var_os("ARCDOS_ANDROID_CLIENT_ID").is_some()
    {
        Some(AndroidClientConfig {
            issuer: required("ARCDOS_ANDROID_ISSUER")?,
            client_id: required("ARCDOS_ANDROID_CLIENT_ID")?,
        })
    } else {
        None
    };
    Ok(arcdos_server::AuthentikConfig {
        issuer: required("ARCDOS_OIDC_ISSUER")?,
        public_origin: required("ARCDOS_PUBLIC_ORIGIN")?,
        client_id: required("ARCDOS_OIDC_CLIENT_ID")?,
        client_secret_file: env::var_os("ARCDOS_OIDC_CLIENT_SECRET_FILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("ARCDOS_OIDC_CLIENT_SECRET_FILE must be set"))?,
        android,
    })
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
