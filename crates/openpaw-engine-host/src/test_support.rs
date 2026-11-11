//! 职责：共享隔离测试配置和正常子场景实际执行证明。
//! 输入输出：受控Command、场景名到唯一started/completed证据。
//! 依赖：仅测试依赖tempfile、uuid、serde_json；不进入生产装配。
//! 禁止职责：不代替场景断言，不对预期硬停进程要求正常完成。
use std::{path::PathBuf, process::Command};

/// 用途：只在正常子场景实际进入后持有的本轮证明。
/// 字段：directory为父进程私有目录，identity绑定nonce和准确场景。
/// 来源去向：start读取父进程环境，complete在真实断言和清理后显式写入。
/// 约束：Drop不标记成功；缺环境、提前返回、崩溃均不能产生完成证明。
/// 稳定性：私有测试设施。
/// 关联：r39子测试零筛选反例。
pub(crate) struct ChildProof {
    directory: PathBuf,
    identity: String,
}
impl ChildProof {
    /// 功能：进入原环境门控后登记子场景开始。
    /// 参数：scenario为准确测试名。
    /// 返回：需显式complete的证明。
    /// 错误：缺少父进程证明环境或场景不符立即失败。
    /// 状态：只写独立证明目录，不触及业务数据。
    /// 并发与取消：每个父测试独立nonce和目录，不共享固定路径。
    /// 权限：仅测试子进程环境。
    /// 关联：子场景实际执行证明；需求REQ-OP-014；设计DES-OP-008；CHG-2026-0007 §12.169。
    pub(crate) fn start(scenario: &str) -> Self {
        assert_eq!(std::env::var("OPENPAW_TEST_SCENARIO").unwrap(), scenario);
        let directory = PathBuf::from(std::env::var_os("OPENPAW_TEST_PROOF").unwrap());
        let identity =
            serde_json::to_string(&(std::env::var("OPENPAW_TEST_NONCE").unwrap(), scenario))
                .unwrap();
        std::fs::write(directory.join("started"), &identity).unwrap();
        Self {
            directory,
            identity,
        }
    }
    /// 功能：全部真实断言及清理之后显式登记完成。
    /// 参数：self为当前子场景证明。
    /// 返回：无。
    /// 错误：写证明失败则测试失败。
    /// 状态：不使用Drop，提前退出不会伪造通过。
    /// 并发与取消：单子场景私有目录。
    /// 权限：仅测试证据。
    /// 关联：子场景实际执行证明；需求REQ-OP-014；设计DES-OP-008；CHG-2026-0007 §12.169。
    pub(crate) fn complete(self) {
        std::fs::write(self.directory.join("completed"), self.identity).unwrap();
    }
}
/// 功能：校验同一次准确场景已经开始且正常完成。
/// 参数：directory为本轮目录，identity为nonce和场景序列化值。
/// 返回：两份证据均准确时为true。
/// 错误：缺标记或读取失败均返回false。
/// 状态：只读证明，不依据本地化测试输出。
/// 并发与取消：调用方等待子进程退出后检查。
/// 权限：仅测试目录。
/// 关联：run_child及反例。
pub(crate) fn proof_matches(directory: &std::path::Path, identity: &str) -> bool {
    ["started", "completed"]
        .into_iter()
        .all(|name| std::fs::read_to_string(directory.join(name)).is_ok_and(|s| s == identity))
}
/// 功能：运行正常子场景并拒绝零测试或提前返回的成功退出。
/// 参数：command保留原参数和环境，scenario为准确目标名。
/// 返回：无，子进程和证明均满足才返回。
/// 错误：启动、失败退出、缺少或错误证明均使父测试失败。
/// 状态：临时证明由父进程清理；不创建业务数据。
/// 并发与取消：每次调用唯一目录及nonce；不改变原场景取消逻辑。
/// 权限：仅测试进程。
/// 关联：ChildProof；预期硬停和worker保留原ready校验。
pub(crate) fn run_child(command: &mut Command, scenario: &str) {
    let directory = tempfile::tempdir().unwrap();
    let nonce = uuid::Uuid::new_v4().to_string();
    let identity = serde_json::to_string(&(&nonce, scenario)).unwrap();
    let status = command
        .env("OPENPAW_TEST_PROOF", directory.path())
        .env("OPENPAW_TEST_NONCE", nonce)
        .env("OPENPAW_TEST_SCENARIO", scenario)
        .status()
        .unwrap();
    assert!(status.success(), "子场景失败：{scenario} {status}");
    assert!(
        proof_matches(directory.path(), &identity),
        "子场景未实际完整执行或证明不匹配：{scenario}"
    );
}
/// 功能：共享原无网络Office展示测试配置。
/// 参数：无。
/// 返回：原activity_window=true及UNUSED密钥名配置。
/// 错误：无。
/// 状态：不写配置或环境。
/// 并发与取消：纯构造函数。
/// 权限：不扩大工具、根、期限和网络权限。
/// 关联：projection/details原独立场景。
#[allow(dead_code)] // 部分集成测试只复用子进程证明，不装配Office。
pub(crate) fn office_config() -> crate::OfficeConfig {
    crate::OfficeConfig {
        activity_window: true,
        base_url: "http://127.0.0.1:9".into(),
        api_key_env: "UNUSED".into(),
        allowed_origins: vec![],
        names: Default::default(),
        creation_slots: vec![],
    }
}
