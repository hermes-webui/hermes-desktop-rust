//! HTTP probe primitives. Semantics carried over from the Swift app:
//! GET (never HEAD — servers may 405/501 it), and ANY HTTP response —
//! including 4xx/5xx — counts as reachable; only transport errors fail.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Probe TLS roots: the bundled Mozilla store UNIONed with the OS trust
/// store. ureq's default is webpki-roots alone, which rejects
/// enterprise/internal CAs the user's OS trusts — the system webview
/// (WKWebView/WebView2/WebKitGTK) verifies against the OS store, so the page
/// loads fine while Test Connection said "Unreachable" (issue #60). The union
/// is deliberately additive-only: everything that verified before still
/// verifies, and a broken/empty OS store can never make things worse than
/// the webpki baseline (ureq's own `native-certs` feature REPLACES the
/// baseline and fails open to an empty store — not acceptable here).
/// Certificate verification itself is never skipped (the closed PR #59
/// approach).
fn probe_roots() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let baseline = roots.roots.len();
    let native = rustls_native_certs::load_native_certs();
    for e in &native.errors {
        log::warn!("probe tls: OS trust store: {e}");
    }
    let (added, unparsable) = roots.add_parsable_certificates(native.certs);
    log::info!("probe tls: {baseline} webpki roots + {added} OS roots ({unparsable} unparsable)");
    roots
}

fn probe_tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            // Mirror ureq's own construction (rtls.rs): explicit ring provider
            // so an ambiguous process-wide default can never panic the build.
            let config = rustls::ClientConfig::builder_with_provider(
                rustls::crypto::ring::default_provider().into(),
            )
            .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
            .expect("ring provider supports TLS 1.2 + 1.3")
            .with_root_certificates(probe_roots())
            .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .tls_config(probe_tls_config())
        .timeout_connect(timeout)
        .timeout(timeout)
        .build()
}

pub fn http_reachable(url: &str, timeout: Duration) -> bool {
    match agent(timeout).get(url).call() {
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
    match agent(timeout).get(url).call() {
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

    #[test]
    fn probe_roots_never_shrink_below_webpki_baseline() {
        // The union property behind the issue-#60 fix: OS roots are ADDED to
        // the webpki baseline, never substituted for it. A broken/empty OS
        // store must degrade to exactly the old behavior, not to no roots
        // (which is ureq's own `native-certs` failure mode).
        let roots = probe_roots();
        assert!(
            roots.roots.len() >= webpki_roots::TLS_SERVER_ROOTS.len(),
            "probe roots ({}) fell below the webpki baseline ({})",
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len()
        );
    }

    #[test]
    fn probe_tls_config_builds() {
        // Guards the provider/protocol construction (would panic here, not in
        // a user's Test Connection click).
        let _ = probe_tls_config();
    }

    #[test]
    #[ignore = "network: manual sanity check that the union store verifies a live public host"]
    fn probe_live_https_roundtrip() {
        assert!(http_reachable(
            "https://github.com",
            Duration::from_secs(10)
        ));
    }
}
