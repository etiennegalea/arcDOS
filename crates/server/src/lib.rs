//! The ArcDOS HTTP application shared by the server binary and integration tests.

use std::{path::Path, sync::Arc};

use arcdos_protocol::{
    connect::arcdos::account::v1::{HEALTH_SERVICE_CHECK_SPEC, HealthService},
    proto::arcdos::account::v1::{CheckRequest, CheckResponse},
};
use connectrpc::{Limits, RequestContext, Response, ServiceRequest, ServiceResult};
use tower_http::services::ServeDir;

#[cfg(target_os = "linux")]
mod auth;
#[cfg(target_os = "linux")]
mod files;

#[cfg(target_os = "linux")]
pub use auth::AuthentikConfig;

#[cfg(target_os = "linux")]
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Clear derived search rows and persist jobs to rebuild them on the next startup.
#[cfg(target_os = "linux")]
pub fn rebuild_search(data_dir: &Path) -> Result<(), BoxError> {
    Ok(files::rebuild_search(data_dir)?)
}

/// Enable the Linux file and transfer routes with a separate local development token.
#[cfg(target_os = "linux")]
pub fn app_with_files(
    web_dir: &Path,
    data_dir: &Path,
    token_file: &Path,
) -> Result<axum::Router, BoxError> {
    let access = auth::Access::development(web_dir, data_dir, token_file)?;
    Ok(base_app(web_dir).merge(auth::router(files::router(web_dir, data_dir)?, access)))
}

/// Enable the Linux file API with Authentik as the identity provider.
#[cfg(target_os = "linux")]
pub async fn app_with_authentik(
    web_dir: &Path,
    data_dir: &Path,
    config: AuthentikConfig,
) -> Result<axum::Router, BoxError> {
    let provider = config.discover(web_dir, data_dir).await?;
    let files = files::router(web_dir, data_dir)?;
    let access = auth::Access::oidc(provider, data_dir)?;
    Ok(base_app(web_dir).merge(auth::router(files, access)))
}

/// Mount the Connect API and serve the static SvelteKit build from the same origin.
pub fn app(web_dir: &Path) -> axum::Router {
    base_app(web_dir).route(
        "/auth/session",
        axum::routing::get(|| async {
            (
                [
                    (axum::http::header::CONTENT_TYPE, "application/json"),
                    (axum::http::header::CACHE_CONTROL, "no-store"),
                ],
                r#"{"mode":"disabled"}"#,
            )
        }),
    )
}

fn base_app(web_dir: &Path) -> axum::Router {
    let procedure = HEALTH_SERVICE_CHECK_SPEC.procedure;
    let health_limits = Limits::default()
        .with_max_request_body_size(4096)
        .with_max_message_size(4096)
        .with_element_memory_limit(16384);
    let connect = connectrpc::Router::new()
        .add_service(Arc::new(Health))
        .with_route_limits(procedure, health_limits)
        .into_axum_service();

    axum::Router::new()
        .route_service(procedure, connect)
        .fallback_service(ServeDir::new(web_dir))
}

struct Health;

#[allow(refining_impl_trait)]
impl HealthService for Health {
    async fn check(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, CheckRequest>,
    ) -> ServiceResult<CheckResponse> {
        Response::ok(CheckResponse {
            ready: true,
            ..Default::default()
        })
    }
}
