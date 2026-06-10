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

pub fn health_url(target: &str) -> String {
    if target.ends_with('/') {
        format!("{target}health")
    } else {
        format!("{target}/health")
    }
}
