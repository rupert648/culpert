//! mock-axum — a synthetic axum service that uses foundations for
//! telemetry and culpert for per-span allocation profiling.
//!
//! Five routes with deliberately distinct allocation patterns. Hit them with
//! `examples/mock-axum/load.sh`, then pull a profile from the telemetry
//! server:
//!
//! ```text
//! cargo run -p mock-axum
//! # in another shell:
//! ./examples/mock-axum/load.sh
//! curl -o /tmp/mock.pb.gz http://127.0.0.1:8081/debug/alloc/profile
//! pprof -tags /tmp/mock.pb.gz
//! pprof -http=:8090 /tmp/mock.pb.gz
//! ```
//!
//! Routes:
//! - `GET /cheap`   — minimal allocation baseline (returns "ok")
//! - `GET /json`    — heavy serde_json allocation (nested object construction)
//! - `GET /strings` — `format!` and `String::push_str` heavy
//! - `GET /vec`     — large `Vec` allocations
//! - `GET /nested`  — calls multiple `#[span_fn]` sub-spans with mixed allocs

use axum::{Json, Router, routing::get};
use culpert::TrackingAllocator;
use foundations::addr::ListenAddr;
use foundations::service_info;
use foundations::telemetry::settings::TelemetrySettings;
use foundations::telemetry::tracing::span_fn;
use foundations::telemetry::{TelemetryConfig, init};
use serde::Serialize;
use std::alloc::System;
use std::net::SocketAddr;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

const APP_PORT: u16 = 8080;
const TELEMETRY_PORT: u16 = 8081;

#[tokio::main]
async fn main() {
    // ----- foundations init ----------------------------------------------

    let mut settings = TelemetrySettings::default();
    // Default tracing settings already enable Active(1.0) sampling, which is
    // what culpert-foundations needs for span attribution to work.
    // (See notes.md Q2 — the unsampled / inactive path is gated out.)
    settings.server.enabled = true;
    let bind: SocketAddr = format!("127.0.0.1:{TELEMETRY_PORT}").parse().unwrap();
    settings.server.addr = ListenAddr::from(bind);

    let driver = init(TelemetryConfig {
        service_info: &service_info!(),
        settings: &settings,
        custom_server_routes: vec![culpert_foundations::pprof_route("/debug/alloc/profile")],
    })
    .expect("foundations::telemetry::init failed");

    // Tighter sample rate than the 512 KiB default so the smaller-alloc
    // spans (parse_payload, build_response, etc.) show up in the demo.
    // Counter-mod sampling biases against small-but-frequent allocations
    // when a heavy allocator (here `/vec`) is in the same workload — at
    // 512 KiB the heavy allocator consumes most of the sample budget.
    culpert_foundations::install_with_config(culpert::Config {
        rate_bytes: 4 * 1024,
        stack_depth: 32,
        buffer_capacity: 1 << 16,
    });

    let telemetry_addr = driver
        .server_addr()
        .expect("telemetry server enabled")
        .clone();

    tokio::spawn(async move {
        if let Err(e) = driver.await {
            eprintln!("telemetry driver error: {e}");
        }
    });

    // ----- axum app ------------------------------------------------------

    let app = Router::new()
        .route("/cheap", get(cheap))
        .route("/json", get(json_route))
        .route("/strings", get(strings_route))
        .route("/vec", get(vec_route))
        .route("/nested", get(nested_route));

    let app_addr: SocketAddr = format!("127.0.0.1:{APP_PORT}").parse().unwrap();
    let listener = tokio::net::TcpListener::bind(app_addr)
        .await
        .expect("bind axum listener");

    eprintln!("mock-axum listening on http://{app_addr}");
    eprintln!("telemetry endpoints on   {telemetry_addr:?}");
    eprintln!("  pprof: http://127.0.0.1:{TELEMETRY_PORT}/debug/alloc/profile");

    // Race the axum server against ctrl-c. Two things make graceful shutdown
    // unfit for a demo binary:
    //
    // 1. axum::serve(...).with_graceful_shutdown(...) waits for *all* in-flight
    //    HTTP connections to close politely. The load.sh client uses HTTP/1.1
    //    keep-alive, so its sockets stay open long after the load is done —
    //    axum hangs waiting for them.
    // 2. The foundations telemetry driver is in a `tokio::spawn`'d background
    //    task; when main's body completes, the runtime would still wait on it
    //    indefinitely.
    //
    // For a demo, the right thing is to terminate immediately on ctrl-c.
    // A real production wiring would coordinate driver shutdown via
    // `driver.with_graceful_shutdown(signal)` and return cleanly.
    tokio::select! {
        res = axum::serve(listener, app) => {
            res.expect("axum serve");
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("shutting down");
            std::process::exit(0);
        }
    }
}

// --- routes -------------------------------------------------------------

#[span_fn("cheap")]
async fn cheap() -> &'static str {
    "ok"
}

#[derive(Serialize)]
struct Record {
    id: u64,
    name: String,
    values: Vec<i32>,
}

#[span_fn("json")]
async fn json_route() -> Json<Vec<Record>> {
    let records: Vec<Record> = (0..200)
        .map(|i| Record {
            id: i,
            name: format!("record-{i:04}"),
            values: (0..32).collect(),
        })
        .collect();
    Json(records)
}

#[span_fn("strings")]
async fn strings_route() -> String {
    let mut buf = String::with_capacity(1024);
    for i in 0..500 {
        buf.push_str(&format!("line {i:04}: {}\n", "x".repeat(40)));
    }
    buf
}

#[span_fn("vec")]
async fn vec_route() -> String {
    // A few medium-large Vec allocations.
    let mut total: u64 = 0;
    for _ in 0..20 {
        let v: Vec<u64> = (0..50_000u64).collect();
        total = total.wrapping_add(v.iter().sum());
    }
    format!("sum={total}")
}

#[span_fn("nested")]
async fn nested_route() -> String {
    let parsed = parse_payload();
    let validated = validate_payload(&parsed);
    
    build_response(validated)
}

#[span_fn("parse_payload")]
fn parse_payload() -> Vec<String> {
    (0..100).map(|i| format!("field_{i}")).collect()
}

#[span_fn("validate_payload")]
fn validate_payload(parsed: &[String]) -> Vec<String> {
    parsed.iter().map(|s| s.to_uppercase()).collect()
}

#[span_fn("build_response")]
fn build_response(validated: Vec<String>) -> String {
    let mut s = String::with_capacity(1024);
    for v in validated {
        s.push_str(&v);
        s.push('\n');
    }
    s
}
