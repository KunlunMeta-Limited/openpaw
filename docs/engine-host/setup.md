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

首次启动仅启用一个办公室，其余31个槽供界面“新建办公室”使用。新安装的 `max_active_runs = 24` 是整个Host共享的执行额度，足够四组主Agent和五个子Agent同时执行；不是每个办公室各有24个额度，也不是只能运行一个办公室。协作主Run会另预留一个子执行额度，排队任务等待额度后才实际开工。`agents.max_active_children = 5` 是每组的子Agent并发上限，32个办公室槽是可创建空间数，三者相互独立。持续协作启用，最多四个在途事项，无总运行时限，但网络空闲/单工具/审批超时仍生效。单次模型响应上限16384，服务必须支持；若模型报不支持，可调整原TOML的 `agents.max_response_tokens` 后重启。

已有安装重跑setup不会覆盖原配置。若旧配置仍为 `max_active_runs = 6`，可在本机 `engine-host-office.toml` 顶层调整为24，待当前任务完成后停止并启动服务；运行中的Host不会热加载这个值。提高并发会增加模型请求、内存和费用，应按机器及模型服务限流调整，仍受原Host最大256的校验限制。人物完成汇报仅显示短业务标题，业务是否验收仍由原任务与账本判断。待执行卡会明确说明“正在排队，尚未开始工作”；不能根据卡片出现或动画位置推断实际执行时间。

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


## 通用协作与版本核验（D170—D173）

任务由本轮用户要求决定。人物名称/role_id只绑定身份，实际工具决定能力；简单问题直接回答，互不依赖工作同批异步委派，同人FIFO，只有真实依赖等待，不默认附加审核。文章、活动指南、方案比较等只是用户需求举例，并非系统内置流水线。没有配置持续工具的普通对话不注入持续任务操作指引。

Office对话及可信子Run可用`inspect_delivery`：`list`返回当前会话最近20个不可变版本；`read`按version取最多6000 Unicode字符，回显SHA256、覆盖范围和read_call_id；`check`绑定同Run读取回执，记录明确contains/excludes和字符界限，或引用原文的模型意见。字符数含空白标点，不等于中文字数。条件通过仅证明列出的条件；模型意见、未查项及业务验收分别说明。改写后的新版本须重新读取和检查；原件及历史不被覆盖。二进制图像内容不由该文本工具验收，不因此宣称已视觉核验。

这些是只读产物/核验元数据操作，不新增写文件或工具授权，不要求每次都派审核人员。现有成果阅读器的验证依据显示准确版本记录；没有记录的旧文件保持未核验。版本及最多256条/Run读取/核验事实沿原Host home持久保存；删除会话沿原墓碑和删除流程失效。常规回归按改动选择原Rust/浏览器测试，历史一次200轮不变成每次编译必须重跑的门槛；真实模型效果以本轮独立样本和报告为准。


## 来源诊断与自动选人（D174—D177）

未指定人物的独立任务可使用原delegate_task：role_id填auto，candidate_roles列能胜任的现有非lead角色，required_tools列实际必要工具；纯文字成果填空数组。选择只比较本组未清理FIFO长度，负载相同按候选顺序；真实role_id以回执为准。固定人员、已明确的作者/写入归属继续填原角色，Host不会静默改派或授予工具。人数不超过已有组/全局容量；不要用auto要求同一文件多人并发改写。没有新增本机配置、角色或权限。

web_search现在默认摘要，不自动抓取全部结果；需要时显式fetch_content=true或用web_fetch抓相关真实链接。工具会说明失配/弱相关、反爬、HTTP与传输失败；词面相交不保证内容正确。web_fetch单次总限30秒覆盖DNS和所有跳转/正文，不改变模型180秒空闲阈值和Run无限持续配置。404/410不继续猜路径，403/挑战页不绕过控制，429不立刻循环，网络不可用时保留已有成果并明确待核实。没有每研究任务次数/页面额度。成功仍使用原15分钟缓存，fresh只用于确需新鲜的数据；不缓存失败页面，也没有新增付费搜索服务。

D174故障收敛补充：仅对搜索引擎的传输/HTTP/限流/反爬失败记60秒进程内冷却，自动链公开deferred原因及剩余时间，避免每个新查询都重新等待同一故障端点。空结果、低相关不是引擎故障，不做跨查询结果复用；显式engine仍可立即探测，恢复后清除。取消中的未完成探测不写失败缓存。该机制不是每研究任务检索/页面额度，不持久改变配置或权限。明确未知条件不得当既定事实，应急建议亦不能静默改变用户硬约束。


