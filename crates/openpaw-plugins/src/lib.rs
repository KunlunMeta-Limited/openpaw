//! openpaw-plugins — plugin system for OpenPaw.
//!
//! Provides manifest parsing, multi-source discovery, hook chains, a plugin
//! tool context, and a trust-level static security scanner.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │                PluginManager                     │
//! │  discover_and_load() scans three provenances:    │
//! │  1. Bundled  (<exe-dir>/plugins/)                │
//! │  2. User     (~/.openpaw/plugins/)               │
//! │  3. Project  (./.openpaw/plugins/)  [opt-in]     │
//! │                                                  │
//! │  Priority: User > Project > Bundled              │
//! │  (higher priority wins on name conflicts)        │
//! └───────────────────┬──────────────────────────────┘
//!                     │
//!         ┌───────────┴───────────┐
//!         │                       │
//! ┌───────┴───────┐     ┌─────────┴─────────┐
//! │PluginContext  │     │ SecurityScanner   │
//! │  tools (RwLock)│    │  pattern-based    │
//! │  hooks (Chain) │    │  threat detection │
//! └───────────────┘     └───────────────────┘
//! ```
//!
//! ## Hook points
//!
//! The crate exposes 24 hook points (`HookPoint`). Five of them
//! (`PeerRegistered`, `PeerUnregistered`, `PeerMessageSent`,
//! `CollaborationStarted`, `CollaborationCompleted`) and the workflow-related
//! ones (`BeforeWorkflow`, `AfterWorkflow`, `BeforeStep`, `AfterStep`,
//! `OnStepFailure`) are fully registration-ready but ship no built-in trigger
//! source in the OpenPaw core — they are reserved for future community
//! workflow / collaboration plugins, which will drive the chain themselves.
//!
//! ## Removed workflow bridge
//!
//! The `workflow_bridge` module was deliberately stripped when porting this
//! crate to OpenPaw: OpenPaw has no workflow engine yet, so the bridge has
//! nothing to bind to. It can be reintroduced alongside a future workflow
//! plugin without changing any of the types re-exported here.

pub mod context;
pub mod discovery;
pub mod hooks;
pub mod manifest;
pub mod security;

pub use context::PluginContext;
pub use discovery::{DiscoveryError, DiscoveryReport, LoadedPlugin, PluginManager, Provenance};
pub use hooks::{HookChain, HookContext, HookDecision, HookPoint, HookResult};
pub use manifest::{
    PluginDependencies, PluginKind, PluginManifest, PluginPermissions, PluginProvides,
};
pub use security::{
    ScanResult, SecurityScanner, Severity, StructureIssue, Threat, ThreatCategory, TrustLevel,
};
