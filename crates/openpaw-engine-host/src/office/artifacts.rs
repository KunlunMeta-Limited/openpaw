//! 职责：将已授权Host产物事实转换为Office官方交付与安全文件响应。
//! 输入输出：真实工具目标到受限blob、确定版本URL和官方result/deliveries。
//! 依赖：原Root锁、Store、路径安全、ring；不增加模型工具。
//! 禁止职责：不扫描工作区补造历史，不把文件存在当作内容验收，不执行HTML。
use super::{dto, OfficeConfig};
use crate::artifacts::{capability, read_blob, read_secret};
#[cfg(test)]
use crate::artifacts::{publish_blob, secret, sha256, TOTAL_LIMIT};
use crate::{Host, HostError, HostResult};
use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use std::{net::SocketAddr, path::Path as FsPath};

fn access(row: &Value, dir: &FsPath) -> HostResult<String> {
    let scope = serde_json::to_string(&row["scope"]).map_err(HostError::storage)?;
    // Scope JSON must use ClientScope field order, identical to the stored binding key.
    let scope: crate::ClientScope = serde_json::from_str(&scope).map_err(HostError::storage)?;
    let scope = serde_json::to_string(&scope).map_err(HostError::storage)?;
    Ok(capability(
        &read_secret(&dir.join("access.key"))?,
        row["version"].as_str().unwrap_or(""),
        &scope,
        row["artifact"].as_str().unwrap_or(""),
    ))
}
fn url(row: &Value, dir: &FsPath, suffix: &str) -> HostResult<String> {
    Ok(format!(
        "{}/office/artifacts/{}{}?access={}",
        row["origin"].as_str().unwrap_or(""),
        row["version"].as_str().unwrap_or(""),
        suffix,
        access(row, dir)?
    ))
}
fn artifact(row: &Value, dir: &FsPath) -> HostResult<Value> {
    Ok(
        json!({"id":row["artifact"],"versionId":row["version"],"version":format!("v{}",row["number"]),"runId":row["delivery"],"title":row["title"],"kind":if row["mime"].as_str().unwrap_or("").starts_with("image/"){"image"}else{"document"},"mimeType":row["mime"],"sha256":row["hash"],"producedAt":row["at"],"url":url(row,dir,"")?}),
    )
}
// The preview is derived separately; raw bytes/hash/download remain exactly as written.
fn preview(row: &Value, rows: &[Value], dir: &FsPath) -> HostResult<Option<Value>> {
    if row["mime"] != "text/markdown" || row["bytes"].as_u64().is_some_and(|size| size > 32000) {
        return Ok(None);
    }
    let bytes = read_blob(dir, row)?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    if dto::units(text) > 8000 {
        return Ok(None);
    }
    let mut images = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut rendered = text.to_owned();
    for image in rows.iter().filter(|r| {
        r["sequence"].as_i64() <= row["sequence"].as_i64()
            && r["mime"].as_str().unwrap_or("").starts_with("image/")
    }) {
        let Some(path) = image["path"].as_str() else {
            continue;
        };
        if !seen.insert(path) {
            continue;
        }
        let base = FsPath::new(row["path"].as_str().unwrap_or(""))
            .parent()
            .unwrap_or(FsPath::new(""));
        // Resolve only existing captured relative links in the same conversation; never read new files.
        let full = base.join(path).to_string_lossy().to_string();
        let relative = FsPath::new(path)
            .strip_prefix(base)
            .ok()
            .and_then(|p| p.to_str());
        let target = relative.unwrap_or(&full);
        // An explicit current-directory prefix names the same captured file; no path is opened here.
        let mut replaced = false;
        for candidate in [target.to_owned(), format!("./{target}")] {
            let needle = format!("]({candidate})");
            if rendered.contains(&needle) {
                rendered = rendered.replace(&needle, &format!("]({})", url(image, dir, "")?));
                replaced = true;
            }
        }
        if replaced {
            images.push(path);
        }
    }
    if images.is_empty() {
        return Ok(None);
    }
    let mut blocks = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for ch in rendered.chars() {
        if units + ch.len_utf16() > 3500 {
            blocks.push(json!({"type":"text","text":chunk}));
            chunk = String::new();
            units = 0;
        }
        units += ch.len_utf16();
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        blocks.push(json!({"type":"text","text":chunk}));
    }
    Ok(Some(
        json!({"kind":"custom","id":format!("preview-{}",row["version"].as_str().unwrap_or("")),"title":"图文预览","purpose":"artifact-preview","artifactId":row["artifact"],"versionId":row["version"],"span":"full","blocks":blocks}),
    ))
}
// 当前与历史均按准确version取核验说明；每文件仍最多一个section，遵守原8项上限。
fn version_section(row: &Value, rows: &[Value], dir: &FsPath) -> HostResult<Option<Value>> {
    let preview = preview(row, rows, dir)?;
    let Some(detail) = row["check_detail"].as_str() else {
        return Ok(preview);
    };
    let mut section = preview.unwrap_or_else(||json!({"kind":"custom","id":format!("check-{}",row["version"].as_str().unwrap_or("")),"title":"版本核验依据","purpose":"evidence","artifactId":row["artifact"],"versionId":row["version"],"span":"full","blocks":[]}));
    section["blocks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"text","text":dto::bounded(detail,1800)}));
    Ok(Some(section))
}
/// 功能：从可信版本构造固定历史交付。
/// 参数：row为准确版本；rows为同会话版本目录；dir为私有blob根。
/// 返回：只含该次真实写入的官方不可变交付对象。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30；REQ-OP-014；DES-OP-007。
pub(crate) fn delivery(row: &Value, rows: &[Value], dir: &FsPath) -> HostResult<Value> {
    let mut result = json!({"runId":row["delivery"],"title":row["title"],"status":"ready","producedAt":row["at"],"artifacts":[artifact(row,dir)?],"note":format!("原工具成功写出的确定版本；{}。", row["check_summary"].as_str().unwrap_or("本版本未登记核验"))});
    if let Some(section) = version_section(row, rows, dir)? {
        result["sections"] = json!([section]);
    }
    Ok(
        json!({"id":row["delivery"],"sequence":row["sequence"],"turnId":row["turn"],"title":row["title"],"createdAt":row["at"],"result":result}),
    )
}
// 目录仅改变展示身份；原始交付、捕获时点图文和版本访问能力均不变。
fn catalog_id(version: &str) -> String {
    format!("catalog-{version}")
}
fn catalog(mut original: Value, version: &str) -> Value {
    let id = catalog_id(version);
    original["id"] = json!(id);
    original["turnId"] = json!("");
    original["result"]["runId"] = json!(id);
    if let Some(artifacts) = original["result"]["artifacts"].as_array_mut() {
        for artifact in artifacts {
            artifact["runId"] = json!(id);
        }
    }
    original
}
/// 功能：将准确会话产物接入当前白板与历史目录。
/// 参数：db为原Store；scope为可信会话；board为原业务白板。
/// 返回：保持任务事实并附加官方文件汇总。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：最多8当前文件及20历史目录；同轮同文件只关联最新聊天摘要，旧版本不删除。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30及12.141节r32；REQ-OP-014；DES-OP-007。
pub(crate) fn project(
    db: &crate::store::Db,
    scope: &crate::ClientScope,
    board: &mut Value,
) -> HostResult<()> {
    let (rows, dir) = db.artifacts(scope)?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut current = Vec::new();
    let mut sections = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        if !seen.insert(row["artifact"].clone().to_string()) {
            continue;
        }
        if current.len() >= 8 {
            break;
        }
        current.push(artifact(row, &dir)?);
        if let Some(section) = version_section(row, &rows, &dir)? {
            sections.push(section);
        }
    }
    if !board.is_object() {
        *board =
            json!({"template":"task-board-v1","title":"文件交付","status":"completed","steps":[]});
    }
    if !board["result"].is_object() {
        board["result"] = json!({});
    }
    board["result"]["artifacts"] = json!(current);
    board["result"]["sections"] = json!(sections);
    board["result"]["status"] = json!("ready");
    board["result"]["producedAt"] = rows[0]["at"].clone();
    // 新的成功文件不能覆盖同会话先前的捕获失败说明。
    let prior_note = board["result"]["note"].as_str().unwrap_or_default();
    let mut checked = std::collections::HashSet::new();
    let notes: Vec<_> = rows
        .iter()
        .filter(|row| checked.insert(row["artifact"].to_string()))
        .take(3)
        .map(|row| {
            format!(
                "{} v{}：{}",
                dto::bounded(row["path"].as_str().unwrap_or("文件"), 40),
                row["number"],
                row["check_summary"].as_str().unwrap_or("未登记核验")
            )
        })
        .collect();
    let note = dto::bounded(
        &format!("ready表示已保存，不代表整体验收；{}", notes.join("；")),
        300,
    );
    board["result"]["note"] = json!(if prior_note.is_empty() {
        note.clone()
    } else {
        dto::bounded(&format!("{prior_note}；{note}"), 300)
    });
    // rows已按准确会话过滤并按序号倒排；不同轮次、不同文件不得合并。
    let mut turns = std::collections::HashSet::new();
    board["deliveries"] = json!(rows.iter().take(20).map(|row| {
        let latest = turns.insert((row["turn"].as_str().unwrap_or(""), row["artifact"].as_str().unwrap_or("")));
        Ok(json!({
            "id": if latest { row["delivery"].clone() } else { json!(catalog_id(row["version"].as_str().unwrap_or(""))) },
            "sequence": row["sequence"],
            "turnId": if latest { row["turn"].clone() } else { json!("") },
            "title": row["title"], "createdAt": row["at"],
            "snapshotUrl": url(row, &dir, if latest { "/delivery" } else { "/catalog" })?
        }))
    }).collect::<HostResult<Vec<Value>>>()?);
    Ok(())
}

