#!/usr/bin/env python3
"""多人 AI 开发治理框架的最小校验原型。"""

from __future__ import annotations

import argparse
import datetime as dt
import fnmatch
import re
import subprocess
import sys
import tomllib
from pathlib import Path, PurePosixPath


CHG_SECTIONS = [
    "## 1. 变更审批",
    "## 2. 变更原因",
    "## 3. 涉及仓库",
    "## 4. 允许修改文件",
    "## 5. 明确禁止范围",
    "## 6. API 变化",
    "## 7. 字段变化",
    "## 8. 保护路径",
    "## 9. 影响、数据流和实现",
    "## 10. 验收测试",
    "## 11. 追踪、兼容和回滚",
    "## 12. 验证和审批记录",
]
CHG_ID = re.compile(r"^CHG-\d{4}-\d{4}$")
API_ID = re.compile(r"^(?:API|EVT|DAT|TRT)-[A-Z0-9-]+$")
FIELD_ID = re.compile(r"^(?:API|EVT|DAT|TRT)-[A-Z0-9-]+-F\d{2}$")
CHINESE = re.compile(r"[\u3400-\u9fff]")
CHG_STATUSES = {
    "draft",
    "review",
    "approved",
    "in-progress",
    "verified",
    "closed",
    "rejected",
}
PR_READY_STATUSES = {"approved", "in-progress"}
APPROVAL_STATUSES = {"approved", "in-progress", "verified", "closed"}


class GovernanceError(Exception):
    """表示治理文件不满足强制规则。"""


def chinese(value: object, label: str) -> None:
    """要求说明字段至少包含一个中文字符。"""
    if not isinstance(value, str) or not CHINESE.search(value):
        raise GovernanceError(f"{label} 必须包含中文说明")


def _plain_cell(value: str) -> str:
    """移除 Markdown 表格单元格外层代码标记。"""
    value = value.strip()
    if len(value) >= 2 and value.startswith("`") and value.endswith("`"):
        return value[1:-1].strip()
    return value


def _table_cells(line: str) -> list[str]:
    """拆分不包含跨行内容的 Markdown 表格行。"""
    return [_plain_cell(cell) for cell in line.strip().strip("|").split("|")]


def _markdown_tables(body: str) -> dict[str, list[dict[str, str]]]:
    """按二级标题读取 CHG 中的人类可读表格。"""
    lines = body.splitlines()
    tables: dict[str, list[dict[str, str]]] = {}
    section = ""
    index = 0
    while index < len(lines):
        line = lines[index].strip()
        if line.startswith("## "):
            section = line
        if (
            section
            and line.startswith("|")
            and index + 1 < len(lines)
            and re.fullmatch(r"\|?\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)+\|?", lines[index + 1].strip())
        ):
            headers = _table_cells(line)
            index += 2
            rows: list[dict[str, str]] = []
            while index < len(lines) and lines[index].strip().startswith("|"):
                values = _table_cells(lines[index])
                if len(values) == len(headers):
                    rows.append(dict(zip(headers, values)))
                index += 1
            tables[section] = rows
            continue
        index += 1
    return tables


def _token(value: str, mappings: dict[str, str] | None = None) -> str:
    """从“中文（machine-token）”形式中取得稳定机器值。"""
    match = re.search(r"[（(]([a-z][a-z0-9-]*)[）)]", value)
    if match:
        return match.group(1)
    return (mappings or {}).get(value, value)


def _rows(tables: dict[str, list[dict[str, str]]], section: str, mapping: dict[str, str]) -> list[dict]:
    """将中文列名映射为治理程序内部字段名。"""
    result: list[dict] = []
    for row in tables.get(section, []):
        if row and next(iter(row.values()), "") not in {"", "无", "不涉及"}:
            result.append({target: row.get(source, "") for source, target in mapping.items()})
    return result


