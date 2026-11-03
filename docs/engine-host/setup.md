# Office 首次安装与日常运行

新电脑从空白数据开始。只复制/克隆代码仓库，不复制任何人的 `.openpaw`、`.env`、聊天、文章、图片或备份。安装不会导入示例聊天，也不会使用 Echo 代替模型。

## 准备与首次安装

需要 Git、Rust stable（最低1.88）、Node.js 24 LTS及npm。Office也支持22.13+的22.x。Linux需要C编译器、curl及系统构建库，Ubuntu/Debian可安装 `build-essential pkg-config libssl-dev curl`；Windows10/11 x64需要 Visual Studio Build Tools 的“使用C++的桌面开发”和Windows SDK。安装完依赖后重新打开终端。脚本检查依赖并给出缺失原因，不自动以管理员权限安装系统软件。

默认布局为同一父目录下的 `openpaw` 和 `ai-office-react`，父目录可自行选择，支持空格与中文。只有OpenPaw仓库也可以运行setup：若旁边没有Office源码，会询问是否从官方仓库克隆OpenPaw契约锁定的Office版本。已有Office目录不覆盖、不切分支。此版本以 `docs/engine-host/contracts/sources.json` 为准，不盲目追踪上游最新提交。

在OpenPaw仓库根目录执行：

```bash
# Linux
bash scripts/openpaw-office.sh setup
```

```powershell
# Windows PowerShell 5.1或PowerShell 7
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\openpaw-office.ps1 setup
```

依次输入协议、模型名、基础URL和API密钥。DeepSeek使用默认值：`openai`、`deepseek-chat`、`https://api.deepseek.com/v1`，再填写自己的DeepSeek API密钥。基础URL不含 `/chat/completions`。终端中密钥隐藏输入，不应把密钥放在命令参数、截图或聊天中。

接着选择权限：

1. 每次确认：写文件、修改、绘图都在界面逐次确认；持续协作也不会绕过确认。
2. 持久允许：在当前配置的工作根内允许 `write_file`、`edit_file`、`render_visual`，重启仍有效，每次调用保留审计。此选择不开放Shell、其他目录或其他工具。

最后选择仅生成配置、安装依赖并编译，或安装、编译并启动。默认第三项，执行 `npm ci`、原Rust和Office生产编译命令，再启动两个服务并等待健康检查。启动完成后打开 [Office](http://127.0.0.1:8765/office/)。setup不发送付费模型请求；输入一条实际聊天才能确认提供方的密钥、余额、模型和响应额度均可用。HTTP402需检查模型平台余额，401需检查密钥。模型暂时不可用不会退回Echo。

## 自动生成什么

默认根目录为Linux的 `~/.openpaw` 或Windows的 `%USERPROFILE%\.openpaw`：

| 相对位置 | 用途 |
| --- | --- |
| `.env` | 官方模型加载器读取的模型、URL和密钥；不传入Office浏览器 |
| `engine-host-office.toml` | 工作根、预授权槽、工具权限、协作与服务地址 |
| `.office-service-key` | 随机生成的独立Office服务密钥，start自动加载；不是模型密钥 |
| `office-host-home/` | 初始为空，运行后保存本机聊天、任务、审批、历史版本 |
| `workspaces/office-1` 至 `office-32` | 初始全部为空，各办公室独立保存文章、图片等文件 |

首次启动仅启用一个办公室，其余31个槽供界面“新建办公室”使用。六个活动Run容纳主Agent和五个子Agent；持续协作启用，最多四个在途事项，无总运行时限，但网络空闲/单工具/审批超时仍生效。单次模型响应上限16384，服务必须支持；若模型报不支持，可调整原TOML的 `agents.max_response_tokens` 后重启。

Linux安装根权限700，配置/密钥600；Windows安装根仅当前用户和SYSTEM有完全控制，子目录继承私有ACL。文件不放在Git仓库内。setup没有读取或拷贝旧聊天/旧文章的步骤。现有完整配置重跑全部保留；部分配置或已有数据但无配套配置时明确拒绝，不会静默覆盖。请使用新的安装根，或先自行核对恢复配套配置。

## 日常命令与配置修改

Linux：

```bash
bash scripts/openpaw-office.sh build
bash scripts/openpaw-office.sh start
bash scripts/openpaw-office.sh status
bash scripts/openpaw-office.sh stop
# 仅在确认无法正常停止时：
bash scripts/openpaw-office.sh stop --force
```

Windows对应命令为 `powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\openpaw-office.ps1 build`，把末尾替换为 `start`、`status`、`stop`；强制停止为 `stop -Force`。`build`只编译，首次依赖安装或锁文件变更后用setup执行 `npm ci`。控制台Windows进程可能不响应关闭窗口请求；脚本会报告超时，用户确认后才能用Force，不把强杀称作优雅停止。

更换模型：编辑本机 `.env` 后重启。撤销持久写入：将本机TOML顶层 `auto_approve_tools = []`，并将 `[agents.continuous]` 下的 `auto_approve = false`，然后重启。两处分别控制普通/持续工作，不能只改一处。

生图服务是独立可选配置，DeepSeek聊天服务不自动提供照片生成功能；无需图片服务也可用 `render_visual` 生成SVG图表。真实生图配置见 `host-config.example.toml` 的 `[agents.images]` 示例，额外密钥仅存本机 `.env`。

需要更改默认路径/端口时，在setup及后续每次命令前设置相同环境变量；可写入自己的终端配置文件，不写到共享仓库：

```bash
export OPENPAW_SETUP_HOME="$HOME/OpenPaw Data"
export OPENPAW_OFFICE_SOURCE="$HOME/Projects/ai-office-react"
# 可选：OPENPAW_HOST_PORT=32885、OPENPAW_OFFICE_PORT=8765
bash scripts/openpaw-office.sh setup
```

```powershell
$env:OPENPAW_SETUP_HOME = Join-Path $env:USERPROFILE 'OpenPaw Data'
$env:OPENPAW_OFFICE_SOURCE = 'D:\Projects\ai-office-react'
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\openpaw-office.ps1 setup
```

原高级变量 `OPENPAW_ENV_FILE`、`OPENPAW_HOST_CONFIG`、`OPENPAW_OFFICE_KEY_FILE`、`OPENPAW_HOST_BINARY`、`OPENPAW_RUN_DIR`、`OPENPAW_NODE` 仍可用于运行已有安装。setup生成的私有配置必须放在 `OPENPAW_SETUP_HOME` 内，避免改动其他既有目录的权限。后续改变端口也需同步本机TOML中的Office地址和allowed_origins。进程环境中显式的 `PAWOFFICE_API_KEY` 优先于密钥文件；不要给Host和Office不同密钥。

日志和PID默认在Linux的 `${XDG_RUNTIME_DIR:-/tmp}/openpaw-office-$UID` 或Windows的 `%LOCALAPPDATA%\OpenPaw\office-runtime`。这些可重建运行文件与持久聊天数据分开。编译失败查看 `build-openpaw.log` / `build-office.log`；启动失败查看 `host.log` / `office.log`。启动最长等候120秒，进程提前退出会尽快报告；不会停止其他程序占用的端口。停止服务后数据仍保留。

## 验证边界

安装回归复用现有host-contracts的Linux/Windows矩阵，不安装新运行依赖。真实浏览器验收使用隔离空安装、真实Host/Office和受控本地HTTP模型服务，不使用开发者历史或付费密钥。Linux实际结果及原生Windows是否执行，以本轮CHG-2026-0007验证记录为准；适配代码、静态解析或Linux运行PowerShell不能替代Windows原生通过。Office自定义修改仍不实施。
