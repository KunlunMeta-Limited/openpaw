//! 职责：将官方新建办公室动作分配到预授权槽，保持持久幂等与发布顺序。
//! 输入输出：创建动作到真实Session绑定和官方完整快照。
//! 依赖：原Host/Store/JSONL/Client，不新增数据库或工作根。
//! 禁止职责：不把显示名称变成路径，不创建或删除用户项目文件。
use super::{client::Client, dto, projection, OfficeConfig};
use crate::{ClientScope, Host, HostError, HostResult};
use serde_json::{json, Value};
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

/// 功能：判断配置槽是否已允许发布。
/// 参数：host/config/team为原服务、可信配置和槽ID。
/// 返回：普通槽始终激活，预留槽需要永久创建事实。
/// 错误：Store读取失败，不将损坏当作未分配。
/// 状态：只读；重启后使用原office_created事实。
/// 并发与取消：调用方持相同投影门，避免先发布后回复的竞争。
/// 权限：仅配置内槽可激活。
/// 关联：需求/设计REQ-OP-014；CHG-2026-0006第9.10节。
pub(super) async fn active(host: &Host, config: &OfficeConfig, team: &str) -> HostResult<bool> {
    if !host.inner.config.slots.contains_key(team) {
        return Ok(false);
    }
    if !config.creation_slots.iter().any(|s| s == team) {
        return Ok(true);
    }
    let team = team.to_owned();
    host.inner
        .store
        .call(move |db| Ok(db.office_creations()?.iter().any(|v| v["slot_id"] == team)))
        .await
}