def frontmatter(path: Path) -> tuple[dict, str]:
    """从中文 Markdown 表格读取 CHG，表格本身是唯一事实源。"""
    body = path.read_text(encoding="utf-8")
    tables = _markdown_tables(body)
    approval_rows = tables.get("## 1. 变更审批", [])
    approval = {row.get("审批项目", ""): row.get("内容", "") for row in approval_rows}
    if not approval:
        raise GovernanceError(f"{path}: 缺少“## 1. 变更审批”中文表格")

    pending = {"", "待审批", "不适用", "无"}
    reviewers = [item.strip() for item in re.split(r"[、,，]", approval.get("必须审核", "")) if item.strip()]
    metadata: dict = {
        "schema": "ai-governance/chg/v2",
        "id": _plain_cell(approval.get("变更编号", "")),
        "title": approval.get("变更标题", ""),
        "status": _token(approval.get("当前状态", ""), {"草案": "draft", "评审中": "review", "已批准": "approved"}),
        "change_type": _token(approval.get("变更类型", "")),
        "risk": _token(approval.get("风险等级", ""), {"低": "low", "中": "medium", "高": "high", "严重": "critical"}),
        "owner": approval.get("负责人", ""),
        "reviewers": reviewers,
        "baseline": _plain_cell(approval.get("设计基线", "")),
        "ai_used": approval.get("AI 参与", "否") == "是",
        "ai_tool": approval.get("AI 工具", ""),
        "target_branch": _plain_cell(approval.get("目标分支", "")),
        "approved_by": "" if approval.get("批准人", "") in pending else approval.get("批准人", ""),
        "approved_at": "" if approval.get("批准时间", "") in pending else approval.get("批准时间", ""),
        "repositories": _rows(tables, "## 3. 涉及仓库", {"仓库编号": "id", "PR 或说明": "pr"}),
        "files": _rows(tables, "## 4. 允许修改文件", {
            "仓库": "repository", "操作": "action", "文件": "path", "模块或符号": "symbol",
            "文件负责人": "owner", "为什么必须修改": "reason", "对其他部分的影响": "impact",
        }),
        "forbidden_paths": _rows(tables, "## 5. 明确禁止范围", {
            "仓库": "repository", "禁止路径": "path", "为什么禁止": "reason",
        }),
        "apis": _rows(tables, "## 6. API 变化", {
            "接口编号": "id", "类型": "kind", "原始标识符": "identifier", "中文名称": "name_zh",
            "兼容性": "compatibility", "功能和边界": "description_zh",
        }),
        "fields": _rows(tables, "## 7. 字段变化", {
            "字段编号": "id", "所属接口": "api_id", "原始字段名": "wire_name", "中文名称": "name_zh",
            "类型": "type", "必填": "required", "默认值": "default", "约束": "constraint_zh",
            "数据来源": "source_zh", "数据去向": "target_zh",
        }),
        "protected_paths": _rows(tables, "## 8. 保护路径", {
            "仓库": "repository", "文件或路径": "path", "等级": "level", "负责人": "owner",
            "为什么需要保护": "reason",
        }),
        "tests": _rows(tables, "## 10. 验收测试", {
            "测试编号": "id", "执行命令": "command", "场景和通过条件": "scenario_zh",
        }),
    }
    for field in metadata["fields"]:
        field["required"] = field["required"] in {"是", "true", "True"}
    return metadata, body


def required_text(metadata: dict, key: str, path: Path) -> None:
    """检查中文 CHG 表格解析出的必填文本。"""
    value = metadata.get(key)
    if not isinstance(value, str) or not value.strip():
        raise GovernanceError(f"{path}: 缺少必填字段 {key}")


def validate_chg_status(metadata: dict, path: Path, require_approved: bool = False) -> None:
    """检查 CHG 生命周期，并在合并检查时要求人工批准。"""
    status = metadata.get("status")
    if status not in CHG_STATUSES:
        raise GovernanceError(f"{path}: 不支持的 CHG 状态：{status}")
    if status in APPROVAL_STATUSES:
        for key in ("approved_by", "approved_at"):
            required_text(metadata, key, path)
    if require_approved and status not in PR_READY_STATUSES:
        raise GovernanceError(
            f"{path}: CHG 当前状态为 {status}，必须先由人审核并设置为 approved 才能进入 PR 门禁"
        )


