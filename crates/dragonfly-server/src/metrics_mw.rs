// Dragonfly
// Copyright (C) Riff Labs Limited <team@riff.cc>
//
// Prometheus `/metrics` endpoint + HTTP request middleware.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Prometheus metrics for the HTTP serving pipeline.
//!
//! Exposes an in-flight request gauge, a per-endpoint request counter, and a
//! per-endpoint latency histogram at `GET /metrics` (text/plain; version=0.0.4)
//! so the serving pipeline — the surface that starves under concurrent PXE
//! imaging — can be scraped and bisected.
//!
//! Endpoint labels are a coarse, *bounded* classification of the request path
//! ([`classify`]). The raw path (e.g. `/boot/{mac}`) is high-cardinality; the
//! label groups by logical serve surface so the latency histogram stays
//! meaningful under load.

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use metrics_exporter_prometheus::{BuildError, PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;
use std::time::Instant;

static METRICS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the Prometheus recorder as the process-global default and cache its
/// render handle. Call once at server start; a second call leaves the cached
/// handle in place (the recorder itself is process-global and rejects re-install
/// by returning an error, which we ignore after the first success).
pub fn install() -> Result<(), BuildError> {
    let handle = PrometheusBuilder::new().install_recorder()?;
    let _ = METRICS_HANDLE.set(handle);
    Ok(())
}

/// Map a request path to a bounded endpoint label.
///
/// Coarse on purpose: `/boot/{mac}`, `/boot/{arch}/{asset}`, and the static
/// boot blobs all collapse to `boot`; the big OS image (`/images/...`) and the
/// JIT-converted `/os/...` are distinguished because they are the bandwidth- and
/// CPU-bound surfaces. This keeps label cardinality small (a handful) regardless
/// of how many distinct MACs/IPs boot.
fn classify(path: &str) -> &'static str {
    if path.starts_with("/images/") {
        "image"
    } else if path.starts_with("/os/") {
        "os_image"
    } else if path.starts_with("/boot-debian/") {
        "boot_debian_asset"
    } else if path.starts_with("/boot/pxelinux.cfg") {
        "pxelinux_config"
    } else if path.starts_with("/boot/") {
        "boot"
    } else if path.starts_with("/ipxe/") {
        "ipxe_artifact"
    } else if path.starts_with("/api/") {
        "api"
    } else if path == "/metrics" || path == "/favicon.ico" {
        "infra"
    } else {
        "other"
    }
}

/// Map an HTTP method to a bounded `&'static str` label (avoids borrowing the
/// request method into the metrics label macros).
fn method_label(method: &axum::http::Method) -> &'static str {
    match *method {
        axum::http::Method::GET => "GET",
        axum::http::Method::POST => "POST",
        axum::http::Method::PUT => "PUT",
        axum::http::Method::DELETE => "DELETE",
        axum::http::Method::PATCH => "PATCH",
        axum::http::Method::HEAD => "HEAD",
        axum::http::Method::OPTIONS => "OPTIONS",
        _ => "other",
    }
}

/// axum middleware: count requests, time them, and track in-flight, all labelled
/// by the bounded endpoint classification.
pub async fn middleware(req: Request, next: Next) -> Response {
    let endpoint = classify(req.uri().path());
    let method = method_label(req.method());
    metrics::gauge!("http_requests_in_flight", "endpoint" => endpoint).increment(1.0);
    let start = Instant::now();
    let resp = next.run(req).await;
    metrics::gauge!("http_requests_in_flight", "endpoint" => endpoint).decrement(1.0);
    let status = resp.status().as_u16();
    metrics::counter!(
        "http_requests_total",
        "endpoint" => endpoint,
        "method" => method,
        "status" => status.to_string()
    )
    .increment(1);
    metrics::histogram!("http_request_duration_seconds", "endpoint" => endpoint)
        .record(start.elapsed().as_secs_f64());
    resp
}

/// `GET /metrics` — render recorded metrics in Prometheus text format.
pub async fn serve_metrics() -> Response {
    let body = METRICS_HANDLE
        .get()
        .map(PrometheusHandle::render)
        .unwrap_or_default();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_routes_to_bounded_labels() {
        assert_eq!(classify("/images/debian-13-amd64.raw"), "image");
        assert_eq!(classify("/os/debian-13/amd64"), "os_image");
        assert_eq!(classify("/boot-debian/amd64/vmlinuz"), "boot_debian_asset");
        assert_eq!(classify("/boot/aa:bb:cc:dd:ee:ff"), "boot");
        assert_eq!(classify("/boot/amd64/initrd.img"), "boot");
        assert_eq!(classify("/boot/pxelinux.cfg/default"), "pxelinux_config");
        assert_eq!(classify("/ipxe/debian/13"), "ipxe_artifact");
        assert_eq!(classify("/api/machines"), "api");
        assert_eq!(classify("/metrics"), "infra");
        assert_eq!(classify("/favicon.ico"), "infra");
        assert_eq!(classify("/something/else"), "other");
    }

    #[test]
    fn classify_is_bounded_under_high_cardinality_inputs() {
        // Distinct MACs must NOT each become a distinct label.
        let mut labels = std::collections::HashSet::new();
        for i in 0..1000 {
            labels.insert(classify(&format!("/boot/aa:bb:cc:dd:ee:{i:02x}")));
        }
        assert_eq!(labels.len(), 1);
        assert_eq!(labels.into_iter().next().unwrap(), "boot");
    }

    #[tokio::test]
    async fn metrics_endpoint_reports_recorded_requests() {
        use axum::body::{self, Body};
        use axum::http::Request;
        use axum::middleware::from_fn as test_from_fn;
        use axum::routing::get;
        use tower::ServiceExt;

        async fn ok_handler() -> &'static str {
            "ok"
        }

        // Install the process-global recorder (ignore a re-install from a prior test).
        let _ = install();

        let app = axum::Router::new()
            .route("/boot/test", get(ok_handler))
            .route("/metrics", get(serve_metrics))
            .layer(test_from_fn(middleware));

        // Drive one request through the instrumented boot path.
        let probe = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/boot/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(probe.status(), axum::http::StatusCode::OK);

        // Scrape /metrics.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .expect("content-type")
            .to_str()
            .unwrap()
            .to_owned();
        assert!(ct.starts_with("text/plain"), "content-type: {ct}");
        let bytes = body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            body.contains("http_requests_total"),
            "missing counter: {body}"
        );
        assert!(
            body.contains("http_request_duration_seconds"),
            "missing histogram: {body}"
        );
        assert!(
            body.contains("endpoint=\"boot\""),
            "expected boot label: {body}"
        );
    }
}
