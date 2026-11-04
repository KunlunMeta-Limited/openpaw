#!/usr/bin/env python3
"""执行已登记命令并保留真实退出码；报告目录不可覆盖，默认在仓库外。"""
from __future__ import annotations
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent


def execute(argv, cwd, log, timeout):
    """不用shell；超时为失败，原始stdout/stderr分别保存供人工查阅。"""
    started = time.monotonic()
    with log.with_suffix('.stdout.log').open('wb') as out, log.with_suffix('.stderr.log').open('wb') as err:
        try:
            # Windows runner的管道默认可能是CP1252；Python子命令也必须能输出中文。
            child_env = dict(os.environ, PYTHONUTF8='1', PYTHONIOENCODING='utf-8')
            process = subprocess.Popen(argv, cwd=cwd, stdout=out, stderr=err, env=child_env, start_new_session=os.name != 'nt')
            try:
                code = process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                # 只执行受信任的注册验证命令。原生Windows taskkill回收测试子树；
                # Linux使用独立会话组，不能只终止cargo而留下测试进程。
                if os.name == 'nt':
                    subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], capture_output=True)
                else:
                    import signal
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                process.wait()
                code = 124
        except OSError as error:
            err.write(str(error).encode('utf-8'))
            code = 127
    return {'argv': argv, 'exit_code': code, 'seconds': round(time.monotonic()-started, 3)}


def validate_registry(commands):
    """验证日志标识、套件与argv，避免重复编号覆盖证据或目录越界。"""
    seen=set()
    if not commands:
        raise ValueError('命令注册表为空')
    for command in commands:
        identifier=command['id']
        if not re.fullmatch(r'TEST-[A-Z0-9-]+',identifier) or identifier in seen:
            raise ValueError('命令编号不合法或重复')
        seen.add(identifier)
        if command['suite'] not in {'rust','quality','contracts','governance'}:
            raise ValueError('未知验证套件')
        if not isinstance(command['argv'],list) or not command['argv'] or not all(isinstance(a,str) and a for a in command['argv']):
            raise ValueError('argv必须是非空字符串数组')
        if command['platforms'] != ['linux','windows']:
            raise ValueError('必须明确双平台')


def command_argv(command, substitutions, source="head", stage="implementation"):
    """保留注册命令职责，统一工作区/head和治理阶段，实际参数进入证据。"""
    argv = [substitutions.get(arg, arg) for arg in command['argv']]
    if 'governance/check.py' in argv and 'validate' in argv and stage == 'delivery':
        base = substitutions.get('{base}')
        head = substitutions.get('{head}')
        if not base or base == '{base}' or (source != 'worktree' and (not head or head == '{head}')):
            raise ValueError('交付比较缺少准确base/head；不能回退为无范围历史审计')
        argv += ['--base', base]
        argv += ['--worktree'] if source == 'worktree' else ['--head', head]
    if source == 'worktree' and '--head' in argv:
        index = argv.index('--head')
        argv[index:index+2] = ['--worktree']
    if 'governance/check.py' in argv and any(name in argv for name in ['validate', 'pr-check']):
        argv += ['--stage', stage]
    return argv


def source_context(root, source, head):
    """head只允许匹配的干净检出；不stash或删除用户内容来制造干净结果。"""
    def git(*args):
        return subprocess.check_output(['git', *args], cwd=root).decode().strip()
    resolved = git('rev-parse', '--verify', '--end-of-options', head + '^{commit}')
    current = git('rev-parse', 'HEAD')
    dirty = bool(git('status', '--porcelain', '--untracked-files=all'))
    if source == 'head' and (resolved != current or dirty):
        raise ValueError('head验证要求检出指定提交且工作区干净；请在独立干净检出中运行，不会自动处理用户改动')
    return {'source': source, 'requested_head': resolved, 'checkout_head': current, 'dirty': dirty}


def run_one(command, root, output, timeout, substitutions, source="head", stage="implementation"):
    """展开固定占位符，校验cwd，异常和依赖隔离失败均返回非零结果。"""
    argv = command_argv(command, substitutions, source, stage)
    cwd = (root / command['cwd']).resolve()
    if not cwd.is_relative_to(root.resolve()):
        raise ValueError('注册cwd越出仓库')
    log = output / command['id']
    result = execute(argv, cwd, log, timeout)
    result['id'] = command['id']
    result.update(source=source, stage=stage)
    forbidden = command.get('reject_stdout_prefix')
    if forbidden:
        lines = log.with_suffix('.stdout.log').read_text(encoding='utf-8', errors='replace').splitlines()
        if any(line.startswith(forbidden) for line in lines):
            result.update(exit_code=1, constraint_failure='stdout含禁止依赖前缀：' + forbidden)
    return result