def validate_chg(path: Path, profile: dict | None = None) -> dict:
    """校验单个 CHG，并返回中文表格中的结构化数据。"""
    metadata, body = frontmatter(path)
    for key in ("schema", "id", "title", "status", "change_type", "risk", "owner", "baseline"):
        required_text(metadata, key, path)
    validate_chg_status(metadata, path)
    change_id = metadata["id"]
    if not CHG_ID.fullmatch(change_id):
        raise GovernanceError(f"{path}: 变更编号必须符合 CHG-YYYY-NNNN：{change_id}")
    if metadata["schema"] != "ai-governance/chg/v2":
        raise GovernanceError(f"{path}: 不支持的 CHG schema：{metadata['schema']}")
    for section in CHG_SECTIONS:
        if section not in body:
            raise GovernanceError(f"{path}: 缺少章节 {section}")
    for field in ("title", "owner"):
        chinese(metadata[field], f"{path} 的 {field}")

    repositories = metadata.get("repositories", [])
    files = metadata.get("files", [])
    apis = metadata.get("apis", [])
    fields = metadata.get("fields", [])
    forbidden_paths = metadata.get("forbidden_paths", [])
    tests = metadata.get("tests", [])
    if not repositories:
        raise GovernanceError(f"{path}: 至少登记一个仓库")
    if not files:
        raise GovernanceError(f"{path}: 至少登记一个修改文件")
    if not tests:
        raise GovernanceError(f"{path}: 至少登记一个验证测试")

    repository_ids = {item.get("id") for item in repositories}
    file_keys: set[tuple[str, str]] = set()
    for item in files:
        for key in ("repository", "action", "path", "symbol", "owner", "reason", "impact"):
            if not item.get(key):
                raise GovernanceError(f"{path}: 文件条目缺少 {key}")
        if item["repository"] not in repository_ids:
            raise GovernanceError(f"{path}: 文件引用未登记仓库 {item['repository']}")
        file_path = PurePosixPath(str(item["path"]))
        if file_path.is_absolute() or ".." in file_path.parts:
            raise GovernanceError(f"{path}: 文件路径必须是仓库内相对路径：{item['path']}")
        chinese(item["reason"], f"{path}: 文件修改原因")
        chinese(item["impact"], f"{path}: 文件影响分析")
        file_keys.add((item["repository"], str(file_path)))

    for item in forbidden_paths:
        for key in ("repository", "path", "reason"):
            if not item.get(key):
                raise GovernanceError(f"{path}: 禁止路径条目缺少 {key}")
        if item["repository"] not in repository_ids:
            raise GovernanceError(f"{path}: 禁止路径引用未登记仓库 {item['repository']}")
        forbidden_path = PurePosixPath(str(item["path"]))
        if forbidden_path.is_absolute() or ".." in forbidden_path.parts:
            raise GovernanceError(f"{path}: 禁止路径必须是仓库内相对路径：{item['path']}")
        if any(
            repository == item["repository"]
            and fnmatch.fnmatch(allowed_path, str(forbidden_path))
            for repository, allowed_path in file_keys
        ):
            raise GovernanceError(f"{path}: 允许文件命中禁止路径模式：{item['path']}")
        chinese(item["reason"], f"{path}: 禁止路径原因")

    protected_paths = metadata.get("protected_paths", [])
    protected_keys: set[tuple[str, str]] = set()
    for item in protected_paths:
        for key in ("repository", "path", "level", "owner", "reason"):
            if not item.get(key):
                raise GovernanceError(f"{path}: 保护路径条目缺少 {key}")
        if item["repository"] not in repository_ids:
            raise GovernanceError(f"{path}: 保护路径引用未登记仓库 {item['repository']}")
        protected_path = PurePosixPath(str(item["path"]))
        if protected_path.is_absolute() or ".." in protected_path.parts:
            raise GovernanceError(f"{path}: 保护路径必须是仓库内相对路径：{item['path']}")
        if item["level"] not in {"L1", "L2", "L3", "L4"}:
            raise GovernanceError(f"{path}: 不支持的保护等级：{item['level']}")
        protected_key = (item["repository"], str(protected_path))
        if protected_key in protected_keys:
            raise GovernanceError(f"{path}: 保护路径重复：{item['repository']}:{item['path']}")
        protected_keys.add(protected_key)
        chinese(item["owner"], f"{path}: 保护路径负责人")
        chinese(item["reason"], f"{path}: 保护路径原因")

    api_ids: set[str] = set()
    for item in apis:
        for key in ("id", "kind", "identifier", "name_zh", "compatibility", "description_zh"):
            if not item.get(key):
                raise GovernanceError(f"{path}: API 条目缺少 {key}")
        if not API_ID.fullmatch(item["id"]):
            raise GovernanceError(f"{path}: API 编号格式错误：{item['id']}")
        if item["id"] in api_ids:
            raise GovernanceError(f"{path}: API 编号重复：{item['id']}")
        api_ids.add(item["id"])
        chinese(item["name_zh"], f"{path}: API 中文名称")
        chinese(item["description_zh"], f"{path}: API 中文说明")

    field_ids: set[str] = set()
    for item in fields:
        for key in (
            "id",
            "api_id",
            "wire_name",
            "name_zh",
            "type",
            "constraint_zh",
            "source_zh",
            "target_zh",
        ):
            if key not in item or item[key] in (None, ""):
                raise GovernanceError(f"{path}: 字段条目缺少 {key}")
        if not FIELD_ID.fullmatch(item["id"]):
            raise GovernanceError(f"{path}: 字段编号格式错误：{item['id']}")
        if item["id"] in field_ids:
            raise GovernanceError(f"{path}: 字段编号重复：{item['id']}")
        if item["api_id"] not in api_ids:
            raise GovernanceError(f"{path}: 字段引用不存在的 API：{item['api_id']}")
        field_ids.add(item["id"])
        for key in ("name_zh", "constraint_zh", "source_zh", "target_zh"):
            chinese(item[key], f"{path}: 字段 {item['id']} 的 {key}")

    for item in tests:
        for key in ("id", "command", "scenario_zh"):
            if not item.get(key):
                raise GovernanceError(f"{path}: 测试条目缺少 {key}")
        chinese(item["scenario_zh"], f"{path}: 测试场景")

    if profile is not None:
        validate_protected_changes(path, metadata, profile)
    return metadata


