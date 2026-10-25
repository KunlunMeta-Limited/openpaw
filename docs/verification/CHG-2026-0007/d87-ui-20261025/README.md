# D87 真实浏览器与重启复核（2026-10-25/26）

本目录保存当前 OpenPaw 分支连接真实 PawOffice 服务的浏览器证据。Office 仓库、源码、配置和测试均未修改。

## 环境

- OpenPaw：`/media/cheng/新加卷/workSpace/Office/openpaw`
- Office：`http://127.0.0.1:8765/office/`
- Host：`127.0.0.1:32885`，Linux；Windows 原生按项目决定暂缓，macOS 不在范围
- Host 诊断数据：`/tmp/openpaw-diagnostics-home-followup`
- 浏览器：Playwright Chromium，真实页面和官方 HTTP/NDJSON 接口

## 已通过的真实行为

- `office.read` 返回 200，当前 seq 和快照大小见 `office-read-1494-summary.json`；快照仍包含天气、亲子场馆和多 Agent 业务会话的任务/白板事实。
- 左侧六名团队成员逐一点击，均打开官方详情 dialog，`scope_denied=0`；单次点击约 0.28–0.30 秒，完整结果见 `members-all.json`。
- 既有会话切换后聊天正文、成员目录和任务事件仍可读；Host 直接 `/office/chat` 及 PawOffice Node 转发均返回 snapshot/status/delta/done，真实“完成”文本没有被错误标成失败。
- Host 重启后重新输出 `engine-host Office office-1: applied`，快照继续可读，旧终态交接没有重播。

## 仍需区分的限制和未通过项

- `OfficeShell.tsx` 的首页 `flowTasks` 明确过滤 `completed`，所以多 Agent 完成后“当前任务流”显示 0；任务中心仍可显示已完成任务。这是 Office R19，Host 不能把真实完成改报为 running，Office 修改需单独审核。
- 诊断目录已经有 232 个 `office-1` 绑定，超过 Host 配置的 100 个会话上限；新建会话返回 `capacity_exceeded` 是该诊断数据耗尽的真实结果，不是模型失败。计数见 `capacity-diagnostic.json`。新鲜数据目录中应重新做新会话创建验收。
- 页面捕获的 410 是 Office `REQUEST_CLOSED` 的过期/重复 `ui.next` 回执；它不改变 Run、任务或白板事实，继续按 R2/R21 记录。没有把 410 写成成功。
- `proper-single-agent-partial.json` 保留 D87 修复前的真实超时失败，不能覆盖成通过。

## 重复方式

```bash
# 读取当前官方快照（PAWOFFICE_API_KEY 只从本机 Office 进程环境读取）
PAW=$(tr '\0' '\n' </proc/2482022/environ | sed -n 's/^PAWOFFICE_API_KEY=//p')
curl -H 'content-type: application/json' -H "Authorization: Bearer $PAW" \
  --data '{"type":"office.read","teamId":"office-1"}' \
  http://127.0.0.1:8765/api/pawoffice/messages

# 浏览器成员点击复核
node /tmp/ui-members-all.cjs
```
