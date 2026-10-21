#!/usr/bin/env python3
"""项目治理入口：复用固定框架的CHG格式，补齐真实范围、追踪与增量门禁。"""
from __future__ import annotations
import argparse
import fnmatch
import hashlib
import importlib.util
import json
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
PROFILE = "governance/project-profile.toml"
CHANGE_DIR = "docs/changes"
REQ = "docs/requirements/产品需求说明.md"
DES = "docs/design/软件设计说明.md"
INDEX = CHANGE_DIR + "/INDEX.md"
TRACE = "docs/verification/追踪矩阵.md"
PLACEHOLDERS = {"", "无", "待审批", "待审核", "待填写", "待确认"}
LEVELS = {"L1": 1, "L2": 2, "L3": 3, "L4": 4}
FORMAT = "openpaw/chg-review-v2"
STAGES = ("proposal", "implementation", "delivery")
# 只有这些首页字段属于方案；审批/PR/实测事实不反向改变已批准方案。
PLAN_KEYS = ("变更编号", "变更标题", "项目CHG格式", "阶段", "提案版本", "变更类型",
             "风险等级", "负责人", "必须审核", "设计基线", "目标分支", "关联修订",
             "历史元数据维护", "历史说明维护")


class Failure(Exception):
    """表示必须显式处理的治理故障，不把校验失败当成功。"""


def git(root: Path, *args: str) -> bytes:
    """执行只读Git命令，参数使用数组，不经shell解释。"""
    result = subprocess.run(["git", *args], cwd=root, capture_output=True)
    if result.returncode:
        raise Failure(result.stderr.decode("utf-8", "replace").strip())
    return result.stdout


def commit(root: Path, ref: str) -> str:
    """把引用固定为提交，拒绝选项注入和漂移的比较对象。"""
    return git(root, "rev-parse", "--verify", "--end-of-options", ref + "^{commit}").decode().strip()


def relative(path: str) -> str:
    """要求规范仓库相对路径，允许中文/空格/竖线，禁止越界与模糊通配。"""
    p = PurePosixPath(path)
    if not path or p.is_absolute() or ".." in p.parts or "\\" in path or str(p) != path or re.match(r"^[A-Za-z]:", path) or "\0" in path:
        raise Failure("无效相对路径：" + path)
    return path