def validate_protected_changes(path: Path, metadata: dict, profile: dict) -> None:
    """确保触及保护路径的文件在 CHG 中声明了保护等级。"""
    protected = metadata.get("protected_paths", [])
    declared = {(item.get("repository"), item.get("path")) for item in protected}
    rules = profile.get("protected_paths", [])
    for item in metadata.get("files", []):
        for rule in rules:
            if rule.get("repository") != item.get("repository"):
                continue
            pattern = str(rule.get("pattern", ""))
            if fnmatch.fnmatch(item["path"], pattern) and (item["repository"], item["path"]) not in declared:
                raise GovernanceError(
                    f"{path}: 文件 {item['repository']}:{item['path']} 命中 {rule.get('level')} 保护规则，"
                    "必须在 CHG protected_paths 中登记"
                )


def load_profile(path: Path) -> dict:
    """读取项目 profile 并检查引用的根目录和基线。"""
    profile = tomllib.loads(path.read_text(encoding="utf-8"))
    if profile.get("schema") != "ai-governance/project/v1":
        raise GovernanceError(f"{path}: 不支持的项目 schema")
    for key in ("project_id", "display_name", "changes_dir"):
        if not profile.get(key):
            raise GovernanceError(f"{path}: 缺少项目字段 {key}")
    root = path.parent.parent
    for repository in profile.get("repositories", []):
        repository_root = (root / repository["root"]).resolve()
        if not repository_root.is_dir():
            raise GovernanceError(f"项目仓库目录不存在：{repository_root}")
    for baseline in profile.get("baseline", []):
        baseline_path = (root / baseline).resolve()
        if not baseline_path.is_file():
            raise GovernanceError(f"设计基线文件不存在：{baseline_path}")
    for contract_dir in profile.get("contract_dirs", []):
        contract_path = (root / contract_dir).resolve()
        if not contract_path.is_dir():
            raise GovernanceError(f"契约目录不存在：{contract_path}")
    return profile


def change_paths(changes_dir: Path) -> list[Path]:
    """收集 CHG 文件，排除模板和索引。"""
    return sorted(
        path
        for path in changes_dir.glob("CHG-*.md")
        if path.name != "CHG-template.md"
    )


def validate_project(profile_path: Path) -> int:
    """校验项目 profile 下的全部 CHG。"""
    profile = load_profile(profile_path)
    root = profile_path.parent.parent
    changes_dir = (root / profile["changes_dir"]).resolve()
    if not changes_dir.is_dir():
        raise GovernanceError(f"CHG 目录不存在：{changes_dir}")
    seen: dict[str, Path] = {}
    for change in change_paths(changes_dir):
        metadata = validate_chg(change, profile)
        identifier = metadata["id"]
        if identifier in seen:
            raise GovernanceError(f"CHG 编号重复：{identifier}（{seen[identifier]} 和 {change}）")
        seen[identifier] = change
        print(f"通过：{change.name}（{identifier}）")
    if not seen:
        raise GovernanceError(f"CHG 目录没有可验证文件：{changes_dir}")
    print(f"项目治理校验通过：{profile['project_id']}，共 {len(seen)} 个 CHG")
    return 0


def index_changes(changes_dir: Path, output: Path) -> int:
    """根据 CHG 中文审批表生成稳定索引。"""
    rows: list[tuple[str, str, str, str, str, int, int]] = []
    for change in change_paths(changes_dir):
        metadata = validate_chg(change)
        rows.append(
            (
                metadata["id"],
                metadata["title"],
                metadata["status"],
                metadata["risk"],
                metadata["owner"],
                len(metadata.get("apis", [])),
                len(metadata.get("files", [])),
            )
        )
    rows.sort()
    lines = [
        "# CHG 索引",
        "",
        "本文件由 `python tools/governance.py index` 生成，不作为独立事实源。",
        "",
        "| 变更编号 | 标题 | 状态 | 风险 | 负责人 | 接口数量 | 文件数量 |",
        "| --- | --- | --- | --- | --- | ---: | ---: |",
    ]
    lines.extend(
        f"| {identifier} | {title} | {status} | {risk} | {owner} | {api_count} | {file_count} |"
        for identifier, title, status, risk, owner, api_count, file_count in rows
    )
    output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"已生成：{output}（{len(rows)} 个 CHG）")
    return 0