/// 功能：分配预授权槽并准备官方office呈现，保留外发门到UI回执结束。
/// 参数：host/client/config为原服务，action为官方空作用域创建动作，cancel为UI期限。
/// 返回：完整呈现和必须持有至ui.respond结束的槽投影门。
/// 错误：非法动作、未配置/容量满、同ID异内容、取消或存储失败。
/// 状态：先真实Session，后永久归属；确认丢失不回滚、不另分配。
/// 并发与取消：独立分配门串行选槽；持久提交后不因连接取消删除业务。
/// 权限：槽及真实根仅来自配置；不写用户项目文件。
/// 关联：需求/设计REQ-OP-014；CHG-2026-0006第9.10节。
pub(super) async fn create(
    host: &Host,
    client: &Client,
    config: &OfficeConfig,
    action: &Value,
    cancel: &CancellationToken,
) -> HostResult<(Value, OwnedMutexGuard<()>)> {
    if !dto::ui_action(action) || action["action"] != "office.create" {
        return Err(HostError::new("invalid_input", "新建办公室动作格式无效"));
    }
    // 保留键不能与scene_id中的任何真实办公室ID相同。
    let allocation = client.projection_gate("/office-allocation").await;
    let _allocation = tokio::select! {
        _ = cancel.cancelled() => return Err(HostError::new("ui_expired", "创建已过期，未分配办公室")),
        guard = allocation.lock() => guard,
    };
    let creations = host.inner.store.call(|db| db.office_creations()).await?;
    let id = action["id"].as_str().unwrap();
    let name = action["name"].as_str().unwrap();
    let existing = creations.iter().find(|v| v["action_id"] == id);
    let team = if let Some(old) = existing {
        if old["name"] != name {
            return Err(HostError::new(
                "scope_conflict",
                "相同创建标识不能用于不同名称",
            ));
        }
        old["slot_id"]
            .as_str()
            .ok_or_else(|| HostError::storage("创建事实缺少槽ID"))?
            .to_owned()
    } else {
        config
            .creation_slots
            .iter()
            .find(|slot| !creations.iter().any(|v| v["slot_id"] == slot.as_str()))
            .cloned()
            .ok_or_else(|| {
                HostError::new(
                    "capacity_exceeded",
                    "没有可用的新建办公室名额，请在Host配置预授权工作区槽",
                )
            })?
    };
    if !host.inner.config.slots.contains_key(&team) {
        return Err(HostError::new(
            "scope_denied",
            "已创建办公室的工作区配置已移除",
        ));
    }
    let gate = client.projection_gate(&team).await;
    let guard = tokio::select! {
        _ = cancel.cancelled() => return Err(HostError::new("ui_expired", "创建已过期，未分配办公室")),
        guard = gate.lock_owned() => guard,
    };
    if cancel.is_cancelled() {
        return Err(HostError::new("ui_expired", "创建已过期，未分配办公室"));
    }
    if existing.is_none() {
        let binding = host
            .open_session(ClientScope {
                namespace: "office".into(),
                slot_id: team.clone(),
                conversation_id: "main".into(),
            })
            .await?;
        let (id, slot, name) = (id.to_owned(), team.clone(), name.to_owned());
        // Session和分配提交都由原Store可靠完成；过期只终止呈现，不撤销已接受业务。
        host.inner
            .store
            .call(move |db| db.save_office_creation(&id, &slot, &name, &binding.session_id))
            .await?;
    }
    let snapshot = projection::prepare_for_ui(host, config, &team).await?;
    Ok((json!({"target":"office","snapshot":snapshot}), guard))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creation_uses_real_scoped_session_and_survives_unconfirmed_ui() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "office::workspaces::tests::creation_fixture_child",
                "--nocapture",
            ])
            .env("OPENPAW_CREATION_CHILD", "1")
            .env("PAWOFFICE_CREATION_TEST_KEY", "isolated-key")
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[test]
    fn creation_fixture_child() {
        if std::env::var("OPENPAW_CREATION_CHILD").as_deref() != Ok("1") {
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("root");std::fs::create_dir(&root).unwrap();
            let mut cfg:crate::HostConfig = serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"existing":"r","reserved":"r"},"provider":"echo","office":{"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_CREATION_TEST_KEY","allowed_origins":["http://127.0.0.1:8765"],"creation_slots":["reserved"]}})).unwrap();
            cfg.validate().unwrap();
            let office = cfg.office.clone().unwrap();
            let host = Host::start(cfg.clone()).await.unwrap();
            let original_entries=std::fs::read_dir(&root).unwrap().map(|e|e.unwrap().file_name()).collect::<std::collections::BTreeSet<_>>();
            let client = Client::new(&office).unwrap();
            let token = CancellationToken::new();
            assert!(active(&host,&office,"existing").await.unwrap());
            assert!(!active(&host,&office,"reserved").await.unwrap());
            assert_eq!(projection::sync_once(&host,&office,"reserved",&client,false,"127.0.0.1:1".parse().unwrap()).await.unwrap(),"reserved");
            let action=json!({"id":"create-1","teamId":"","conversationId":"","action":"office.create","name":"新办公室"});
            assert!(dto::ui_action(&action));
            let (result,guard) = create(&host,&client,&office,&action,&token).await.unwrap();
            assert_eq!(result["target"],"office");assert_eq!(result["snapshot"]["workspace"]["id"],"reserved");
            assert_eq!(result["snapshot"]["workspace"]["name"],"新办公室");
            let gate=client.projection_gate("reserved").await;
            assert!(gate.try_lock().is_err(),"ui.respond尚未结束时不可提前initialize");
            let bindings=host.inner.store.call(|db|db.bindings()).await.unwrap();
            assert_eq!(bindings.len(),1);assert_eq!(bindings[0].root_id,"r");assert_eq!(bindings[0].scope.conversation_id,"main");
            let session_path=cfg.home.join("sessions").join(format!("{}.jsonl",bindings[0].session_id));
            assert!(session_path.exists(),"新建绑定必须有真实原JSONL");
            assert_eq!(std::fs::read_dir(&root).unwrap().map(|e|e.unwrap().file_name()).collect::<std::collections::BTreeSet<_>>(),original_entries,"不以新建业务办公室创建项目文件");
            // 模拟官方410/断线：释放呈现门，不删除已经提交的归属。
            drop(guard);
            let (same,guard)=create(&host,&client,&office,&action,&token).await.unwrap();assert_eq!(same,result);drop(guard);
            let mut conflict=action.clone();conflict["name"]=json!("另一名称");
            assert_eq!(create(&host,&client,&office,&conflict,&token).await.err().unwrap().kind,"scope_conflict");
            let mut next=action.clone();next["id"]=json!("create-2");
            assert_eq!(create(&host,&client,&office,&next,&token).await.err().unwrap().kind,"capacity_exceeded");
            let mut invalid=action.clone();invalid["teamId"]=json!("existing");assert!(!dto::ui_action(&invalid));
            let cancelled=CancellationToken::new();cancelled.cancel();
            assert_eq!(create(&host,&client,&office,&action,&cancelled).await.err().unwrap().kind,"ui_expired");
            host.shutdown().await.unwrap();
            cfg.office.as_mut().unwrap().names.insert("reserved".into(),"不能覆盖持久名称".into());
            let host=Host::start(cfg).await.unwrap();
            assert!(active(&host,&office,"reserved").await.unwrap());
            let (restored,guard)=create(&host,&client,&office,&action,&token).await.unwrap();assert_eq!(restored,result);drop(guard);
            host.delete_session(&bindings[0].scope).await.unwrap();
            assert!(active(&host,&office,"reserved").await.unwrap(),"删除会话不能回收办公室或重复分配根");
            assert_eq!(host.inner.store.call(|db|db.office_creations()).await.unwrap().len(),1);
            host.shutdown().await.unwrap();
        });
    }
}