class Reader:
    """按提交读取对象；不使用脏工作区内容冒充head版本。"""

    def __init__(self, root: Path, ref: str | None = None):
        self.root, self.ref = root.resolve(), ref
        self._paths = None

    def read(self, path: str) -> str:
        relative(path)
        if self.ref:
            return git(self.root, "show", self.ref + ":" + path).decode("utf-8")
        resolved = (self.root / path).resolve()
        if not resolved.is_relative_to(self.root):
            raise Failure("文件链接越出仓库：" + path)
        return resolved.read_bytes().decode("utf-8")

    def files(self) -> list[str]:
        if self._paths is not None:
            return self._paths
        if self.ref:
            data = git(self.root, "ls-tree", "-r", "--name-only", "-z", self.ref)
        else:
            data = git(self.root, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
        paths = sorted(set(p.decode("utf-8") for p in data.split(b"\0") if p))
        self._paths = paths if self.ref else [p for p in paths if (self.root / p).is_file()]
        return self._paths

    def exists(self, path: str) -> bool:
        return path in self.files()


def cells(line: str) -> list[str]:
    """严格解析单行中文表格，支持转义竖线，不静默丢弃坏行。"""
    if not line.strip().startswith("|") or not line.strip().endswith("|"):
        raise Failure("表格行必须有首尾竖线")
    text = line.strip()[1:-1]
    values, current = [], []
    index = 0
    while index < len(text):
        char = text[index]
        if char == "\\" and index + 1 < len(text) and text[index + 1] in "\\|":
            index += 1
            current.append(text[index])
        elif char == "|":
            values.append("".join(current).strip().strip(chr(96)))
            current = []
        else:
            current.append(char)
        index += 1
    values.append("".join(current).strip().strip(chr(96)))
    return values


def tables(body: str) -> dict[str, list[dict[str, str]]]:
    """每个二级标题只允许一张数据表；重复标题、错列和缺分隔符明确失败。"""
    lines, result, seen, section = body.splitlines(), {}, set(), ""
    index, fence = 0, False
    while index < len(lines):
        line = lines[index].strip()
        if line.startswith(("~~~", chr(96) * 3)):
            fence = not fence
            index += 1
            continue
        if fence:
            index += 1
            continue
        if line.startswith("## "):
            if line in seen:
                raise Failure("重复章节：" + line)
            seen.add(line)
            section = line
        elif line.startswith("|"):
            if not section or section in result or index + 1 >= len(lines):
                raise Failure("无章节或重复表格")
            headers = cells(line)
            if len(set(headers)) != len(headers) or any(not h for h in headers):
                raise Failure("表格列名重复或为空")
            if any(not re.fullmatch(r":?-{3,}:?", cell) for cell in cells(lines[index + 1])):
                raise Failure("缺少合法表格分隔行")
            if len(cells(lines[index + 1])) != len(headers):
                raise Failure("分隔行列数不一致")
            rows = []
            index += 2
            while index < len(lines) and lines[index].strip().startswith("|"):
                values = cells(lines[index])
                if len(values) != len(headers):
                    raise Failure("表格列数不一致：" + lines[index])
                rows.append(dict(zip(headers, values)))
                index += 1
            result[section] = rows
            continue
        index += 1
    if fence:
        raise Failure("未闭合代码块")
    return result


def upstream():
    """只加载校验通过的固定原型；不暴露其approve或弱范围CLI。"""
    lock = tomllib.loads((ROOT / "governance/framework.lock.toml").read_text(encoding="utf-8"))
    path = ROOT / relative(lock["file"])
    if hashlib.sha256(path.read_bytes()).hexdigest() != lock["revision"]:
        raise Failure("framework快照哈希不匹配")
    spec = importlib.util.spec_from_file_location("openpaw_chg_format", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module._table_cells = cells
    module._markdown_tables = tables
    module.APPROVAL_STATUSES = {"approved", "closed"}
    return module


def approval_fields(parsed: dict) -> dict:
    """读取唯一审批表；重复键不能通过字典覆盖隐藏。"""
    rows = parsed.get("## 1. 变更审批", [])
    result = {r["审批项目"]: r["内容"] for r in rows}
    if len(rows) != len(result):
        raise Failure("重复审批字段")
    return result


def normalized_markdown(value: str) -> str:
    """表格对齐/CRLF不改变方案；代码块及说明正文保持原意，不放宽实际字段。"""
    result, fence = [], False
    for line in value.replace("\r\n", "\n").splitlines():
        if line.strip().startswith(("~~~", chr(96) * 3)):
            fence = not fence
        if not fence and line.strip().startswith("|"):
            row = cells(line)
            result.append("TABLE-SEPARATOR" if all(re.fullmatch(r":?-{3,}:?", c) for c in row)
                          else json.dumps(row, ensure_ascii=False))
        else:
            result.append(line)
    return "\n".join(result)


def plan_digest(body: str) -> str:
    """绑定实质方案，忽略审批事实/履历及CRLF，不忽略方法、权限与测试。"""
    parsed = tables(body)
    approval = approval_fields(parsed)
    sections = re.split(r"(?m)^## ", body.replace("\r\n", "\n"))
    plan = [normalized_markdown(section.strip()) for section in sections[1:]
            if re.match(r"(?:[2-9]|10|11)\. ", section)]
    value = {"fields": {k: approval.get(k, "") for k in PLAN_KEYS}, "sections": plan,
             "introduction": sections[0].strip()}
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True).encode()).hexdigest()


def legacy_entry(reader: Reader, path: str, profile: dict) -> dict:
    """有限兼容必须定位已存在的历史记录，不能通过自填legacy免审。"""
    entries = [r for r in profile.get("legacy_changes", []) if r["path"] == path]
    if len(entries) != 1:
        raise Failure("未登记项目格式；旧CHG必须在profile精确登记：" + path)
    entry = entries[0]
    if not re.fullmatch(r"[0-9a-f]{40}", entry["ref"]):
        raise Failure("legacy必须固定完整历史提交")
    original = approval_fields(tables(Reader(reader.root, entry["ref"]).read(path)))
    current = approval_fields(tables(reader.read(path)))
    if original["变更编号"] != current["变更编号"]:
        raise Failure("legacy历史身份不匹配")
    return entry


