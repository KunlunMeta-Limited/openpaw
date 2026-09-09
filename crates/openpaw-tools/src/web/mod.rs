//! Web tool stack: web_fetch / web_search / web_extract + html_to_md.
//!
//! Ported from the upstream web tools crate with the browser stack stripped:
//! - `web_fetch`: SSRF validation + DNS-rebinding guard, hop-by-hop manual
//!   redirect handling, LRU cache (50 entries / 15 min TTL), process-level
//!   reqwest client. `auto_browser` and `summarize_prompt` (secondary-model
//!   extraction) dropped with the browser/summarizer stack.
//! - `web_extract`: degraded to plain HTTP fetch + HTML extraction (the
//!   upstream implementation drives the CDP browser pool); `wait_until` /
//!   `wait_for_selector` params dropped — a browser fallback is deferred
//!   until the sandbox/browser batch.
//! - `web_search`: adapter architecture kept; engines converge to Bing +
//!   DuckDuckGo (upstream also had baidu/sogou/tavily); the DuckDuckGo
//!   adapter is new per the port plan.

pub mod html_to_md;
pub mod preapproved;
pub mod web_extract;
pub mod web_fetch;
pub mod web_search;
