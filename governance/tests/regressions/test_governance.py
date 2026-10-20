"""项目治理功能与回归：使用临时Git仓和真实CHG校验，不读写Office。"""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT/'governance'))
import check
import verify


def table(headers, rows):
    return '\n'.join(['| '+' | '.join(headers)+' |', '| '+' | '.join(['---']*len(headers))+' |',
                      *['| '+' | '.join(row)+' |' for row in rows]])+'\n'


def chg(files=None, status='评审中（review）', risk='严重（critical）', protected=None):
    files = files or [('docs/changes/CHG-2026-0001.md','新增','必改'),('中文 空格.txt','新增','必改')]
    sections = check.upstream().CHG_SECTIONS
    content = ['# 工程变更记录\n',sections[0],table(['审批项目','内容'],[
        ('变更编号','CHG-2026-0001'),('变更标题','治理回归测试'),('当前状态',status),
        ('变更类型','新增（additive）'),('风险等级',risk),('负责人','测试开发者'),('必须审核','非作者审核人'),
        ('设计基线','DES-OP-001'),('目标分支','merge/ai-ofice-react-overagent'),
        ('批准人','待审批'),('批准时间','待审批'),('实施授权来源','测试夹具，不代表实际批准')]),
        sections[1], '最小真实Git与格式校验夹具。\n', sections[2],table(['仓库编号','PR 或说明'],[('openpaw','测试提案')]),
        sections[3],table(['仓库','操作','文件','模块或符号','文件负责人','为什么必须修改','对其他部分的影响','必要性'],
                         [('openpaw',action,path,'模块','开发者','验证边界','仅测试夹具',required) for path,action,required in files]),
        sections[4], table(['仓库','禁止路径','为什么禁止'],[('openpaw','never/**','不在变更范围')]),
        sections[5], '无运行API变化。\n',sections[6], '无业务字段变化。\n',sections[7],
        table(['仓库','文件或路径','等级','负责人','为什么需要保护'],
              [('openpaw',p,level,'审核人','防止弱化保护') for p,level in (protected or [])]),
        sections[8], table(['文档','章节与处理理由'],[(check.REQ,'说明治理需求')]),
        sections[9], table(['测试编号','执行命令','场景和通过条件'],[('TEST-GOV','{python} -c pass','完成真实命令')]),
        sections[10], table(['需求编号','设计章节','接口编号','代码文件或符号','测试编号','验收证据'],
                           [('REQ-OP-001','DES-OP-001','无','中文 空格.txt','TEST-GOV','临时仓库测试')]),
        '兼容：只读校验；回滚撤销本变更，不删除用户数据。\n', sections[11], '人工审核待执行，未声称批准。\n']
    return '\n\n'.join(content)