def require_fact(approval: dict, keys: tuple[str, ...]) -> None:
    """要求可审计事实字段；非空文本本身不能认证审核人身份。"""
    for key in keys:
        if approval.get(key, "") in PLACEHOLDERS or approval.get(key, "").startswith("待"):
            raise Failure("缺少审批事实：" + key)


def check_stage(reader: Reader, path: str, meta: dict, stage: str, archived: bool = False) -> None:
    """方案只验可审材料，实施绑定批准对象，交付另验准确版本的人工验收。"""
    a = meta["approval"]
    if not meta["modern"] or stage == "proposal":
        return
    if meta["status"] in {"draft", "review", "rejected"} or meta["status"] == "closed" and not archived:
        raise Failure("当前修订未获有效实施批准：" + path)
    require_fact(a, ("提案版本", "方案批准版本", "方案审核人", "方案审核时间", "方案审核证据", "方案批准对象"))
    if a.get("方案审核状态") != "approved" or a["提案版本"] != a["方案批准版本"]:
        raise Failure("方案未批准或批准版本不匹配：" + path)
    ref = a["方案批准对象"]
    if not re.fullmatch(r"[0-9a-f]{40}(?::" + re.escape(path) + r")?", ref):
        raise Failure("方案批准对象必须为完整Git blob SHA或固定提交:本CHG路径")
    original = git(reader.root, "cat-file", "blob", ref).decode("utf-8")
    if plan_digest(original) != plan_digest(reader.read(path)):
        raise Failure("方案实质变化，需重新审核批准版本：" + path)
    if stage == "delivery":
        require_fact(a, ("交付验收人", "交付验收时间", "交付验收证据", "交付验收提交", "验证证据"))
        if a.get("交付验收状态") != "approved":
            raise Failure("交付验收未通过：" + path)
        accepted = a["交付验收提交"]
        if not re.fullmatch(r"[0-9a-f]{40}", accepted):
            raise Failure("交付验收必须绑定完整提交")
        # 元数据补登允许晚于交付版本；任何代码或方案变化都需重新验收。
        if commit(reader.root, accepted) != accepted:
            raise Failure("无效交付提交")
        for platform in ("Linux", "Windows"):
            require_fact(a, (platform + "验证提交", platform + "验证结果", platform + "验证证据"))
            if a[platform + "验证提交"] != accepted or a[platform + "验证结果"] != "passed":
                raise Failure("缺少准确交付提交的原生验证：" + platform)
        # 已交付记录不以未来其他CHG的代码变化推翻，但仍核对原批准/验收事实。
        if archived:
            return
        diff = changes(reader.root, accepted, reader.ref)
        for changed in diff:
            if changed in {INDEX, TRACE}:
                continue
            if not changed.startswith(CHANGE_DIR + "/CHG-") or not changed.endswith(".md"):
                raise Failure("交付验收后版本变化：" + changed)
            prior = Reader(reader.root, accepted).read(changed)
            if plan_digest(prior) != plan_digest(reader.read(changed)) or prior.split("## 12.", 1)[-1] not in reader.read(changed):
                raise Failure("交付验收后方案/历史变化：" + changed)