def validate_diff(change_path: Path, changed_files: list[str]) -> int:
    """比较实际变更文件和 CHG 文件总表，防止 AI 越界修改。"""
    metadata, _ = frontmatter(change_path)
    declared = {
        f"{item['repository']}:{PurePosixPath(str(item['path']))}"
        for item in metadata.get("files", [])
    }
    actual = {item.replace("\\", "/") for item in changed_files if item.strip()}
    unexpected = sorted(actual - declared)
    missing = sorted(declared - actual)
    forbidden = metadata.get("forbidden_paths", [])
    explicit_forbidden = sorted(
        item
        for item in actual
        if ":" in item
        and any(
            item.split(":", 1)[0] == rule.get("repository")
            and fnmatch.fnmatch(item.split(":", 1)[1], str(rule.get("path", "")))
            for rule in forbidden
        )
    )
    if explicit_forbidden:
        raise GovernanceError(
            f"{change_path}: 实际修改命中 CHG 明确禁止路径：{', '.join(explicit_forbidden)}"
        )
    if unexpected:
        raise GovernanceError(
            f"{change_path}: 实际修改超出 CHG 文件总表：{', '.join(unexpected)}"
        )
    if missing:
        raise GovernanceError(
            f"{change_path}: CHG 声明的文件没有出现在实际 diff：{', '.join(missing)}"
        )
    print(f"变更边界检查通过：{change_path.name}，共 {len(actual)} 个文件")
    return 0


def git_diff(change_path: Path, repository: str, root: Path, base: str, head: str) -> int:
    """从 Git 获取 PR 文件并应用 CHG 边界检查。"""
    changed = [f"{repository}:{line}" for line in git_changed_paths(root, base, head)]
    return validate_diff(change_path, changed)


