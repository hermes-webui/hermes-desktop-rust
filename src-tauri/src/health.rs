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
        Err(ureq::Error::Status(code, _)) => ready_from_status(code),
        Err(_) => false,
    }
}

/// The pure readiness decision for a completed round-trip's status code: only
/// the upstream-unavailable gateway codes (502/503/504) are NOT ready; every
/// other status — 200, a 401/403 login wall, a 404, the 405/501 a server
/// returns for an unsupported method — is the server being present (invariant
/// #2). Extracted so the load-bearing 502–504 boundary (the whole #28 fix) is
/// unit-testable without constructing a `ureq` transport error.
fn ready_from_status(code: u16) -> bool {
    !matches!(code, 502..=504)
}

pub fn health_url(target: &str) -> String {
    if target.ends_with('/') {
        format!("{target}health")
    } else {
        format!("{target}/health")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_codes_are_not_ready() {
        // 502/503/504 = proxy up but upstream still booting → restore must wait
        // (issue #28). These are the ONLY statuses http_ready rejects.
        for code in [502u16, 503, 504] {
            assert!(!ready_from_status(code), "{code} should be NOT ready");
        }
    }

    #[test]
    fn present_server_statuses_are_ready() {
        // Everything that completes a round-trip and isn't a 502–504 gateway
        // error is the server being present — including a 401/403 login wall, a
        // 404, a 500, and the 405/501 a server returns for a method it doesn't
        // implement (invariant #2: GET-not-HEAD, any response = reachable).
        for code in [
            200u16, 204, 301, 302, 400, 401, 403, 404, 405, 418, 500, 501, 505,
        ] {
            assert!(ready_from_status(code), "{code} should be ready");
        }
    }

    #[test]
    fn health_url_joins_with_one_slash() {
        assert_eq!(health_url("http://h:8787"), "http://h:8787/health");
        assert_eq!(health_url("http://h:8787/"), "http://h:8787/health");
    }
}
