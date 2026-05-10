//! Q5: TelemetryServerRoute can serve a binary `application/octet-stream` body.
//!
//! "Can `TelemetryServerRoute` serve a binary `application/octet-stream` body
//!  (gzipped pprof), not just text/json?"
//!
//! Approach: register a custom route that returns 256 binary bytes via
//! foundations' init flow, hit it with a hand-rolled HTTP/1.1 client over TCP,
//! confirm the body round-trips byte-for-byte and the content-type is honoured.

use foundations::addr::ListenAddr;
use foundations::service_info;
use foundations::telemetry::reexports::http_body_util::{BodyExt, Full};
use foundations::telemetry::reexports::hyper::body::Bytes;
use foundations::telemetry::reexports::hyper::{Method, Response, StatusCode, header};
use foundations::telemetry::settings::TelemetrySettings;
use foundations::telemetry::{
    TelemetryConfig, TelemetryRouteBody, TelemetryServerRoute, init,
};
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 256 sequential bytes — every value 0x00..=0xFF, the bytes most likely
/// to expose accidental text-mode mangling.
const BODY: [u8; 256] = {
    let mut b = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        b[i] = i as u8;
        i += 1;
    }
    b
};

const CONTENT_TYPE: &str = "application/octet-stream";

pub async fn run() {
    println!("Q5: TelemetryServerRoute serves a binary body");
    println!("---------------------------------------------");

    let route = TelemetryServerRoute {
        path: "/binary".into(),
        methods: vec![Method::GET],
        handler: Box::new(|_, _| {
            Box::pin(async move {
                let resp: Result<Response<TelemetryRouteBody>, Infallible> = Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, CONTENT_TYPE)
                    .body(
                        Full::from(Bytes::from_static(&BODY))
                            .map_err(Into::into)
                            .boxed(),
                    )
                    .unwrap());
                resp
            })
        }),
    };

    let mut settings = TelemetrySettings::default();
    settings.server.enabled = true;
    // bind to an ephemeral port; we'll read the assigned port back from the driver.
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    settings.server.addr = ListenAddr::from(bind);

    let mut driver = init(TelemetryConfig {
        service_info: &service_info!(),
        settings: &settings,
        custom_server_routes: vec![route],
    })
    .expect("foundations::telemetry::init failed");

    let addr = driver
        .server_addr()
        .expect("server should be enabled")
        .clone();
    println!("server bound at {addr:?}");

    // Drive the server in the background. We give it a graceful-shutdown signal
    // so the spike binary actually exits.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    driver.with_graceful_shutdown(async move {
        let _ = shutdown_rx.await;
    });
    let driver_task = tokio::spawn(driver);

    // Tiny moment for the listener to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Hand-rolled HTTP/1.1 GET. Avoids pulling another HTTP client crate into the spike.
    let socket: SocketAddr = match &addr {
        ListenAddr::Tcp(sa) => *sa,
        _ => panic!("expected TCP address"),
    };
    let mut conn = TcpStream::connect(socket).await.expect("connect");
    conn.write_all(b"GET /binary HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write GET");

    let mut raw = Vec::with_capacity(2048);
    conn.read_to_end(&mut raw).await.expect("read response");

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response missing CRLF*2");
    let head = &raw[..split];
    let body = &raw[split + 4..];

    let head_str = std::str::from_utf8(head).expect("headers should be utf-8");
    let status_ok = head_str.starts_with("HTTP/1.1 200");
    let content_type_ok = head_str
        .lines()
        .any(|l| l.eq_ignore_ascii_case(&format!("content-type: {CONTENT_TYPE}")));
    let body_ok = body == BODY;

    println!("status 200 OK:           {status_ok}");
    println!("content-type honoured:   {content_type_ok}");
    println!("binary body round-trip:  {body_ok} ({} bytes)", body.len());

    let _ = shutdown_tx.send(());
    let _ = driver_task.await;

    println!();
    if status_ok && content_type_ok && body_ok {
        println!(
            "VERDICT: yes — TelemetryServerRoute serves arbitrary binary bodies. \
             TelemetryRouteBody = BoxBody<Bytes, Error>; the built-in `/pprof/heap` route \
             already does the same with `application/x-gperftools-profile`. \
             NOTE: routes are registered at init() time via TelemetryConfig::custom_server_routes \
             — there is no runtime `add_route` API as the plan example suggests; amend the \
             plan's wiring example to use `custom_server_routes`."
        );
    } else {
        println!("UNEXPECTED: binary body did not round-trip cleanly. Investigate before Phase 1.");
    }
}