def load_chg(reader: Reader, path: str, profile: dict, tool=None, stage: str = "proposal") -> tuple[dict, dict]:
    """验证CHG结构和项目约束；不把自填批准字段视为人工授权。"""
    tool = tool or upstream()
    body = reader.read(path)
    parsed = tables(body)
    approval = approval_fields(parsed)
    modern = approval.get("项目CHG格式") == FORMAT
    if not modern:
        legacy_entry(reader, path, profile)
    elif any(approval.get(key) not in {"pending", "approved", "rejected"}
             for key in ("方案审核状态", "交付验收状态")):
        raise Failure("独立审批状态必须为pending/approved/rejected")
    if any(section not in parsed and section not in body.splitlines() for section in tool.CHG_SECTIONS):
        raise Failure(path + "：缺少十二节")
    # 固定框架继续验证基础格式/保护；新审批语义只在项目层处理。
    original_frontmatter = tool.frontmatter
    def normalized(copy):
        metadata, text = original_frontmatter(copy)
        if modern:
            metadata["approved_by"] = approval.get("方案审核人", "")
            metadata["approved_at"] = approval.get("方案审核时间", "")
        return metadata, text
    tool.frontmatter = normalized
    with tempfile.TemporaryDirectory(prefix="openpaw-chg-") as temp:
        copy = Path(temp) / Path(path).name
        copy.write_text(body, encoding="utf-8", newline="\n")
        try:
            meta = tool.validate_chg(copy, profile)
        except tool.GovernanceError as error:
            raise Failure(str(error)) from error
        finally:
            tool.frontmatter = original_frontmatter
    if approval.get("实施授权来源", "") in PLACEHOLDERS:
        raise Failure(path + "：缺少真实实施授权来源；提案可填仅起草未获编码授权")
    historical_tx = re.fullmatch(r"T[0-8]", approval.get("阶段", "")) is not None
    expected_branch = profile["integration_branch"] if historical_tx or profile.get("initial_tx_phase", True) else profile.get("normal_target_branch", "main")
    if meta["target_branch"] != expected_branch:
        raise Failure(path + "：目标分支与项目配置不一致")
    if {r["id"] for r in meta["repositories"]} != {"openpaw"}:
        raise Failure("本profile只允许OpenPaw，Office先Rx另仓审核")
    declared = parsed["## 4. 允许修改文件"]
    keys = [relative(r["文件"]) for r in declared]
    if len(set(keys)) != len(keys) or any(any(c in p for c in "*?[") for p in keys):
        raise Failure("允许文件重复或包含通配；必须准确登记")
    if any(r.get("必要性") not in {"必改", "条件"} for r in declared):
        raise Failure("每个允许文件必须标明必改或条件")
    for row in declared:
        if row["操作"] not in {"新增", "修改", "删除", "重命名源", "重命名目标"}:
            raise Failure("文件操作不受支持")
        if modern:
            for key in ("具体修改方向与复用方式", "完成标准与证据"):
                if row.get(key, "") in PLACEHOLDERS:
                    raise Failure("文件缺少可审核说明：" + key)
        elif any(key in row for key in ("具体修改方向与复用方式", "完成标准与证据")):
            if any(row.get(key, "") in PLACEHOLDERS for key in ("具体修改方向与复用方式", "完成标准与证据")):
                raise Failure("历史说明迁移不能留空")
    protected = {r["path"]: r for r in meta["protected_paths"]}
    for row in meta["files"]:
        level = max((LEVELS[r["level"]] for r in profile.get("protected_paths", [])
                     if fnmatch.fnmatchcase(row["path"], r["pattern"])), default=1)
        if level > 1 and LEVELS.get(protected.get(row["path"], {}).get("level"), 0) < level:
            raise Failure("保护等级低于profile：" + row["path"])
        required = {1: "low", 2: "medium", 3: "high", 4: "critical"}[level]
        risks = {"low": 1, "medium": 2, "high": 3, "critical": 4}
        if risks.get(meta["risk"], 0) < risks[required]:
            raise Failure("变更风险低于保护路径要求")
    # 方案批准早于PR；禁止把PR URL当方案批准的前置条件。
    if meta["status"] == "closed" and not re.fullmatch("[0-9a-f]{40}", approval.get("合入提交", "")):
        raise Failure("closed必须有合入提交")
    if meta["status"] == "closed" and not re.fullmatch(r"https://github\.com/[^/]+/[^/]+/pull/\d+", approval.get("平台PR URL", approval.get("PR审核证据", ""))):
        raise Failure("closed必须有真实PR引用；关闭未合入不能完成")
    commands = json.loads(reader.read("governance/commands.json"))["commands"]
    registry = {c["id"]: c for c in commands}
    for test in meta["tests"]:
        if test["id"] not in registry:
            raise Failure("测试未注册可执行命令：" + test["id"])
        if test["command"] != " ".join(registry[test["id"]]["argv"]):
            raise Failure("CHG测试命令与执行注册表不同：" + test["id"])
    impacts = parsed.get("## 9. 影响、数据流和实现", [])
    if not impacts or any(not r.get("文档") or not r.get("章节与处理理由") for r in impacts):
        raise Failure("缺少文档影响表/理由")
    for row in impacts:
        if row["文档"] != "不涉及" and not reader.exists(relative(row["文档"])):
            raise Failure("文档影响指向不存在文件：" + row["文档"])
    trace = parsed.get("## 11. 追踪、兼容和回滚", [])
    reqs, designs = reader.read(REQ), reader.read(DES)
    for row in trace:
        if not re.fullmatch(r"REQ-OP-\d{3}", row.get("需求编号", "")) or row["需求编号"] not in reqs:
            raise Failure("追踪需求不存在")
        if not re.fullmatch(r"DES-OP-\d{3}", row.get("设计章节", "")) or row["设计章节"] not in designs:
            raise Failure("追踪设计不存在")
        if row.get("测试编号", "") not in registry or not row.get("代码文件或符号"):
            raise Failure("追踪代码/测试不完整")
        code_path = row["代码文件或符号"].split("::", 1)[0]
        if not reader.exists(relative(code_path)):
            raise Failure("追踪代码不存在：" + code_path)
    if not trace:
        raise Failure("缺少需求设计代码测试追踪")
    meta["approval"] = approval
    meta["modern"] = modern
    if modern:
        require_fact(approval, ("提案版本",))
        if Path(path).name != meta["id"] + ".md":
            raise Failure("CHG编号与文件名不一致")
    check_stage(reader, path, meta, "delivery" if modern and meta["status"] == "closed" else stage,
                archived=modern and meta["status"] == "closed")
    return meta, parsed


