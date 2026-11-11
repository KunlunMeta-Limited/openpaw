//! 职责：承载Office与固定OpenCode适配器共用的运行服务。
//! 输入输出：可信Rust作用域到Session、Run、审批、查询及就绪监听。
//! 依赖：可选核心引擎、工具、原存储和执行环境。
//! 禁止职责：不复制AgentLoop、会话账本或工具主体；外部协议留后续适配器。
#[cfg(feature = "engine-host")]
mod agents;
#[cfg(feature = "engine-host")]
mod approval;
#[cfg(feature = "engine-host")]
mod artifacts;
#[cfg(feature = "engine-host")]
mod config;
#[cfg(feature = "engine-host")]
mod continuous;
#[cfg(feature = "engine-host")]
mod query;
#[cfg(feature = "engine-host")]
mod question;
#[cfg(feature = "engine-host")]
mod resource;
#[cfg(feature = "engine-host")]
mod server;
#[cfg(feature = "engine-host")]
mod service;
#[cfg(feature = "engine-host")]
mod store;
#[cfg(feature = "engine-host")]
mod tool_host;
#[cfg(feature = "engine-host")]
mod types;
#[cfg(feature = "engine-host")]
pub use {
    config::{AgentsConfig, HostConfig},
    continuous::ContinuousConfig,
    server::HostServer,
    service::Host,
    types::*,
};

#[cfg(feature = "engine-host")]
mod office;
#[cfg(feature = "engine-host")]
pub use office::OfficeConfig;

#[cfg(feature = "engine-host")]
mod task_progress;

#[cfg(feature = "engine-host")]
mod orchestration;

#[cfg(feature = "engine-host")]
mod workspace_migration;
#[cfg(feature = "engine-host")]
pub use workspace_migration::{apply_workspaces, plan_workspaces};

#[cfg(feature = "engine-host")]
mod delivery;

#[cfg(all(test, feature = "engine-host"))]
mod test_support;

#[cfg(all(test, feature = "engine-host"))]
mod child_proof_tests {
    #[test]
    fn rejects_zero_filter_early_return_incomplete_and_stale_proofs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!super::test_support::proof_matches(dir.path(), "current"));
        std::fs::write(dir.path().join("started"), "current").unwrap();
        assert!(!super::test_support::proof_matches(dir.path(), "current"));
        std::fs::write(dir.path().join("completed"), "old").unwrap();
        assert!(!super::test_support::proof_matches(dir.path(), "current"));
        std::fs::write(dir.path().join("completed"), "current").unwrap();
        assert!(super::test_support::proof_matches(dir.path(), "current"));
        for name in [
            "does_not_exist_r39",
            "office::scene::tests::scene_fixture_child",
        ] {
            let result = std::panic::catch_unwind(|| {
                super::test_support::run_child(
                    std::process::Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", name])
                        .env_remove("OPENPAW_SCENE_CHILD"),
                    name,
                );
            });
            assert!(result.is_err(), "成功退出而未执行真实场景必须拒绝：{name}");
        }
    }
}