def git_changed_paths(root: Path, base: str, head: str) -> list[str]:
    """读取两个 Git 提交之间的相对文件路径。"""
    try:
        subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
    except FileNotFoundError as error:
        raise GovernanceError("未找到 git，无法读取实际 PR diff") from error
    except subprocess.CalledProcessError as error:
        raise GovernanceError(f"当前目录不是 Git 仓库，pr-check 需要在 Git 工作树中运行：{root}") from error
    try:
        result = subprocess.run(
            ["git", "-c", "core.quotePath=false", "diff", "--name-only", f"{base}...{head}"],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
    except FileNotFoundError as error:
        raise GovernanceError("未找到 git，无法读取实际 PR diff") from error
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() or error.stdout.strip()
        first_line = detail.splitlines()[0] if detail else "未知错误"
        raise GovernanceError(f"git diff 失败：{first_line}") from error
    return [line.strip().replace("\\", "/") for line in result.stdout.splitlines() if line.strip()]


def repository_declared_paths(metadata: dict, repository: str) -> set[str]:
    """取得 CHG 中某个仓库的带仓库编号文件清单。"""
    return {
        f"{repository}:{PurePosixPath(str(item['path']))}"
        for item in metadata.get("files", [])
        if item.get("repository") == repository
    }


def protected_matches(profile: dict, repository: str, path: str) -> list[dict]:
    """返回命中项目保护路径的规则。"""
    return [
        rule
        for rule in profile.get("protected_paths", [])
        if rule.get("repository") == repository
        and fnmatch.fnmatch(path, str(rule.get("pattern", "")))
    ]


def changed_chg_paths(changes_dir: Path, changed_paths: list[str], root: Path) -> list[Path]:
    """从 Git 文件清单中找出本次 PR 修改的 CHG 文件。"""
    try:
        relative_changes_dir = changes_dir.relative_to(root).as_posix()
    except ValueError as error:
        raise GovernanceError(f"CHG 目录不在项目根目录内：{changes_dir}") from error
    prefix = f"{relative_changes_dir.rstrip('/')}/"
    return sorted(
        (root / path).resolve()
        for path in changed_paths
        if path.startswith(prefix)
        and PurePosixPath(path).name.startswith("CHG-")
        and path.endswith(".md")
    )


def pr_check(profile_path: Path, repository: str, base: str, head: str, root: Path | None = None) -> int:
    """执行 PR 的 Git diff、CHG 审批和保护路径闭环检查。"""
    profile = load_profile(profile_path)
    project_root = (root or profile_path.parent.parent).resolve()
    repository_ids = {item.get("id") for item in profile.get("repositories", [])}
    if repository not in repository_ids:
        raise GovernanceError(f"项目 profile 未登记仓库：{repository}")
    changes_dir = (project_root / profile["changes_dir"]).resolve()
    changed_paths = git_changed_paths(project_root, base, head)
    if not changed_paths:
        raise GovernanceError("Git diff 没有文件，无法执行 PR 变更检查")

    chg_paths = changed_chg_paths(changes_dir, changed_paths, project_root)
    protected_changes = [
        (path, protected_matches(profile, repository, path))
        for path in changed_paths
        if protected_matches(profile, repository, path)
    ]
    if not chg_paths:
        if protected_changes:
            paths = ", ".join(path for path, _ in protected_changes)
            raise GovernanceError(f"PR 修改了保护路径但没有修改 CHG：{paths}")
        print("PR 未触及保护路径，也未发现 CHG；按普通内部变更处理")
        return 0
    if len(chg_paths) != 1:
        names = ", ".join(path.name for path in chg_paths)
        raise GovernanceError(f"一个 PR 必须对应一份 CHG，当前发现：{names}")

    change_path = chg_paths[0]
    metadata = validate_chg(change_path, profile)
    validate_chg_status(metadata, change_path, require_approved=True)
    actual = [f"{repository}:{path}" for path in changed_paths]
    declared = repository_declared_paths(metadata, repository)
    explicit_forbidden = sorted(
        item
        for item in actual
        if any(
            fnmatch.fnmatch(item.split(":", 1)[1], str(rule.get("path", "")))
            for rule in metadata.get("forbidden_paths", [])
            if rule.get("repository") == repository
        )
    )
    if explicit_forbidden:
        raise GovernanceError(
            f"{change_path}: Git diff 命中 CHG 明确禁止路径：{', '.join(explicit_forbidden)}"
        )
    unexpected = sorted(set(actual) - declared)
    missing = sorted(declared - set(actual))
    if unexpected:
        raise GovernanceError(
            f"{change_path}: 实际修改超出 CHG 文件总表：{', '.join(unexpected)}"
        )
    if missing:
        raise GovernanceError(
            f"{change_path}: CHG 声明的文件没有出现在实际 diff：{', '.join(missing)}"
        )
    print(f"PR 治理检查通过：{change_path.name}，CHG 已批准，文件范围共 {len(actual)} 个")
    return 0


def task_packet(change_path: Path, output: Path) -> int:
    """从 CHG 生成给 AI 使用的受控任务包。"""
    metadata, _ = frontmatter(change_path)
    validate_chg(change_path)
    validate_chg_status(metadata, change_path, require_approved=True)
    files = metadata.get("files", [])
    apis = metadata.get("apis", [])
    fields = metadata.get("fields", [])
    forbidden_paths = metadata.get("forbidden_paths", [])
    tests = metadata.get("tests", [])
    lines = [
        f"# AI 受控任务包：{metadata['id']}：{metadata['title']}",
        "",
        "本文件由 CHG 生成。AI 只能在允许文件、允许符号和本任务接口范围内工作。",
        "发现必须修改未列出文件时，必须停止并要求更新 CHG，不得自行扩大范围。",
        "",
        "## 任务目标",
        "",
        f"- 变更类型：{metadata['change_type']}",
        f"- 风险等级：{metadata['risk']}",
        f"- 设计基线：{metadata['baseline']}",
        f"- 负责人：{metadata['owner']}",
        "",
        "## 允许修改的文件和符号",
        "",
        "| 仓库 | 文件 | 符号 | 修改原因 |",
        "| --- | --- | --- | --- |",
    ]
    lines.extend(
        f"| {item['repository']} | `{item['path']}` | `{item['symbol']}` | {item['reason']} |"
        for item in files
    )
    lines.extend(["", "## 接口和字段约束", "", "| 接口 ID | 原始标识符 | 中文名称 | 中文说明 |", "| --- | --- | --- | --- |"])
    lines.extend(
        f"| {item['id']} | `{item['identifier']}` | {item['name_zh']} | {item['description_zh']} |"
        for item in apis
    )
    lines.extend(
        f"| {item['id']} | `{item['wire_name']}` | {item['name_zh']} | {item['constraint_zh']} |"
        for item in fields
    )
    lines.extend(
        [
            "",
            "## 禁止事项",
            "",
            "本次明确禁止的关键路径：",
            "",
            "| 仓库 | 禁止路径 | 禁止原因 |",
            "| --- | --- | --- |",
        ]
    )
    lines.extend(
        f"| {item['repository']} | `{item['path']}` | {item['reason']} |"
        for item in forbidden_paths
    )
    lines.extend(
        [
            "",
            "- 不得修改上述清单之外的文件、模块或符号。",
            "- 不得新增未登记的 API、字段、依赖、状态或数据写入。",
            "- 不得改变设计基线定义的数据流、权限、超时、重试、并发和取消语义。",
            "- 不得删除、弱化或改写已有测试断言来获得通过。",
            "- 所有新增或修改的公共接口必须使用中文说明并关联 API/需求/CHG 编号。",
            "",
            "## 必须执行的测试",
            "",
            "| 测试 ID | 命令 | 场景和通过条件 |",
            "| --- | --- | --- |",
        ]
    )
    lines.extend(f"| {item['id']} | `{item['command']}` | {item['scenario_zh']} |" for item in tests)
    lines.extend(
        [
            "",
            "## 完成条件",
            "",
            "1. 实际 diff 与 CHG 文件总表完全一致。",
            "2. 契约、字段中文说明、注释和测试全部通过。",
            "3. 变更未越过设计基线和保护路径。",
            "4. 失败、兼容、回滚和证据记录已补全。",
        ]
    )
    output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"已生成 AI 任务包：{output}")
    return 0


