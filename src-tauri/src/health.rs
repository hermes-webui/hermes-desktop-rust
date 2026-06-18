//! HTTP probe primitives. Semantics carried over from the Swift app:
//! GET (never HEAD — servers may 405/501 it), and ANY HTTP response —
//! including 4xx/5xx — counts as reachable; only transport errors fail.

use std::time::Duration;

pub fn http_reachable(url: &str, timeout: Duration) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(timeout)
        .timeout(timeout)
        .build();
    match agent.get(url).call() {
        Ok(_) => true,
        // An HTTP status error still means the round-trip completed.
        Err(ureq::Error::Status(_, _)) => true,
        Err(_) => false,
    }
}

/// Stricter readiness than `http_reachable`, for gating window restore + health
/// (issue #28). A reverse proxy in front of a still-booting upstream
/// (Docker/Tailscale/nginx/Cloudflare) answers `502`/`503`/`504`: the
/// round-trip completes — so `http_reachable` says "up" — but the app isn't
/// actually serving, so restoring tabs onto it lands them on a gateway error
/// page that never recovers. Treat those gateway statuses as NOT ready.
///
/// Everything else that completes a round-trip still counts as ready, so this
/// keeps the spirit of invariant #2 (GET, not HEAD; a `401` login page, a
/// `403`, or a `405` is the server being present): only the
/// upstream-unavailable gateway codes are rejected. Transport errors = not
/// ready, as before.
pub fn http_ready(url: &str, timeout: Duration) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(timeout)
        .timeout(timeout)
        .build();
    match agent.get(url).call() {
        Ok(_) => true,
        Err(ureq::Error::Status(code, _)) => !matches!(code, 502..=504),
        Err(_) => false,
    }
}

pub fn health_url(target: &str) -> String {
    if target.ends_with('/') {
        format!("{target}health")
    } else {
        format!("{target}/health")
    }
}
