//! Redirect policies for bearer-bearing HTTP clients.
//!
//! `LocalProvider` and the `cqs doctor` LLM probe both target the
//! user-configured `CQS_LLM_API_BASE`. The submit path attaches an
//! `Authorization: Bearer <key>` header on every request when
//! `CQS_LLM_API_KEY` is set.
//!
//! reqwest's stock `Policy::limited(N)` follows redirects without
//! checking the destination origin — and while reqwest 0.12 strips
//! `Authorization` cross-origin by default, that strip is silent
//! (operator sees a 401 loop on the redirect target instead of a
//! clean fail-fast) and it depends on a default that could shift
//! across versions. SEC-V1.30.1-7 (#1223) closed that gap by adding
//! `same_origin_redirect_policy`, which:
//!
//! 1. Refuses any redirect whose target origin (scheme + host + port)
//!    differs from the previous hop's origin, with a `tracing::warn!`
//!    naming both URLs.
//! 2. Caps the same-origin redirect chain at `max` hops so a
//!    pathologically misconfigured load balancer can't loop us.
//!
//! Refusing the redirect causes `reqwest::send()` to return
//! `Err(reqwest::Error)` whose `is_redirect()` is true — callers
//! surface that as a regular request failure, which for `LocalProvider`
//! flows through the existing retry/backoff path.

use reqwest::redirect::Policy;

/// Build a redirect policy that:
///
/// - Refuses cross-origin redirects (scheme / host / port must match
///   the previous hop). Emits `tracing::warn!` naming the source and
///   destination URLs and converts the redirect into a `reqwest::Error`
///   with `is_redirect()=true` so the caller gets a loud fail-fast.
/// - Caps the same-origin redirect chain at `max` hops by surfacing
///   the same fail-fast error (matches the historical
///   `Policy::limited(max)` behavior, which produced a `TooManyRedirects`
///   error rather than returning the 3xx response).
///
/// `max` is the historical `Policy::limited(max)` cap — a value of `0`
/// disables redirects entirely (any 3xx becomes an error on the first
/// hop).
///
/// We pass error messages through `std::io::Error::new(Other, ..)` so
/// the surfaced `reqwest::Error::source()` chain still names the
/// reason without us having to add a public error type to the lib
/// crate's surface.
pub fn same_origin_redirect_policy(max: usize) -> Policy {
    Policy::custom(move |attempt| {
        if let Some(prev_url) = attempt.previous().last() {
            if prev_url.origin() != attempt.url().origin() {
                tracing::warn!(
                    from = %prev_url,
                    to = %attempt.url(),
                    "Refusing cross-origin redirect on bearer-bearing request"
                );
                let msg = format!(
                    "cross-origin redirect refused: {} -> {}",
                    prev_url,
                    attempt.url()
                );
                return attempt.error(std::io::Error::other(msg));
            }
        }
        if attempt.previous().len() >= max {
            let msg = format!("redirect chain exceeded {} hops at {}", max, attempt.url());
            return attempt.error(std::io::Error::other(msg));
        }
        attempt.follow()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-origin 302 against a request that carries an `Authorization`
    /// header. The redirect chain server A → server B has different
    /// `host:port` (httpmock binds each `MockServer::start()` on a
    /// distinct port), so the same-origin policy must refuse.
    ///
    /// We assert at the `reqwest::blocking::Client` level rather than
    /// going through `LocalProvider::submit_batch_prebuilt` so the
    /// failure mode is unambiguous: the test fails iff the policy
    /// itself stops permitting the redirect.
    #[test]
    fn cross_origin_redirect_is_refused() {
        let server_b = httpmock::MockServer::start();
        let server_a = httpmock::MockServer::start();

        let _redirect_mock = server_a.mock(|when, then| {
            when.method("GET").path("/redirect");
            then.status(302)
                .header("Location", &format!("{}/target", server_b.base_url()));
        });
        let target_mock = server_b.mock(|when, then| {
            when.method("GET").path("/target");
            then.status(200).body("should never be reached");
        });

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .redirect(same_origin_redirect_policy(2))
            .build()
            .unwrap();

        let result = client
            .get(format!("{}/redirect", server_a.base_url()))
            .header("Authorization", "Bearer test-token")
            .send();

        assert!(
            result.is_err(),
            "expected cross-origin redirect to fail, got {result:?}"
        );
        let err = result.unwrap_err();
        assert!(err.is_redirect(), "expected redirect error, got {err}");
        // The target on server B must NOT have been hit.
        assert_eq!(
            target_mock.calls(),
            0,
            "cross-origin target was reached — bearer header would have leaked"
        );
    }

    /// Same-origin 302 within `max` hops follows cleanly. This is the
    /// case the original `Policy::limited(2)` covered (HTTP→HTTPS on
    /// the same host:port, e.g. local TLS terminator) and we must not
    /// regress it.
    #[test]
    fn same_origin_redirect_within_limit_follows() {
        let server = httpmock::MockServer::start();
        let _redirect_mock = server.mock(|when, then| {
            when.method("GET").path("/redirect");
            then.status(302).header("Location", "/target");
        });
        let target_mock = server.mock(|when, then| {
            when.method("GET").path("/target");
            then.status(200).body("ok");
        });

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .redirect(same_origin_redirect_policy(2))
            .build()
            .unwrap();

        let resp = client
            .get(format!("{}/redirect", server.base_url()))
            .send()
            .expect("same-origin redirect should follow");
        assert_eq!(resp.status(), 200);
        assert_eq!(target_mock.calls(), 1);
    }

    /// Same-origin chain that exceeds `max` hops stops cleanly. Using
    /// max=0 forces the very first redirect to stop.
    #[test]
    fn same_origin_redirect_chain_capped() {
        let server = httpmock::MockServer::start();
        let _redirect_mock = server.mock(|when, then| {
            when.method("GET").path("/redirect");
            then.status(302).header("Location", "/target");
        });
        let _target_mock = server.mock(|when, then| {
            when.method("GET").path("/target");
            then.status(200).body("ok");
        });

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .redirect(same_origin_redirect_policy(0))
            .build()
            .unwrap();

        let result = client.get(format!("{}/redirect", server.base_url())).send();
        assert!(
            result.is_err(),
            "max=0 must stop on first redirect, got {result:?}"
        );
        assert!(result.unwrap_err().is_redirect());
    }
}