#[derive(Clone)]
struct FileState {
    host: Host,
    config: OfficeConfig,
    address: SocketAddr,
}
/// 功能：为原loopback监听器装配单版本只读文件路由。
/// 参数：host/config/address为原Host及已验证本机配置。
/// 返回：无写操作的Router。
/// 错误：存储、作用域或数据损坏明确拒绝。
/// 状态：只读或发布派生版本，不修改原工具及会话。
/// 并发与取消：原Store线程串行，不重放工具；删除墓碑优先核验。
/// 权限：完整Office会话及原父子绑定，不接受模型指定范围。
/// 关联：CHG-2026-0007第12.124节r30及12.141节r32；REQ-OP-014；DES-OP-007。
pub(crate) fn router(host: Host, config: OfficeConfig, address: SocketAddr) -> Router {
    Router::new()
        .route("/office/artifacts/{version}", get(raw).options(options))
        .route(
            "/office/artifacts/{version}/delivery",
            get(historical).options(options),
        )
        .route(
            "/office/artifacts/{version}/catalog",
            get(catalog_view).options(options),
        )
        .with_state(FileState {
            host,
            config,
            address,
        })
}
fn source(state: &FileState, headers: &HeaderMap) -> HostResult<String> {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ![
        state.address.to_string(),
        format!("localhost:{}", state.address.port()),
    ]
    .contains(&host.to_owned())
        || (!origin.is_empty() && !state.config.allowed_origins.iter().any(|o| o == origin))
    {
        return Err(HostError::new("scope_denied", "文件来源未获授权"));
    }
    Ok(origin.to_owned())
}
fn secured(mut response: Response, origin: &str) -> Response {
    let h = response.headers_mut();
    for (k, v) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'",
        ),
        ("vary", "Origin"),
    ] {
        h.insert(k, v.parse().unwrap());
    }
    if !origin.is_empty() {
        h.insert("access-control-allow-origin", origin.parse().unwrap());
    }
    response
}
fn failure(error: HostError) -> Response {
    let code = if error.kind == "scope_denied" || error.kind == "session_deleted" {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    secured((code,axum::Json(json!({"code":code.as_u16(),"msg":error.message,"data":{"reasonCode":error.kind,"details":{},"issues":[]}}))).into_response(),"")
}
async fn options(State(state): State<FileState>, headers: HeaderMap) -> Response {
    let origin = match source(&state, &headers) {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    if headers
        .get("access-control-request-method")
        .and_then(|h| h.to_str().ok())
        != Some("GET")
        || headers
            .get("access-control-request-headers")
            .is_some_and(|h| {
                h.to_str().ok().is_none_or(|v| {
                    v.split(',').any(|x| {
                        !matches!(
                            x.trim().to_ascii_lowercase().as_str(),
                            "accept" | "content-type"
                        )
                    })
                })
            })
    {
        return failure(HostError::new("scope_denied", "仅允许只读文件预检"));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert("access-control-allow-methods", "GET".parse().unwrap());
    secured(response, &origin)
}
async fn raw(
    State(state): State<FileState>,
    Path(version): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    serve(state, version, query, headers, FileView::Raw).await
}
async fn historical(
    State(state): State<FileState>,
    Path(version): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    serve(state, version, query, headers, FileView::Delivery).await
}
async fn catalog_view(
    State(state): State<FileState>,
    Path(version): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    serve(state, version, query, headers, FileView::Catalog).await
}
#[derive(Clone, Copy)]
enum FileView {
    Raw,
    Delivery,
    Catalog,
}
async fn serve(
    state: FileState,
    version: String,
    query: Option<String>,
    headers: HeaderMap,
    view: FileView,
) -> Response {
    let origin = match source(&state, &headers) {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    if version.len() > 80
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || !query
            .as_deref()
            .is_some_and(|s| s.starts_with("access=") && !s.contains('&'))
    {
        return secured(
            failure(HostError::new("scope_denied", "文件版本或访问凭证无效")),
            &origin,
        );
    }
    let access = query
        .unwrap_or_default()
        .strip_prefix("access=")
        .unwrap_or("")
        .to_owned();
    let result = state
        .host
        .inner
        .store
        .call_priority(move |db| {
            let (data, rows, dir) =
                db.artifact_read(&version, &access, !matches!(view, FileView::Raw))?;
            if matches!(view, FileView::Raw) {
                let bytes = crate::artifacts::read_blob(&dir, &data)?;
                return Ok((
                    bytes,
                    data["mime"]
                        .as_str()
                        .unwrap_or("application/octet-stream")
                        .to_owned(),
                ));
            }
            let checked = rows
                .iter()
                .find(|row| row["version"] == data["version"])
                .unwrap_or(&data);
            let original = delivery(checked, &rows, &dir)?;
            let bytes = serde_json::to_vec(&original).map_err(HostError::storage)?;
            if bytes.len() > 64000 {
                return Err(HostError::new("capacity_exceeded", "历史交付JSON超过64KB"));
            }
            if matches!(view, FileView::Catalog) {
                Ok((
                    serde_json::to_vec(&catalog(original, &version)).map_err(HostError::storage)?,
                    "application/json".into(),
                ))
            } else {
                Ok((bytes, "application/json".into()))
            }
        })
        .await;
    match result {
        Ok((bytes, mime)) => {
            let mut response = (StatusCode::OK, bytes).into_response();
            response
                .headers_mut()
                .insert("content-type", mime.parse().unwrap());
            secured(response, &origin)
        }
        Err(e) => secured(failure(e), &origin),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_markdown_images_bind_to_capture_time_version_without_mutating_original() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        secret(dir).unwrap();
        let raw = b"# Review\n\n![diagram](images/flow.svg)\n";
        let hash = sha256(raw);
        publish_blob(dir, &hash, raw).unwrap();
        let scope = json!({"namespace":"office","slot_id":"team-a","conversation_id":"chat-a"});
        let doc = json!({"version":"v-doc","artifact":"doc","delivery":"delivery-doc","path":"notes.md","mime":"text/markdown","hash":hash,"bytes":raw.len(),"sequence":2,"scope":scope,"origin":"http://127.0.0.1:1234","title":"notes.md","at":"2026-10-30T00:00:00Z","turn":"turn-a","number":1});
        let image = |seq, ver| json!({"version":ver,"artifact":"image","path":"images/flow.svg","mime":"image/svg+xml","sequence":seq,"scope":scope,"origin":"http://127.0.0.1:1234"});
        let rows = vec![
            image(3, "future-image"),
            doc.clone(),
            image(1, "captured-image"),
        ];
        let d = delivery(&doc, &rows, dir).unwrap();
        let preview = d["result"]["sections"][0]["blocks"][0]["text"]
            .as_str()
            .unwrap();
        assert!(preview.contains("/captured-image?access="));
        assert!(!preview.contains("future-image"));
        assert_eq!(read_blob(dir, &doc).unwrap(), raw);
        let old = d.clone();
        let mut next = rows.clone();
        next.insert(0, image(4, "newer-image"));
        assert_eq!(delivery(&doc, &next, dir).unwrap(), old);
        let history = catalog(d.clone(), "v-doc");
        assert_eq!(history["id"], "catalog-v-doc");
        assert_eq!(history["turnId"], "");
        assert_eq!(history["result"]["runId"], history["id"]);
        assert_eq!(history["result"]["artifacts"][0]["runId"], history["id"]);
        assert_eq!(history["result"]["sections"], old["result"]["sections"]);
        for key in ["id", "versionId", "url", "sha256", "producedAt"] {
            assert_eq!(
                history["result"]["artifacts"][0][key],
                old["result"]["artifacts"][0][key]
            );
        }
        assert_eq!(
            history,
            catalog(delivery(&doc, &next, dir).unwrap(), "v-doc")
        );
        assert_eq!(d, old, "catalog must not mutate the original delivery");
    }
    #[test]
    fn d143_dot_relative_images_use_captured_versions_without_broadening_links() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        secret(dir).unwrap();
        for (doc_path, image_path, target) in [
            ("notes/article.md", "notes/flow.svg", "flow.svg"),
            ("article.md", "images/flow.svg", "images/flow.svg"),
        ] {
            let raw = format!(
                "![plain]({target})\n![dot](./{target})\n![parent](../{target})\n![remote](https://example.invalid/{target})\n"
            );
            let hash = sha256(raw.as_bytes());
            publish_blob(dir, &hash, raw.as_bytes()).unwrap();
            let scope = json!({"namespace":"office","slot_id":"team-a","conversation_id":"chat-a"});
            let doc = json!({"version":"doc","artifact":"doc","delivery":"delivery-doc","path":doc_path,"mime":"text/markdown","hash":hash,"bytes":raw.len(),"sequence":2,"scope":scope,"origin":"http://127.0.0.1:1234"});
            let image = |sequence, version| json!({"version":version,"artifact":"image","path":image_path,"mime":"image/svg+xml","sequence":sequence,"scope":scope,"origin":"http://127.0.0.1:1234"});
            let rows = vec![image(3, "future"), doc.clone(), image(1, "captured")];
            let rendered = preview(&doc, &rows, dir).unwrap().unwrap();
            let text = rendered["blocks"][0]["text"].as_str().unwrap();
            assert_eq!(text.matches("/captured?access=").count(), 2);
            assert!(!text.contains("/future?access="));
            assert!(text.contains(&format!("![parent](../{target})")));
            assert!(text.contains(&format!("![remote](https://example.invalid/{target})")));
            assert_eq!(read_blob(dir, &doc).unwrap(), raw.as_bytes());
            let mut newer = rows.clone();
            newer.insert(0, image(4, "newer"));
            assert_eq!(preview(&doc, &newer, dir).unwrap().unwrap(), rendered);
        }
    }

    #[tokio::test]
    async fn d170_real_inspection_receipt_is_accepted_by_original_progress_and_survives_restart() {
        use crate::{ClientScope, HostConfig, RunState};
        use openpaw_core::{
            Capabilities, CompletionRequest, CompletionResponse, ContentBlock, LlmProvider,
            StopReason,
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        struct Checks(AtomicUsize);
        #[async_trait::async_trait]
        impl LlmProvider for Checks {
            async fn complete(
                &self,
                req: CompletionRequest,
            ) -> openpaw_core::Result<CompletionResponse> {
                let n = self.0.fetch_add(1, Ordering::SeqCst);
                let last =
                    req.messages
                        .iter()
                        .flat_map(|m| &m.content)
                        .rev()
                        .find_map(|c| match c {
                            ContentBlock::ToolResult {
                                content, is_error, ..
                            } => {
                                assert!(!is_error, "{content}");
                                Some(content)
                            }
                            _ => None,
                        });
                let parsed = || serde_json::from_str::<Value>(last.unwrap()).unwrap();
                let (name, input) = match n {
                    0 => (
                        "report_progress",
                        json!({"type":"create","steps":[{"id":"verify","title":"核对限定内容"}]}),
                    ),
                    1 => (
                        "write_file",
                        json!({"path":"checks.md","content":"# 核验\n金额100元"}),
                    ),
                    2 => ("inspect_delivery", json!({"action":"list"})),
                    3 => (
                        "inspect_delivery",
                        json!({"action":"read","version":parsed()["versions"][0]["version"]}),
                    ),
                    4 => {
                        let r = parsed();
                        assert_eq!(r["full_text"], true);
                        (
                            "inspect_delivery",
                            json!({"action":"check","version":r["version"],"inspected_call_id":r["read_call_id"],"contains":["100元"],"excludes":["200元"]}),
                        )
                    }
                    5 => {
                        assert_eq!(parsed()["deterministic_failed"], 0);
                        (
                            "report_progress",
                            json!({"type":"update","stepId":"verify","state":"completed","evidenceCallIds":["inspection-4"]}),
                        )
                    }
                    _ => {
                        return Ok(CompletionResponse {
                            content: vec![ContentBlock::Text {
                                text: "已核对两项实际内容，其他要求未核验".into(),
                            }],
                            stop_reason: StopReason::EndTurn,
                            usage: Default::default(),
                        })
                    }
                };
                Ok(CompletionResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: format!("inspection-{n}"),
                        name: name.into(),
                        input,
                    }],
                    stop_reason: StopReason::ToolUse,
                    usage: Default::default(),
                })
            }
            fn capabilities(&self) -> Capabilities {
                Capabilities::default()
            }
            fn name(&self) -> &str {
                "delivery-fixture"
            }
            fn default_model(&self) -> &str {
                "delivery-fixture"
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let cfg:HostConfig=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"r":root},"slots":{"office-1":"r"},"provider":"mock","tools":["write_file"],"auto_approve_tools":["write_file"],"office":{"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_UNUSED","allowed_origins":["http://127.0.0.1:8765"]}})).unwrap();
        let host = Host::with_provider(cfg.clone(), Arc::new(Checks(AtomicUsize::new(0))))
            .await
            .unwrap();
        host.inner
            .store
            .call(|db| db.artifact_origin("http://127.0.0.1:12345"))
            .await
            .unwrap();
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "checks".into(),
        };
        let session = host.open_session(scope.clone()).await.unwrap();
        let run = host
            .submit_run(&scope, &session.session_id, "保存并核验".into())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), host.wait_idle())
            .await
            .unwrap();
        let final_run = host.get_run(&scope, &run.run_id).await.unwrap();
        assert_eq!(
            final_run.state,
            RunState::Completed,
            "{:?}",
            final_run.error
        );
        assert_eq!(final_run.tools.len(), 6);
        assert!(final_run.tools.iter().all(|t| t.state == "completed"));
        let id = run.run_id.clone();
        host.inner
            .store
            .call(move |db| {
                assert_eq!(db.progress(&id)?.unwrap()["steps"][0]["state"], "completed");
                Ok(())
            })
            .await
            .unwrap();
        host.shutdown().await.unwrap();
        let host = Host::with_provider(cfg, Arc::new(openpaw_llm::MockProvider::fixed("未调用")))
            .await
            .unwrap();
        host.inner
            .store
            .call(move |db| {
                let (rows, dir) = db.artifacts(&scope)?;
                assert_eq!(rows.len(), 1);
                assert!(rows[0]["check_summary"]
                    .as_str()
                    .unwrap()
                    .contains("0项未通过"));
                let d = delivery(&rows[0], &rows, &dir)?;
                assert!(d["result"]["sections"][0]["blocks"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("100元"));
                Ok(())
            })
            .await
            .unwrap();
        host.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn real_tools_http_versions_restart_scope_revocation_and_capture_warning() {
        use crate::{ClientScope, HostConfig, RunState};
        use openpaw_llm::{MockMode, MockProvider};
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let mut cfg:HostConfig=serde_json::from_value(json!({"home":tmp.path().join("home"),"roots":{"project":root},"slots":{"office-1":"project"},"provider":"mock","tools":["write_file","edit_file"]})).unwrap();
        let config:OfficeConfig=serde_json::from_value(json!({"base_url":"http://127.0.0.1:9","api_key_env":"PAWOFFICE_UNUSED","allowed_origins":["http://127.0.0.1:8765"]})).unwrap();
        cfg.office = Some(config.clone());
        let scope = ClientScope {
            namespace: "office".into(),
            slot_id: "office-1".into(),
            conversation_id: "versions".into(),
        };
        let steps = vec![
            MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"version.md","content":"# first\n"}),
                text: "实际写入第一版".into(),
            },
            MockMode::Fixed("第一版保存。".into()),
            MockMode::ToolUse {
                tool_name: "edit_file".into(),
                input: json!({"path":"version.md","old_string":"first","new_string":"second"}),
                text: "实际修改第二版".into(),
            },
            MockMode::Fixed("第二版保存。".into()),
            MockMode::ToolUse {
                tool_name: "edit_file".into(),
                input: json!({"path":"version.md","old_string":"second","new_string":"third"}),
                text: "同轮修订第三版".into(),
            },
            MockMode::ToolUse {
                tool_name: "edit_file".into(),
                input: json!({"path":"version.md","old_string":"third","new_string":"fourth"}),
                text: "同轮修订第四版".into(),
            },
            MockMode::ToolUse {
                tool_name: "write_file".into(),
                input: json!({"path":"other.md","content":"独立第二文件"}),
                text: "同轮保存不同文件".into(),
            },
            MockMode::Fixed("两文件已保存。".into()),
        ];
        let host = Host::with_provider(cfg.clone(), Arc::new(MockProvider::scripted(steps)))
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let init = origin.clone();
        host.inner
            .store
            .call_priority(move |db| db.artifact_origin(&init))
            .await
            .unwrap();
        let app = router(host.clone(), config.clone(), address);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let binding = host.open_session(scope.clone()).await.unwrap();
        async fn execute(host: &Host, scope: &ClientScope, session: &str, turn: &str) {
            let run = host
                .submit_idempotent(
                    scope,
                    session,
                    turn.into(),
                    vec![],
                    turn.into(),
                    json!([{"role":"user","content":turn}]).to_string(),
                )
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let (snapshot, _) = host.snapshot_and_subscribe(scope).await.unwrap();
                    if let Some(approval) = snapshot.pending_approvals.first() {
                        host.decide_approval(
                            scope,
                            &approval.approval_id,
                            approval.revision,
                            approval.binding.clone(),
                            true,
                        )
                        .await
                        .unwrap();
                    }
                    let state = host.get_run(scope, &run.run_id).await.unwrap().state;
                    if state.is_terminal() {
                        assert_eq!(state, RunState::Completed);
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        execute(&host, &scope, &binding.session_id, "turn-first").await;
        let snapshot = super::super::projection::prepare(&host, &config, "office-1")
            .await
            .unwrap();
        let board = &snapshot["conversations"][0]["whiteboard"];
        assert_eq!(board["result"]["status"], "ready");
        let first = board["result"]["artifacts"][0].clone();
        let old_delivery = board["deliveries"][0].clone();
        let client = reqwest::Client::new();
        let first_url = first["url"].as_str().unwrap();
        let response = client
            .get(first_url)
            .header("origin", "http://127.0.0.1:8765")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "http://127.0.0.1:8765"
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"# first\n");
        assert_eq!(
            client
                .get(first_url)
                .header("origin", "http://evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .get(first_url)
                .header("host", "evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .get(first_url.replace("access=", "access=00"))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        let old: Value = client
            .get(old_delivery["snapshotUrl"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(old["turnId"], "turn-first");
        execute(&host, &scope, &binding.session_id, "turn-second").await;
        let snapshot = super::super::projection::prepare(&host, &config, "office-1")
            .await
            .unwrap();
        let board = &snapshot["conversations"][0]["whiteboard"];
        let second = &board["result"]["artifacts"][0];
        assert_eq!(first["id"], second["id"]);
        assert_ne!(first["versionId"], second["versionId"]);
        assert_eq!(board["deliveries"].as_array().unwrap().len(), 2);
        assert_eq!(
            client
                .get(first_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"# first\n"
        );
        assert_eq!(
            client
                .get(second["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"# second\n"
        );
        let unchanged: Value = client
            .get(old_delivery["snapshotUrl"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(old, unchanged);
        // Same exact call is idempotent; version publication must not replay the tool or allocate another delivery.
        let check_scope = scope.clone();
        host.inner
            .store
            .call_priority(move |db| {
                let (rows, _) = db.artifacts(&check_scope)?;
                assert_eq!(rows.len(), 2);
                let row = rows.last().unwrap();
                let binding = openpaw_core::ToolCallBinding {
                    run_id: row["run_id"].as_str().unwrap().into(),
                    call_id: row["call_id"].as_str().unwrap().into(),
                    tool_name: "write_file".into(),
                    input: json!({"path":"version.md","content":"# first\n"}),
                };
                db.capture_artifact(&binding, "version.md", "text/markdown", b"# first\n")?;
                assert_eq!(db.artifacts(&check_scope)?.0.len(), 2);
                Ok(())
            })
            .await
            .unwrap();
        execute(&host, &scope, &binding.session_id, "turn-third").await;
        let snapshot = super::super::projection::prepare(&host, &config, "office-1")
            .await
            .unwrap();
        let board = &snapshot["conversations"][0]["whiteboard"];
        let deliveries = board["deliveries"].as_array().unwrap();
        assert_eq!(board["result"]["artifacts"].as_array().unwrap().len(), 2);
        assert_eq!(
            deliveries.len(),
            5,
            "all immutable versions remain discoverable"
        );
        assert_eq!(
            deliveries
                .iter()
                .filter(|d| d["turnId"] == "turn-first")
                .count(),
            1
        );
        assert_eq!(
            deliveries
                .iter()
                .filter(|d| d["turnId"] == "turn-second")
                .count(),
            1
        );
        assert_eq!(
            deliveries
                .iter()
                .filter(|d| d["turnId"] == "turn-third")
                .count(),
            2,
            "one summary per file in this turn"
        );
        let historical = deliveries.iter().find(|d| d["turnId"] == "").unwrap();
        let catalog_url = historical["snapshotUrl"].as_str().unwrap().to_owned();
        let response = client
            .get(&catalog_url)
            .header("origin", "http://127.0.0.1:8765")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "http://127.0.0.1:8765"
        );
        let catalog_bytes = response.bytes().await.unwrap();
        let catalog_value: Value = serde_json::from_slice(&catalog_bytes).unwrap();
        for key in ["id", "sequence", "turnId"] {
            assert_eq!(
                catalog_value[key], historical[key],
                "lazy response identity must match descriptor"
            );
        }
        assert_eq!(catalog_value["result"]["runId"], historical["id"]);
        let third = &catalog_value["result"]["artifacts"][0];
        assert_eq!(third["runId"], historical["id"]);
        assert_eq!(
            client
                .get(third["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "# third\n"
        );
        let original_third_url = catalog_url.replace("/catalog?", "/delivery?");
        let original_third_bytes = client
            .get(&original_third_url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let original_third: Value = serde_json::from_slice(&original_third_bytes).unwrap();
        assert_eq!(original_third["turnId"], "turn-third");
        assert_ne!(original_third["id"], historical["id"]);
        for key in ["id", "versionId", "url", "sha256"] {
            assert_eq!(third[key], original_third["result"]["artifacts"][0][key]);
        }
        for bad_url in [
            catalog_url.replace("access=", "access=00"),
            format!("{catalog_url}&extra=1"),
        ] {
            assert_eq!(client.get(bad_url).send().await.unwrap().status(), 403);
        }
        assert_eq!(
            client
                .get(&catalog_url)
                .header("origin", "http://evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .get(&catalog_url)
                .header("host", "evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert!(serde_json::to_vec(&snapshot).unwrap().len() <= 65536);
        assert_eq!(
            client
                .get(old_delivery["snapshotUrl"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap(),
            old
        );
        host.shutdown().await.unwrap();
        task.abort();
        let _ = task.await;
        drop(host);
        let host = Host::with_provider(
            cfg.clone(),
            Arc::new(MockProvider::scripted(vec![
                MockMode::ToolUse {
                    tool_name: "write_file".into(),
                    input: json!({"path":"quota.md","content":"原件保留"}),
                    text: "写入配额验收".into(),
                },
                MockMode::Fixed("已写入工作区。".into()),
            ])),
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let app = router(host.clone(), config, address);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert_eq!(
            client
                .get(first_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"# first\n"
        );
        let immutable: Value = client
            .get(old_delivery["snapshotUrl"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(old, immutable);
        assert_eq!(
            client
                .get(&catalog_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap(),
            catalog_bytes
        );
        assert_eq!(
            client
                .get(&original_third_url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap(),
            original_third_bytes
        );
        // A different configured conversation has no version directory and cannot obtain another scope's capability.
        let foreign = ClientScope {
            conversation_id: "foreign".into(),
            ..scope.clone()
        };
        host.open_session(foreign.clone()).await.unwrap();
        assert!(host
            .inner
            .store
            .call_priority(move |db| db.artifacts(&foreign).map(|v| v.0))
            .await
            .unwrap()
            .is_empty());
        let scope_check = scope.clone();
        host.inner
            .store
            .call_priority(move |db| {
                let mut board =
                    json!({"result":{"note":"文件版本捕获警告：已写入但没有保存新版本"}});
                project(db, &scope_check, &mut board)?;
                let note = board["result"]["note"].as_str().unwrap();
                assert!(note.contains("文件版本捕获警告") && note.contains("ready表示已保存"));
                Ok(())
            })
            .await
            .unwrap();
        // Force the real storage quota without consuming disk blocks; successful write must remain successful.
        let dir = cfg.home.join("engine-host/artifacts");
        let sparse = dir.join(format!("{}.blob", "e".repeat(64)));
        std::fs::File::create(&sparse)
            .unwrap()
            .set_len(TOTAL_LIMIT)
            .unwrap();
        let quota_scope = ClientScope {
            conversation_id: "quota".into(),
            ..scope.clone()
        };
        let quota_session = host.open_session(quota_scope.clone()).await.unwrap();
        execute(&host, &quota_scope, &quota_session.session_id, "turn-quota").await;
        assert_eq!(
            std::fs::read_to_string(root.join("quota.md")).unwrap(),
            "原件保留"
        );
        let (snapshot, _) = host.snapshot_and_subscribe(&quota_scope).await.unwrap();
        let run = snapshot.runs[0].clone();
        let display = super::super::turns::display(&host, &run).await.unwrap();
        assert!(display.contains("512MiB") && display.contains("已写入"));
        assert_eq!(run.state, RunState::Completed);
        let q = quota_scope.clone();
        assert!(host
            .inner
            .store
            .call_priority(move |db| db.artifacts(&q).map(|a| a.0))
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_file(sparse).unwrap();
        host.delete_session(&scope).await.unwrap();
        assert_eq!(client.get(first_url).send().await.unwrap().status(), 403);
        assert_eq!(client.get(&catalog_url).send().await.unwrap().status(), 403);
        assert_eq!(
            client
                .get(&original_third_url)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert!(root.join("version.md").exists());
        let count = rusqlite::Connection::open(cfg.home.join("engine-host/host.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM host_artifact_versions", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        host.shutdown().await.unwrap();
        task.abort();
        let _ = task.await;
    }
}
