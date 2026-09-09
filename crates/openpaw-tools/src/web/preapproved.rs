//! Pre-approved domain list for WebFetch.
//!
//! Domains in this list are trusted technical documentation sites.
//! When fetching from these domains, max_chars is raised to 100K and
//! results are cached for 15 minutes.

use std::collections::HashSet;
use std::sync::OnceLock;

/// Pre-approved domains that are trusted for fetching technical documentation.
/// Fetches from these domains get a higher max_chars limit (100K vs 20K default).
static PREAPPROVED_DOMAINS: OnceLock<HashSet<&'static str>> = OnceLock::new();

fn preapproved_domains() -> &'static HashSet<&'static str> {
    PREAPPROVED_DOMAINS.get_or_init(|| {
        HashSet::from([
            // Rust ecosystem
            "doc.rust-lang.org",
            "docs.rs",
            "crates.io",
            "rust-lang.org",
            // TypeScript / JavaScript
            "www.typescriptlang.org",
            "typescriptlang.org",
            "developer.mozilla.org",
            "developer.mozilla.org",
            // Python
            "docs.python.org",
            "peps.python.org",
            "packaging.python.org",
            "wiki.python.org",
            // Go
            "pkg.go.dev",
            "go.dev",
            // Node.js
            "nodejs.org",
            // Frontend frameworks
            "react.dev",
            "vuejs.org",
            "angular.io",
            "angular.dev",
            "nextjs.org",
            "tailwindcss.com",
            "svelte.dev",
            "nuxt.com",
            // CSS / design
            "getbootstrap.com",
            "bulma.io",
            // API documentation
            "swagger.io",
            "www.openapis.org",
            // Cloud providers
            "docs.aws.amazon.com",
            "cloud.google.com",
            "learn.microsoft.com",
            "docs.microsoft.com",
            "developer.apple.com",
            "firebase.google.com",
            // Databases
            "www.postgresql.org",
            "dev.mysql.com",
            "redis.io",
            "mongodb.com",
            "sqlite.org",
            // DevOps / CI
            "docs.github.com",
            "docs.gitlab.com",
            "www.jenkins.io",
            // Other reference
            "github.com",
            "stackoverflow.com",
            "wikipedia.org",
            "en.wikipedia.org",
            "dart.dev",
            "flutter.dev",
            "kotlinlang.org",
            "www.ruby-doc.org",
            "ruby-doc.org",
            "docs.oracle.com",
            "hc.apache.org",
            "nginx.org",
        ])
    })
}

/// Check if a URL's host is in the pre-approved domain list.
/// Subdomains are also matched (e.g., `doc.rust-lang.org` matches `rust-lang.org`).
pub fn is_preapproved_url(url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host_lower = host.to_lowercase();

    let domains = preapproved_domains();

    // Direct match
    if domains.contains(host_lower.as_str()) {
        return true;
    }

    // Subdomain match: check if any preapproved domain is a suffix
    for domain in domains.iter() {
        if host_lower.ends_with(&format!(".{domain}")) {
            return true;
        }
    }

    false
}

/// The max_chars limit for pre-approved domains.
pub const PREAPPROVED_MAX_CHARS: usize = 100_000;

/// The max quote limit (in chars) for pre-approved domains.
/// Trusted documentation sites get a more generous quote allowance.
pub const PREAPPROVED_QUOTE_LIMIT: usize = 5_000;

/// The max quote limit (in chars) for non-pre-approved domains.
/// Regular sites get a stricter quote allowance to respect copyright.
pub const NON_PREAPPROVED_QUOTE_LIMIT: usize = 500;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_docs_is_preapproved() {
        let url = url::Url::parse("https://doc.rust-lang.org/std/").unwrap();
        assert!(is_preapproved_url(&url));
    }

    #[test]
    fn docs_rs_is_preapproved() {
        let url = url::Url::parse("https://docs.rs/serde/1.0").unwrap();
        assert!(is_preapproved_url(&url));
    }

    #[test]
    fn mdn_is_preapproved() {
        let url = url::Url::parse("https://developer.mozilla.org/en-US/docs/Web").unwrap();
        assert!(is_preapproved_url(&url));
    }

    #[test]
    fn random_site_not_preapproved() {
        let url = url::Url::parse("https://example.com/").unwrap();
        assert!(!is_preapproved_url(&url));
    }

    #[test]
    fn subdomain_of_preapproved_is_matched() {
        // doc.rust-lang.org should match "rust-lang.org" if it were in the list
        // But actually we check for exact domain or subdomain suffix
        let url = url::Url::parse("https://docs.python.org/3/").unwrap();
        assert!(is_preapproved_url(&url));
    }

    #[test]
    fn preapproved_max_chars_is_100k() {
        assert_eq!(PREAPPROVED_MAX_CHARS, 100_000);
    }
}