def metadata(root):
    """只记工具版本与工作树摘要，不采集环境变量、模型密钥或远端凭据。"""
    result = {'platform': platform.platform(), 'python': sys.version, 'commands': {}}
    for name, argv in [('git_head', ['git','rev-parse','HEAD']), ('rustc',['rustc','--version']),
                       ('cargo',['cargo','--version']), ('node',['node','--version']), ('npm',['npm','--version'])]:
        try:
            p = subprocess.run(argv, cwd=root, capture_output=True, timeout=30)
            result['commands'][name] = {'exit_code':p.returncode, 'stdout':p.stdout.decode('utf-8','replace').strip()}
        except (OSError, subprocess.TimeoutExpired) as error:
            result['commands'][name] = {'error':type(error).__name__}
    files = subprocess.check_output(['git','ls-files','-z','--cached','--others','--exclude-standard'],cwd=root)
    digest = hashlib.sha256()
    for raw in sorted(set(files.split(b'\0')) - {b''}):
        path = root / os.fsdecode(raw)
        digest.update(raw + b'\0')
        if path.is_symlink():
            digest.update(os.fsencode(os.readlink(path)))
        elif path.is_file():
            digest.update(hashlib.sha256(path.read_bytes()).digest())
        else:
            digest.update(b'DELETED')
    result['worktree_sha256'] = digest.hexdigest()
    result['dirty'] = bool(subprocess.check_output(['git','status','--porcelain'],cwd=root))
    return result


def main():
    # 显式覆盖重定向管道的系统编码，避免中文进度在检查开始前崩溃。
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, 'reconfigure'):
            stream.reconfigure(encoding='utf-8')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suite', choices=['all','rust','quality','contracts','governance'], default='all')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--base', default='origin/merge/ai-ofice-react-overagent')
    parser.add_argument('--head', default='HEAD')
    parser.add_argument('--source', choices=['worktree', 'head'], default='worktree')
    parser.add_argument('--stage', choices=['proposal', 'implementation', 'delivery'], default='implementation')
    parser.add_argument('--timeout', type=int, default=1800)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error('timeout必须大于零')
    output = args.output.resolve()
    if output.is_relative_to(ROOT):
        parser.error('证据目录必须在仓库外，避免报告改变验证对象')
    output.mkdir(parents=True, exist_ok=False)
    before = metadata(ROOT)
    report = {'schema':'openpaw/verification/v1', 'started_at':dt.datetime.now(dt.timezone.utc).isoformat(),
              'metadata':before,'suite':args.suite,'base':args.base,'head':args.head,'results':[], 'complete':False}
    try:
        report.update(source_context(ROOT, args.source, args.head))
        report['stage'] = args.stage
        registry = json.loads((ROOT/'governance/commands.json').read_text(encoding='utf-8'))['commands']
        validate_registry(registry)
        selected = [c for c in registry if args.suite in ['all', c['suite']]]
        if not selected:
            raise ValueError('套件没有注册命令')
        current = 'windows' if os.name == 'nt' else 'linux' if sys.platform.startswith('linux') else 'unsupported'
        for command in selected:
            if current not in command['platforms']:
                raise ValueError('本机不在验证平台中：' + current)
            print('执行 ' + command['id'], flush=True)
            result = run_one(command, ROOT, output, args.timeout,
                             {'{python}':sys.executable, '{base}':args.base, '{head}':report['requested_head']},
                             args.source, args.stage)
            report['results'].append(result)
            (output/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        after = metadata(ROOT)
        report['source_changed_during_run'] = before['worktree_sha256'] != after['worktree_sha256']
        report['complete'] = len(report['results']) == len(selected)
        report['passed'] = report['complete'] and not report['source_changed_during_run'] and all(r['exit_code']==0 for r in report['results'])
        return 0 if report['passed'] else 1
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        report.update(passed=False, error=str(error))
        print('验证失败：' + str(error), file=sys.stderr)
        return 1
    finally:
        report['finished_at'] = dt.datetime.now(dt.timezone.utc).isoformat()
        (output/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')


if __name__ == '__main__':
    raise SystemExit(main())