def profile_for(reader: Reader) -> dict:
    """校验本项目固定范围，平台不能隐式扩成macOS。"""
    profile = tomllib.loads(reader.read(PROFILE))
    if profile.get("platforms") != ["linux", "windows"] or profile.get("schema") != "ai-governance/project/v1":
        raise Failure("项目平台/schema不符合基线")
    if profile.get("chg_format", FORMAT) != FORMAT:
        raise Failure("项目CHG格式不符合基线")
    for path in profile["baseline"]:
        if not reader.exists(relative(path)):
            raise Failure("基线文件不存在：" + path)
    return profile


def validate(reader: Reader, stage: str = "proposal") -> list[tuple[str, dict, dict]]:
    """验证全部CHG、标识、命令和框架来源，返回单事实源。"""
    profile, tool = profile_for(reader), upstream()
    lock = tomllib.loads(reader.read("governance/framework.lock.toml"))
    if hashlib.sha256(reader.read(lock["file"]).encode("utf-8")).hexdigest() != lock["revision"]:
        raise Failure("目标版本框架哈希不匹配")
    paths = [p for p in reader.files() if p.startswith(CHANGE_DIR + "/CHG-") and p.endswith(".md")]
    result, ids = [], set()
    for path in paths:
        meta, parsed = load_chg(reader, path, profile, tool, stage)
        if meta["id"] in ids:
            raise Failure("重复CHG编号：" + meta["id"])
        ids.add(meta["id"])
        result.append((path, meta, parsed))
    if not result:
        raise Failure("所有变更必须有主CHG")
    registry = json.loads(reader.read("governance/identifiers.json"))["ids"]
    if len(registry) != len(set(registry)):
        raise Failure("登记ID重复")
    sources = [reader.read(p) for p in [REQ, DES, "docs/engine-host/audit.md", *paths]]
    for identifier in registry:
        if not any(re.search(r"(?<![A-Za-z0-9-])" + re.escape(identifier) + r"(?![A-Za-z0-9-])", s) for s in sources):
            raise Failure("登记ID没有权威来源：" + identifier)
    commands = json.loads(reader.read("governance/commands.json"))["commands"]
    if len({c["id"] for c in commands}) != len(commands):
        raise Failure("命令ID重复")
    for command in commands:
        if not command["argv"] or command["platforms"] != ["linux", "windows"]:
            raise Failure("命令未明确双平台/argv")
        if command["cwd"] != ".":
            relative(command["cwd"])
        for token in command["argv"]:
            if token.endswith(".py") and not reader.exists(token):
                raise Failure("注册脚本不存在：" + token)
    return result


def parse_status(raw: bytes) -> dict[str, str]:
    """解析NUL分隔真实Git diff，重命名登记两端，复制只算新增目标。"""
    fields, index, result = raw.split(b"\0"), 0, {}
    names = {"A": "新增", "M": "修改", "D": "删除", "T": "修改"}
    while index < len(fields) and fields[index]:
        status = fields[index].decode()
        first = fields[index + 1].decode("utf-8")
        index += 2
        if status[0] in "RC":
            second = fields[index].decode("utf-8")
            index += 1
            if status[0] == "R":
                result[first], result[second] = "重命名源", "重命名目标"
            else:
                result[second] = "新增"
        elif status[0] in names:
            result[first] = names[status[0]]
        else:
            raise Failure("未处理Git状态：" + status)
    return result