def approve_change(change_path: Path, approver: str, approved_at: str | None) -> int:
    """由人工批准 CHG，并直接更新中文审批表。"""
    metadata, _ = frontmatter(change_path)
    validate_chg(change_path)
    status = metadata.get("status")
    if status not in {"draft", "review"}:
        raise GovernanceError(f"{change_path}: 只有 draft 或 review 状态可以执行批准")
    if not approver.strip():
        raise GovernanceError("批准人不能为空")
    approval_date = approved_at or dt.date.today().isoformat()
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", approval_date):
        raise GovernanceError("批准日期必须使用 YYYY-MM-DD 格式")

    lines = change_path.read_text(encoding="utf-8").splitlines()
    replacements = {
        "当前状态": "已批准（approved）",
        "批准人": approver,
        "批准时间": approval_date,
    }
    found: set[str] = set()
    in_approval = False
    for index, line in enumerate(lines):
        if line.strip() == "## 1. 变更审批":
            in_approval = True
            continue
        if in_approval and line.startswith("## "):
            break
        if not in_approval or not line.strip().startswith("|"):
            continue
        cells = _table_cells(line)
        if len(cells) == 2 and cells[0] in replacements:
            lines[index] = f"| {cells[0]} | {replacements[cells[0]]} |"
            found.add(cells[0])
    missing = set(replacements) - found
    if missing:
        raise GovernanceError(f"{change_path}: 中文审批表缺少项目：{', '.join(sorted(missing))}")
    change_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"CHG 已批准：{change_path.name}，批准人：{approver}，日期：{approval_date}")
    return 0


def contract_check(contract_dir: Path) -> int:
    """校验 JSON Schema 的编号、中文标题和字段说明。"""
    schemas = sorted(contract_dir.rglob("*.json"))
    if not schemas:
        raise GovernanceError(f"契约目录没有 JSON Schema：{contract_dir}")
    seen: set[str] = set()
    checked = 0
    for path in schemas:
        try:
            import json

            document = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            raise GovernanceError(f"{path}: JSON Schema 无法解析：{error}") from error
        identifier = document.get("$id")
        if not isinstance(identifier, str) or not identifier:
            raise GovernanceError(f"{path}: 缺少契约 $id")
        if identifier in seen:
            raise GovernanceError(f"契约 $id 重复：{identifier}")
        seen.add(identifier)
        for key in ("title", "description"):
            chinese(document.get(key), f"{path}: 契约 {key}")
        for property_name, definition in _schema_properties(document):
            if isinstance(definition, dict) and "description" in definition:
                chinese(definition["description"], f"{path}: 字段 {property_name} 说明")
        checked += 1
        print(f"契约通过：{path.name}（{identifier}）")
    print(f"契约检查通过：{checked} 个 JSON Schema")
    return 0


def _schema_properties(document: object) -> list[tuple[str, object]]:
    """递归收集 JSON Schema 中的 properties。"""
    found: list[tuple[str, object]] = []
    if not isinstance(document, dict):
        return found
    properties = document.get("properties")
    if isinstance(properties, dict):
        found.extend(properties.items())
        for definition in properties.values():
            found.extend(_schema_properties(definition))
    for key in ("oneOf", "anyOf", "allOf", "prefixItems"):
        values = document.get(key)
        if isinstance(values, list):
            for value in values:
                found.extend(_schema_properties(value))
    items = document.get("items")
    if isinstance(items, dict):
        found.extend(_schema_properties(items))
    return found


def comment_files(
    root: Path,
    language: str,
    all_files: bool,
    files: list[str],
    report_only: bool,
) -> int:
    """检查 Rust 或 TypeScript 公共声明前是否有中文文档。"""
    if files:
        candidates = [root / item for item in files]
    elif all_files:
        suffix = ".rs" if language == "rust" else ".ts"
        candidates = [
            path
            for path in root.rglob(f"*{suffix}")
            if "node_modules" not in path.parts and "target" not in path.parts
        ]
    else:
        raise GovernanceError("comments 命令必须提供 --all 或 --file")

    if language == "rust":
        declaration = re.compile(r"^\s*pub\s+(?:(?:async)\s+)?(?:struct|enum|trait|fn|type|const|static|mod)\b")
        markers = ("///", "//!")
    elif language == "typescript":
        declaration = re.compile(r"^\s*export\s+(?:interface|type|class|function|const)\b")
        markers = ("/**", "//!", "//")
    else:
        raise GovernanceError(f"暂不支持语言：{language}")

    failures: list[str] = []
    checked = 0
    for path in candidates:
        if not path.is_file():
            continue
        lines = path.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            if not declaration.search(line):
                continue
            checked += 1
            previous = lines[max(0, index - 6) : index]
            comment = "\n".join(previous)
            if not any(marker in comment for marker in markers) or not CHINESE.search(comment):
                failures.append(f"{path}:{index + 1}: 公共声明缺少中文注释")
    if failures and report_only:
        print(f"中文注释报告（报告模式）：发现 {len(failures)} 个缺口")
        print("\n".join(failures))
        return 0
    if failures:
        raise GovernanceError("\n".join(failures))
    print(f"中文注释检查通过：{checked} 个公共声明")
    return 0


