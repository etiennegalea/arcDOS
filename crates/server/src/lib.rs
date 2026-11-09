//! The ArcDOS HTTP application shared by the server binary and integration tests.

use std::{path::Path, sync::Arc};

use arcdos_protocol::{
    connect::arcdos::account::v1::{HEALTH_SERVICE_CHECK_SPEC, HealthService},
    proto::arcdos::account::v1::{CheckRequest, CheckResponse},
};
use connectrpc::{Limits, RequestContext, Response, ServiceRequest, ServiceResult};
use tower_http::services::ServeDir;

/// Mount the Connect API and serve the static SvelteKit build from the same origin.
pub fn app(web_dir: &Path) -> axum::Router {
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