def changes(root: Path, base: str, head: str | None) -> dict[str, str]:
    """获取确定比较对象，工作区模式显式包括未跟踪文件。"""
    args = ["diff", "--no-ext-diff", "--no-textconv", "--name-status", "-z", "--find-renames", base]
    if head:
        args.append(head)
    result = parse_status(git(root, *args))
    if not head:
        for path in git(root, "ls-files", "-z", "--others", "--exclude-standard").split(b"\0"):
            if path:
                result[path.decode("utf-8")] = "新增"
    return result


def sections(body: str) -> dict[str, str]:
    """按原始章节保存历史正文，CRLF只作平台换行归一化。"""
    result = {}
    for part in re.split(r"(?m)^## ", body.replace("\r\n", "\n"))[1:]:
        heading, _, content = part.partition("\n")
        result["## " + heading] = content.strip()
    return result


def historical_update(before: str, after: str, explanation: bool) -> None:
    """补登只动审批/追加履历；获批说明迁移可改原因及新增方法列，不能改历史范围。"""
    old, new = sections(before), sections(after)
    old_tables, new_tables = tables(before), tables(after)
    for key in old:
        if key.startswith(("## 1.", "## 12.")) or explanation and key.startswith("## 4."):
            continue
        if key == "## 3. 涉及仓库":
            # 仓库身份不变，PR说明属于完成事实，而非旧代码授权。
            old_rows, new_rows = old_tables.get(key, []), new_tables.get(key, [])
            if [r.get("仓库编号") for r in old_rows] != [r.get("仓库编号") for r in new_rows]:
                raise Failure("历史维护改变原仓库范围")
            left = "\n".join(line for line in old[key].splitlines() if not line.strip().startswith("|"))
            right = "\n".join(line for line in new.get(key, "").splitlines() if not line.strip().startswith("|"))
        else:
            left, right = old[key], new.get(key, "")
        if normalized_markdown(left) != normalized_markdown(right):
            raise Failure("历史维护改变受保护章节：" + key)
    if old.get("## 12. 验证和审批记录", "") not in new.get("## 12. 验证和审批记录", ""):
        raise Failure("历史维护删除原验证/审批履历")
    if explanation:
        key = "## 4. 允许修改文件"
        rows_old, rows_new = tables(before)[key], tables(after)[key]
        identity = ("仓库", "操作", "文件", "模块或符号", "文件负责人", "对其他部分的影响", "必要性")
        if [[r.get(k) for k in identity] for r in rows_old] != [[r.get(k) for k in identity] for r in rows_new]:
            raise Failure("历史说明迁移改变原文件范围/操作/必要性")
        for row in rows_new:
            if any(row.get(k, "") in PLACEHOLDERS for k in ("为什么必须修改", "具体修改方向与复用方式", "完成标准与证据")):
                raise Failure("历史说明迁移缺少原因/方法/完成标准")


def references(value: str) -> dict[str, str]:
    """解析显式CHG@版本关联；不从自然语言猜测交付归属。"""
    if value in PLACEHOLDERS or value == "不涉及":
        return {}
    result = {}
    for item in value.split("、"):
        match = re.fullmatch(r"(CHG-\d{4}-\d{4})@([A-Za-z0-9._-]+)", item.strip())
        if not match or match[1] in result:
            raise Failure("关联修订必须唯一且写为CHG编号@提案版本")
        result[match[1]] = match[2]
    return result


def maintenance_paths(value: str) -> set[str]:
    """历史维护使用准确文件清单，不支持通配或隐式授权。"""
    if value in PLACEHOLDERS or value == "不涉及":
        return set()
    return {relative(p.strip()) for p in value.split("、")}