class GovernanceTests(unittest.TestCase):
    def test_api_snapshot_survives_windows_style_checkout(self):
        # 使用真正的Git检出而非比较配置字符串；CRLF不能制造接口漂移。
        with tempfile.TemporaryDirectory(prefix='openpaw-snapshot-eol-') as temp:
            root=Path(temp)
            def git(*args):
                return subprocess.run(['git','-C',str(root),*args],check=True,capture_output=True)
            git('init','-q')
            original=(ROOT/'openpaw-core.api.json').read_bytes()
            (root/'openpaw-core.api.json').write_bytes(original)
            (root/'.gitattributes').write_bytes((ROOT/'.gitattributes').read_bytes())
            git('add','.gitattributes','openpaw-core.api.json')
            output=root/'checkout'
            output.mkdir()
            git('-c','core.autocrlf=true','checkout-index','--all','--prefix='+output.as_posix()+'/')
            self.assertEqual((output/'openpaw-core.api.json').read_bytes(),original)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='openpaw-governance-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.command('init','-q')
        self.command('config','user.email','fixture@example.invalid')
        self.command('config','user.name','测试夹具')
        self.write(check.PROFILE, '''schema="ai-governance/project/v1"
integration_branch="merge/ai-ofice-react-overagent"
platforms=["linux","windows"]
baseline=["docs/requirements/产品需求说明.md","docs/design/软件设计说明.md"]
[[repositories]]
id="openpaw"
root="."
[[protected_paths]]
repository="openpaw"
pattern="protected.txt"
level="L4"
owner="审核人"
reason="需要保护"
''')
        for path in [check.REQ,check.DES,'docs/engine-host/audit.md','governance/framework.lock.toml','governance/vendor/framework_governance.py']:
            self.write(path,(ROOT/path).read_text(encoding='utf-8'))
        self.write('governance/identifiers.json', json.dumps({'ids':['REQ-OP-001','DES-OP-001']}))
        self.write('governance/commands.json', json.dumps({'commands':[{'id':'TEST-GOV','argv':['{python}','-c','pass'],'cwd':'.','platforms':['linux','windows']}]}))
        self.write('unchanged.txt','base')
        self.commit()
        self.base = check.commit(self.root,'HEAD')
        self.path = 'docs/changes/CHG-2026-0001.md'
        self.write('中文 空格.txt','data')
        self.write(self.path,chg())

    def command(self,*args):
        return subprocess.check_output(['git',*args],cwd=self.root,stderr=subprocess.PIPE)

    def write(self,path,body):
        target = self.root/path
        target.parent.mkdir(parents=True,exist_ok=True)
        target.write_text(body,encoding='utf-8',newline='\n')

    def commit(self):
        self.command('add','--all')
        self.command('commit','-qm','fixture')

    def validate(self):
        return check.validate(check.Reader(self.root))

    def test_normal_chg_after_phase_and_historical_tx(self):
        profile=(self.root/check.PROFILE).read_text(encoding='utf-8')+'\ninitial_tx_phase=false\n'
        # TOML根字段必须放在表之前，而不是误放到最后一个保护规则里。
        profile=profile.replace('\ninitial_tx_phase=false\n','').replace('schema=', 'initial_tx_phase=false\nnormal_target_branch="main"\nschema=',1)
        self.write(check.PROFILE,profile)
        self.write(self.path,chg().replace('merge/ai-ofice-react-overagent','main'))
        self.validate()
        self.write(self.path,chg().replace('| 变更标题 |','| 阶段 | T0 |\n| 变更标题 |'))
        self.validate()

    def test_valid_worktree_committed_head_and_dirty_isolation(self):
        self.assertEqual(check.pr_check(self.root,self.base,None)['files'],2)
        self.commit()
        head = check.commit(self.root,'HEAD')
        self.write('rogue.txt','outside')
        self.assertEqual(check.pr_check(self.root,self.base,head)['files'],2)
        with self.assertRaisesRegex(check.Failure,'越界'):
            check.pr_check(self.root,self.base,None)

    def test_missing_chg_and_multiple_chgs_fail(self):
        (self.root/self.path).unlink()
        with self.assertRaises(check.Failure): self.validate()
        self.write(self.path,chg())
        self.write('docs/changes/CHG-2026-0002.md',chg().replace('CHG-2026-0001','CHG-2026-0002'))
        with self.assertRaisesRegex(check.Failure,'每PR'):
            check.pr_check(self.root,self.base,None)

    def test_deleted_historical_chg_cannot_hide_behind_new_chg(self):
        self.commit()
        base=check.commit(self.root,'HEAD')
        (self.root/self.path).unlink()
        self.write('docs/changes/CHG-2026-0002.md',chg().replace('CHG-2026-0001','CHG-2026-0002'))
        with self.assertRaisesRegex(check.Failure,'每PR'):
            check.pr_check(self.root,base,None)

    def test_duplicate_fields_wrong_columns_duplicate_heading_fenced_text(self):
        body=chg()
        variants=[body.replace('| 变更编号 | CHG-2026-0001 |','| 变更编号 | CHG-2026-0001 |\n| 变更编号 | CHG-2026-0001 |'),
                  body.replace('| openpaw | 新增 |','| extra | openpaw | 新增 |',1), body+'\n## 1. 变更审批\n']
        for bad in variants:
            self.write(self.path,bad)
            with self.assertRaises(check.Failure): self.validate()
        self.write(self.path,body+'\n~~~text\n## 1. 变更审批\n| not a table\n~~~\n')
        self.validate()
        self.assertEqual(check.cells(r'| 中文 | a\|b |'),['中文','a|b'])

    def test_invalid_paths_and_symlink_escape(self):
        for path in ['/etc/passwd','../escape','./x','x/../y','x\\y']:
            with self.assertRaises(check.Failure): check.relative(path)
        if sys.platform != 'win32':
            (self.root/'escape').symlink_to(Path(self.temp.name).parent)
            with self.assertRaises(check.Failure): check.Reader(self.root).read('escape/anything')

    def test_action_mandatory_and_conditional_files(self):
        body=chg(files=[(self.path,'新增','必改'),('中文 空格.txt','新增','必改'),('optional.txt','新增','条件')])
        self.write(self.path,body)
        self.assertEqual(check.pr_check(self.root,self.base,None)['unused_conditional'],['optional.txt'])
        self.write(self.path,body.replace('optional.txt | 模块 | 开发者 | 验证边界 | 仅测试夹具 | 条件','optional.txt | 模块 | 开发者 | 验证边界 | 仅测试夹具 | 必改'))
        with self.assertRaisesRegex(check.Failure,'必改缺失'): check.pr_check(self.root,self.base,None)
        self.write(self.path,chg().replace('| 新增 | 中文 空格.txt','| 修改 | 中文 空格.txt'))
        with self.assertRaisesRegex(check.Failure,'文件操作'): check.pr_check(self.root,self.base,None)

    def test_rename_both_ends_nul_paths(self):
        raw = b'R100\0old name.txt\0'+'新 文件.txt'.encode()+b'\0M\0a.txt\0'
        self.assertEqual(check.parse_status(raw),{'old name.txt':'重命名源','新 文件.txt':'重命名目标','a.txt':'修改'})
        self.assertEqual(check.parse_status(b'C100\0old\0new\0'),{'new':'新增'})

    def test_risk_downgrade_and_profile_downgrade(self):
        self.write('protected.txt','protected')
        files=[(self.path,'新增','必改'),('中文 空格.txt','新增','必改'),('protected.txt','新增','必改')]
        self.write(self.path,chg(files=files,risk='低（low）',protected=[('protected.txt','L4')]))
        with self.assertRaisesRegex(check.Failure,'风险'): self.validate()
        self.write(self.path,chg(files=files,protected=[('protected.txt','L1')]))
        with self.assertRaisesRegex(check.Failure,'保护等级'): self.validate()
        profile=(self.root/check.PROFILE).read_text(encoding='utf-8').replace('level="L4"','level="L1"')
        self.write(check.PROFILE,profile)
        files.append((check.PROFILE,'修改','必改'))
        self.write(self.path,chg(files=files,protected=[('protected.txt','L1')]))
        with self.assertRaisesRegex(check.Failure,'base保护'): check.pr_check(self.root,self.base,None)

    def test_approval_cannot_be_empty_or_missing_platform_reference(self):
        self.write(self.path,chg(status='已批准（approved）'))
        with self.assertRaises(check.Failure): self.validate()
        self.write(self.path,chg(status='已批准（approved）').replace('待审批','测试人').replace('| 目标分支 |','| PR审核证据 | 自填姓名不是平台证据 |\n| 目标分支 |'))
        with self.assertRaisesRegex(check.Failure,'真实平台'): self.validate()
        self.write(self.path,chg().replace('评审中（review）','不存在（fake）'))
        with self.assertRaises(check.Failure): self.validate()

    def test_no_fake_registered_command_trace_or_identifier(self):
        for before,after in [('TEST-GOV | {python} -c pass','TEST-GOV | echo pass'),('DES-OP-001 | 无 |','DES-OP-999 | 无 |'),('中文 空格.txt | TEST-GOV','missing.txt | TEST-GOV')]:
            self.write(self.path,chg().replace(before,after))
            with self.assertRaises(check.Failure): self.validate()
        self.write(self.path,chg())
        self.write('governance/identifiers.json',json.dumps({'ids':['REQ-OP-999']}))
        with self.assertRaisesRegex(check.Failure,'权威来源'): self.validate()

    def test_crlf_chg_is_not_double_converted_in_temporary_copy(self):
        path=self.root/self.path
        path.write_bytes(chg().encode('utf-8').replace(b'\n',b'\r\n'))
        self.validate()

    def test_framework_raw_newlines_are_part_of_hash(self):
        path=self.root/'governance/vendor/framework_governance.py'
        path.write_bytes(path.read_bytes().replace(b'\n',b'\r\n'))
        with self.assertRaisesRegex(check.Failure,'哈希'):self.validate()

    def test_framework_hash_and_generated_index_trace_drift(self):
        valid = self.validate()
        generated=check.index_text(valid)
        path=self.root/check.INDEX
        check.write_generated(path,generated,False)
        check.write_generated(path,generated,True)
        path.write_text('fake',encoding='utf-8')
        with self.assertRaises(check.Failure): check.write_generated(path,generated,True)
        trace=check.trace_text(check.Reader(self.root),valid)
        self.assertIn('REQ-OP-020',trace)
        self.write('governance/vendor/framework_governance.py','tampered')
        with self.assertRaisesRegex(check.Failure,'哈希'): self.validate()
