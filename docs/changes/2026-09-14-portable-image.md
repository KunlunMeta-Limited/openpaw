# OpenPaw 可迁移镜像变更记录

- 来源任务 ID：T7、T9；跨项目依赖：T10。
- 目标：让 OpenPaw 运行时镜像内置 Engine Host/sidecar 所需二进制和 CA 依赖，容器启动阶段不下载 Rust 或运行时包。
- 范围：`Dockerfile` 的构建/运行分层、健康检查和离线启动验证；不改 Engine Host 业务代码。
- 基线 commit：`0726288`。
- 实施步骤：新增 `Dockerfile.engine-host`，构建阶段固化 `engine-host` feature 的 release 二进制，运行阶段仅复制二进制和系统运行库；Compose 通过挂载数据卷保留会话；验证重启不触发下载。既有 `Dockerfile` 继续提供 sidecar 运行时。
- 验证方法：`docker compose build openpaw`；`docker compose up -d openpaw`；健康/协议冒烟；重启并检查日志。
- 状态：文档先行，代码待实施。

## 实际结果（2026-09-14）

- 实际文件：`Dockerfile.engine-host`、根 `docker-compose.yml`（Compose 由总仓库维护）；既有 `Dockerfile` sidecar 运行路径保留。
- 构建结果：`jyzs-openpaw-engine-host:latest` 构建成功，使用本地 `rust:1.88-bookworm` 缓存；运行层只包含 `/usr/local/bin/openpaw` 和数据目录，不挂载 Cargo、不执行运行时编译或下载。
- 启动结果：`docker compose --profile dev up -d --wait --force-recreate openpaw-engine-host openwork-web` 通过；Engine Host `healthy`，宿主端口 `8789` 映射容器端口 `8788`。
- HTTP 验证：`http://127.0.0.1:8789/global/health` 返回 200；容器日志输出 `openpaw server listening on http://0.0.0.0:8788`。
- 联调证据：OpenWork T9 attached eval 通过，1 file / 3 tests passed，无 skip。
- 运行期日志未出现 Cargo、pnpm/npm 或 registry 下载；数据卷 `openpaw-engine-data` 保留可迁移运行数据。
- 未完成项：桌面 UI 人工审批、工具和 abort 手测不属于本镜像变更，仍待单独验收。
- 关联提交：代码和本记录将在本轮 OpenPaw 提交中关联，提交完成后补入具体 hash。
 - 关联提交更正：`9e9e861e836d78cdf53733530f786ca808c8eb9c`（`chore(openpaw): package portable engine host`）。