## 交互补充信息（r37）

新安装的setup会单独询问是否启用沟通工具。选择y才将`clarify`加入Host配置`tools`，不会改变`auto_approve_tools`；重跑setup保留原配置，不能静默扩大许可。已有安装如需启用，在实际`OPENPAW_HOST_CONFIG`所指TOML的顶层`tools`数组中明确加入`"clarify"`，待任务空闲后重启Host；Office配置/代码不变。角色显式`agents.tools.<role>`清单若缩小了工具，需由用户决定是否也加入clarify，默认角色仅继承Host已许可的沟通工具。

Agent仅在关键缺项实质影响结果时调用。Office显示真实选项及“其他需求”输入；选择或提交文本后原Run继续，不需要另发聊天消息。无选项时显示两个通用选择“按明确标注的假设继续”“暂不补充”，用户也可自行补充，没有默认代选。正文中的“先确认一下”不等于调用了提问工具。普通聊天仍受原session_busy约束；请使用当前问题菜单，或停止后重发。

问题固定最多300秒，受更早Run/父组期限约束，刷新不延长。此期限与模型180秒无有效事件阈值、普通工具执行时限不同。超时不自动默认或重复提问；网络/协议问题有界重试同题后返回明确错误，已保存内容保留。停止或删除会话将取消待回答菜单并清理；重启不自动重放问题或执行操作。等待占用真实Run和角色FIFO，但不占文件根锁，不阻塞其他有容量的独立角色/会话。回答仅是补充信息，不能授权文件、命令或更改工具许可。


## D182—D184：主动分工、来源故障与真实演示

多个独立实质成果默认由不同合格成员同批异步执行；简单问题直接答复。角色擅长不限制工种，能联网的其他同事也可检索。未指定归属时使用原auto选择；固定role_id不能同时附candidate_roles/required_tools，错误说明会提示准确纠正方式。不要把accepted/同人排队说成多人实际并行。

搜索发现链每后端8秒、整链共享12秒，失败回执说明backend/阶段/耗时；不是研究次数上限，也不是模型180秒无有效事件期限。显式批量正文读取另受原网页工具限制。若Bing原始HTML泛化或DDG连接不可达，不能把候选说成核实，用户可核对可达的原始官方链接；工具不自动改主机代理或网络。

演示可在单一对话输入持续的技术内容工作室目标，指定最大在途数量、实际产出、真实前序交接、文件路径、图注和明确停止方式，工种由团队按工具自主分配；无需把文章/公园/旅行等业务写进系统提示词。现有UI的“关闭空闲闲聊”开关可避免咖啡占用演示人物，属于显式显示设置，不代表改Office或让后台等待动画。正式服务是否已切换须依据进程和准确二进制记录，本轮隔离UI测试不能代替正式部署。


D182真实长任务诊断：子Run的有限执行轮次与持续任务整体时长不同，反复读写/核验也可能耗尽轮次；新版将明确提示“执行轮次已达安全上限”，不是网络或模型180秒。应先检查循环及任务范围，不直接扩大预算。近似字数不反复精确计数；审查等同一成果真实修订完成，传入准确前序task_id。身份并不限定工种，未指名且无已有责任时用auto。该提示修复不表示OfficeR27或r38已实施。


## 明确网页编码兼容（r38 r2 / D185）

web_fetch沿原工具入口支持HTTP Content-Type、BOM或HTML前1024原始字节明确声明的编码，使用锁定encoding_rs 0.8.35严格解码；标准标签gb2312/GBK按库的标准别名映射处理。未知、互相冲突、损坏编码、解码后NUL以及无声明的非UTF-8仍返回unsupported_encoding；不猜测编码，不使用替换字符把损坏正文当成功。有效UTF-8保留原兼容路径，但显式未知或冲突声明仍拒绝。HTML声明只在HTML媒体类型（或原未声明媒体类型）扫描，忽略注释及script/style中的假meta；明确UTF-16按码元扫描，仍受1024原始字节窗口限制。

原5MiB响应字节上限、逐跳SSRF校验、30秒工具总时限、取消、15分钟成功缓存和fresh语义不变。正确解码仅证明取得可读正文，不代表官网首页包含待查规则或模型已完成事实核验。无新工具、权限、模型阈值或Office改动；原搜索源不可达及低相关问题不因此视为解决。依赖许可证为(Apache-2.0 OR MIT) AND BSD-3-Clause，MSRV 1.36；依赖及双平台验收以准确本轮报告为准，未运行不记通过。
