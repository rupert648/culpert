//! [`pprof_route`] — builder for the `TelemetryServerRoute` that serves
//! culpert's gzipped pprof profile.

use foundations::telemetry::reexports::http_body_util::{BodyExt, Full};
use foundations::telemetry::reexports::hyper::body::Bytes;
use foundations::telemetry::reexports::hyper::{header, Method, Response, StatusCode};
use foundations::telemetry::TelemetryServerRoute;
use std::convert::Infallible;

/// Build a [`TelemetryServerRoute`] that serves a gzipped pprof profile of
/// culpert's accumulated allocation samples.
///
/// Pass into `foundations::telemetry::TelemetryConfig::custom_server_routes`
/// at init time. Stock `pprof` reads it directly:
///
/// ```text
/// curl -o prof.pb.gz http://localhost:8080/debug/alloc/profile
/// pprof -http=:8081 prof.pb.gz
/// ```
///
/// The handler calls [`culpert::snapshot`] each time it's hit. Snapshots are
/// destructive (they drain the per-thread sample buffers), so back-to-back
/// requests will produce empty responses if no allocations happened in between.
///
/// Content-Type is `application/x-gperftools-profile`, matching foundations'
/// own built-in `/pprof/heap` route convention.
pub fn pprof_route(path: &str) -> TelemetryServerRoute {
    let path = path.to_owned();
    TelemetryServerRoute {
        path,
        methods: vec![Method::GET],
        handler: Box::new(|_req, _settings| {
            Box::pin(async move {
                let profile = culpert::snapshot();
                let resp: Result<_, Infallible> = match culpert::pprof::encode_gzipped(&profile) {
                    Ok(bytes) => Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "application/x-gperftools-profile")
                        .body(Full::from(Bytes::from(bytes)).map_err(Into::into).boxed())
                        .unwrap()),
                    Err(e) => Ok(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                        .body(
                            Full::from(Bytes::from(format!("pprof encode error: {e}")))
                                .map_err(Into::into)
                                .boxed(),
                        )
                        .unwrap()),
                };
                resp
            })
        }),
    }
}