def pr_check(root: Path, base: str, head: str | None, stage: str = "implementation") -> dict:
    """独立审核共同交付范围与历史补登；技术通过不认证人类或替代PR批准。"""
    base = commit(root, base)
    head = commit(root, head) if head else None
    reader, base_reader = Reader(root, head), Reader(root, base)
    valid, delta = validate(reader), changes(root, base, head)
    changed_chgs = {p for p in delta if p.startswith(CHANGE_DIR + "/CHG-") and p.endswith(".md")}
    if not changed_chgs or any(delta[p] in {"删除", "重命名源", "重命名目标"} for p in changed_chgs):
        raise Failure("每PR必须有主CHG，不得删除或重命名历史CHG")
    candidates = [(p, m, t) for p, m, t in valid if p in changed_chgs]
    primary = []
    for candidate in candidates:
        path, meta, _ = candidate
        a = meta["approval"]
        linked = references(a.get("关联修订", ""))
        maintained = maintenance_paths(a.get("历史元数据维护", "")) | maintenance_paths(a.get("历史说明维护", ""))
        covers = {p for p, m, _ in candidates if m["id"] in linked} | maintained | {path}
        if changed_chgs <= covers:
            primary.append(candidate)
    if len(primary) != 1:
        raise Failure("每PR必须明确一份主CHG；多CHG需显式关联修订或受限历史维护")
    path, meta, parsed = primary[0]
    linked = references(meta["approval"].get("关联修订", ""))
    explanations = maintenance_paths(meta["approval"].get("历史说明维护", ""))
    maintained = explanations | maintenance_paths(meta["approval"].get("历史元数据维护", ""))
    if set(linked) - {m["id"] for _, m, _ in candidates}:
        raise Failure("关联修订没有出现在本次diff")
    if maintained & {path} or any(p not in base_reader.files() or not re.fullmatch(CHANGE_DIR + r"/CHG-\d{4}-\d{4}\.md", p) for p in maintained):
        raise Failure("历史维护只能引用已存在的其他CHG")
    if any(m["id"] in linked and p in maintained for p, m, _ in candidates):
        raise Failure("关联修订不能同时作为历史维护绕过独立审核")
    active = []
    for candidate_path, candidate, candidate_tables in candidates:
        if candidate_path in maintained:
            historical_update(base_reader.read(candidate_path), reader.read(candidate_path), candidate_path in explanations)
            continue
        if candidate_path != path and linked.get(candidate["id"]) != candidate["approval"].get("提案版本"):
            raise Failure("关联修订版本不匹配")
        if stage != "proposal":
            if candidate["status"] in {"draft", "review", "rejected", "closed"}:
                raise Failure("当前CHG未获有效实施批准")
            check_stage(reader, candidate_path, candidate, stage)
            if not candidate["modern"]:
                # 历史实施只追溯固定版本，不能借兼容入口开发新的代码。
                entry = legacy_entry(reader, candidate_path, profile_for(reader))
                original = Reader(root, entry["ref"])
                historical_update(original.read(candidate_path), reader.read(candidate_path), True)
                for changed in set(delta) - {candidate_path, INDEX, TRACE}:
                    if not original.exists(changed) or not reader.exists(changed) or original.read(changed) != reader.read(changed):
                        raise Failure("历史批准不能覆盖新的实施；请先审核新修订：" + changed)
                if stage == "delivery":
                    raise Failure("历史记录不补造新流程交付验收；需明确的新修订验收")
        active.append((candidate_path, candidate, candidate_tables))
    owners = {}
    for _, candidate, candidate_tables in active:
        rows = {r["文件"]: r for r in candidate_tables["## 4. 允许修改文件"]}
        missing = {p for p, r in rows.items() if r["必要性"] == "必改"} - delta.keys()
        if missing:
            raise Failure("必改缺失=" + repr(sorted(missing)))
        for p, row in rows.items():
            owners.setdefault(p, []).append((candidate, row))
    unexpected = set(delta) - owners.keys()
    if unexpected:
        raise Failure("越界文件=" + repr(sorted(unexpected)))
    base_rules = tomllib.loads(base_reader.read(PROFILE)).get("protected_paths", []) if base_reader.exists(PROFILE) else []
    for p, operation in delta.items():
        entries = owners[p]
        for candidate, row in entries:
            if row["操作"] != operation:
                raise Failure("文件操作与diff不一致：" + p)
            if len(entries) > 1 and any(other["id"] not in row.get("共同修改职责", "") for other, _ in entries if other != candidate):
                raise Failure("重叠文件缺少双方明确职责：" + p)
            if any(fnmatch.fnmatchcase(p, r["path"]) for r in candidate["forbidden_paths"]):
                raise Failure("命中禁止范围：" + p)
            level = max((LEVELS[r["level"]] for r in base_rules if fnmatch.fnmatchcase(p, r["pattern"])), default=1)
            protected = {r["path"]: LEVELS[r["level"]] for r in candidate["protected_paths"]}
            if level > 1 and protected.get(p, 0) < level:
                raise Failure("降低base保护级别：" + p)
            if {"low":1,"medium":2,"high":3,"critical":4}.get(candidate["risk"],0) < level:
                raise Failure("降低base风险等级：" + p)
    return {"base": base, "head": head or "WORKTREE", "stage": stage, "change": meta["id"],
            "linked": linked, "historical": sorted(maintained), "files": len(delta),
            "human_review": "技术检查不能认证人工审核", "unused_conditional": sorted(set(owners) - delta.keys())}