def comment_git_diff(root: Path, language: str, base: str, head: str) -> int:
    """只检查 Git diff 中新增或修改的源文件，避免历史缺口阻塞新变更。"""
    changed = git_changed_paths(root, base, head)
    suffixes = (".rs",) if language == "rust" else (".ts", ".tsx")
    files = [
        path
        for path in changed
        if path.lower().endswith(suffixes)
        and "node_modules/" not in path
        and "target/" not in path
    ]
    if not files:
        print(f"Git diff 中文注释检查通过：没有需要检查的 {language} 源文件")
        return 0
    return comment_files(root, language, False, files, False)


def parser() -> argparse.ArgumentParser:
    """创建命令行解析器。"""
    root = argparse.ArgumentParser(description="多人 AI 开发治理框架校验原型")
    sub = root.add_subparsers(dest="command", required=True)

    validate = sub.add_parser("validate", help="校验项目 profile 和全部 CHG")
    validate.add_argument("--profile", type=Path, required=True)

    index = sub.add_parser("index", help="生成 CHG 索引")
    index.add_argument("--changes", type=Path, required=True)
    index.add_argument("--output", type=Path, required=True)

    diff = sub.add_parser("diff", help="检查实际文件是否越过 CHG 范围")
    diff.add_argument("--change", type=Path, required=True)
    diff.add_argument("--changed-file", action="append", default=[])

    git_change = sub.add_parser("git-diff", help="读取 Git PR diff 并检查 CHG 范围")
    git_change.add_argument("--change", type=Path, required=True)
    git_change.add_argument("--repository", required=True)
    git_change.add_argument("--root", type=Path, required=True)
    git_change.add_argument("--base", default="origin/main")
    git_change.add_argument("--head", default="HEAD")

    pr = sub.add_parser("pr-check", help="从 Git PR 自动检查 CHG 审批和实际变更范围")
    pr.add_argument("--profile", type=Path, required=True)
    pr.add_argument("--repository", required=True)
    pr.add_argument("--base", default="origin/main")
    pr.add_argument("--head", default="HEAD")
    pr.add_argument("--root", type=Path)

    packet = sub.add_parser("packet", help="从 CHG 生成 AI 受控任务包")
    packet.add_argument("--change", type=Path, required=True)
    packet.add_argument("--output", type=Path, required=True)

    approve = sub.add_parser("approve", help="人工批准 CHG 并自动写入批准字段")
    approve.add_argument("--change", type=Path, required=True)
    approve.add_argument("--by", required=True, help="批准人姓名或团队")
    approve.add_argument("--at", help="批准日期，格式 YYYY-MM-DD；默认使用当天")

    contracts = sub.add_parser("contracts", help="检查 JSON Schema 编号和中文字段说明")
    contracts.add_argument("--dir", type=Path, required=True)

    comments = sub.add_parser("comments", help="检查公共声明中文注释")
    comments.add_argument("--root", type=Path, required=True)
    comments.add_argument("--language", choices=("rust", "typescript"), required=True)
    comments.add_argument("--all", action="store_true")
    comments.add_argument("--file", action="append", default=[])
    comments.add_argument(
        "--report-only",
        action="store_true",
        help="发现缺口时输出报告但不阻断，用于历史代码迁移阶段",
    )

    comments_diff = sub.add_parser("comments-diff", help="只检查 Git diff 中的公共声明中文注释")
    comments_diff.add_argument("--root", type=Path, required=True)
    comments_diff.add_argument("--language", choices=("rust", "typescript"), required=True)
    comments_diff.add_argument("--base", default="origin/main")
    comments_diff.add_argument("--head", default="HEAD")
    return root


def main() -> int:
    """执行治理命令并输出可读错误。"""
    args = parser().parse_args()
    try:
        if args.command == "validate":
            return validate_project(args.profile.resolve())
        if args.command == "index":
            return index_changes(args.changes.resolve(), args.output.resolve())
        if args.command == "diff":
            return validate_diff(args.change.resolve(), args.changed_file)
        if args.command == "git-diff":
            return git_diff(args.change.resolve(), args.repository, args.root.resolve(), args.base, args.head)
        if args.command == "pr-check":
            return pr_check(
                args.profile.resolve(),
                args.repository,
                args.base,
                args.head,
                args.root.resolve() if args.root else None,
            )
        if args.command == "packet":
            return task_packet(args.change.resolve(), args.output.resolve())
        if args.command == "approve":
            return approve_change(args.change.resolve(), args.by, args.at)
        if args.command == "contracts":
            return contract_check(args.dir.resolve())
        if args.command == "comments":
            return comment_files(
                args.root.resolve(),
                args.language,
                args.all,
                args.file,
                args.report_only,
            )
        if args.command == "comments-diff":
            return comment_git_diff(args.root.resolve(), args.language, args.base, args.head)
    except (GovernanceError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"治理校验失败：{error}", file=sys.stderr)
        return 1
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