def index_text(valid: list) -> str:
    """从CHG生成索引，不另存一份可手改状态。"""
    lines = ["# CHG索引", "", "由 python governance/check.py index 生成；CHG正文是唯一状态源。", "",
             "| 编号 | 标题 | 当前修订状态 | 原交付 | 方案审核 | 交付验收 | PR/合入 | Tx | 目标分支 |", "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for path, meta, _ in valid:
        lines.append(f"| [{meta['id']}]({Path(path).name}) | {meta['title']} | {meta['status']} | {meta['approval'].get('原交付状态','见CHG')} | {meta['approval'].get('方案审核状态','历史追溯')} | {meta['approval'].get('交付验收状态','历史追溯')} | {meta['approval'].get('平台PR状态','见CHG')} | {meta['approval'].get('阶段','无')} | {meta['target_branch']} |")
    return "\n".join(lines) + "\n"


def trace_text(reader: Reader, valid: list) -> str:
    """生成需求到设计/阶段/验收与当前CHG代码追踪，不冒充运行通过。"""
    req_rows = tables(reader.read(REQ))["## 需求清单"]
    design_rows = tables(reader.read(DES))["## 设计清单"]
    lines = ["# 需求设计验证追踪矩阵", "", "自动生成；待实现不表示已验收，实际结果见当前提交CI/验证报告。", "",
             "| 需求 | 设计 | 阶段与平台 | 验收入口 | 状态 |", "| --- | --- | --- | --- | --- |"]
    for req in req_rows:
        key = req["需求编号"]
        designs = [d["设计编号"] for d in design_rows if key in d["关联需求"]]
        if not designs:
            raise Failure("需求没有设计关联：" + key)
        lines.append(f"| {key} | {'、'.join(designs)} | {req['平台与阶段']} | {req['验收入口']} | {req['当前状态']} |")
    lines += ["", "## 已登记CHG的代码与测试", "",
              "| CHG | 需求 | 设计 | 代码或符号 | 测试 |", "| --- | --- | --- | --- | --- |"]
    for _, meta, parsed in valid:
        for row in parsed["## 11. 追踪、兼容和回滚"]:
            lines.append(f"| {meta['id']} | {row['需求编号']} | {row['设计章节']} | {row['代码文件或符号']} | {row['测试编号']} |")
    return "\n".join(lines) + "\n"


def write_generated(path: Path, text: str, check: bool) -> None:
    """检查模式只比较；生成模式显式写入且不执行批准。"""
    if check:
        if not path.is_file() or path.read_text(encoding="utf-8") != text:
            raise Failure("生成文件过期：" + str(path))
    else:
        path.write_text(text, encoding="utf-8")


def main() -> int:
    """解析受限CLI；不提供人工批准或自动合并入口。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("validate").add_argument("--stage", choices=STAGES, default="proposal")
    for name in ("index", "trace"):
        sub.add_parser(name).add_argument("--check", action="store_true")
    pr = sub.add_parser("pr-check")
    pr.add_argument("--base", required=True)
    pr.add_argument("--stage", choices=STAGES, default="implementation")
    choice = pr.add_mutually_exclusive_group(required=True)
    choice.add_argument("--head")
    choice.add_argument("--worktree", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "pr-check":
            print(json.dumps(pr_check(args.root, args.base, args.head, args.stage), ensure_ascii=False, indent=2))
        else:
            reader = Reader(args.root)
            valid = validate(reader, getattr(args, "stage", "proposal"))
            if args.command == "index":
                write_generated(args.root / INDEX, index_text(valid), args.check)
            elif args.command == "trace":
                write_generated(args.root / TRACE, trace_text(reader, valid), args.check)
            print(f"治理技术检查通过：{args.command}；主CHG={len(valid)}；不代表人工批准")
        return 0
    except (Failure, OSError, ValueError, KeyError, IndexError) as error:
        print("治理检查失败：" + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
