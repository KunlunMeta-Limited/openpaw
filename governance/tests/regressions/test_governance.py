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
        ('变更编号','CHG-2026-0001'),('项目CHG格式',check.FORMAT),('提案版本','fixture-r1'),('关联修订','无'),('历史元数据维护','无'),('历史说明维护','无'),('方案审核状态','pending'),('交付验收状态','pending'),('方案批准版本','待审核'),('方案审核人','待审核'),('方案审核时间','待审核'),('方案审核证据','待审核'),('方案批准对象','待登记'),('变更标题','治理回归测试'),('当前状态',status),
        ('变更类型','新增（additive）'),('风险等级',risk),('负责人','测试开发者'),('必须审核','非作者审核人'),
        ('设计基线','DES-OP-001'),('目标分支','merge/ai-ofice-react-overagent'),
        ('批准人','待审批'),('批准时间','待审批'),('实施授权来源','测试夹具，不代表实际批准')]),
        sections[1], '最小真实Git与格式校验夹具。\n', sections[2],table(['仓库编号','PR 或说明'],[('openpaw','测试提案')]),
        sections[3],table(['仓库','操作','文件','模块或符号','文件负责人','为什么必须修改','对其他部分的影响','必要性','具体修改方向与复用方式','完成标准与证据'],
                         [('openpaw',action,path,'模块','开发者','验证边界','仅测试夹具',required,'复用原测试入口实现范围检查','真实Git范围与状态结果一致') for path,action,required in files]),
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

    def pr_check(self, base, head, stage="proposal"):
        return check.pr_check(self.root, base, head, stage)

    def approve(self, path=None):
        path = path or self.path
        body=(self.root/path).read_text(encoding='utf-8')
        blob=subprocess.check_output(['git','hash-object','-w','--stdin'],cwd=self.root,input=body.encode()).decode().strip()
        body=body.replace('评审中（review）','实施中（in-progress）')
        for key,value in [('方案审核状态','approved'),('方案批准版本','fixture-r1'),('方案审核人','夹具用户'),
                          ('方案审核时间','2026-10-21'),('方案审核证据','夹具明确批准，不认证真实身份'),('方案批准对象',blob)]:
            old=check.approval_fields(check.tables(body))[key]
            body=body.replace('| '+key+' | '+old+' |','| '+key+' | '+value+' |')
        self.write(path,body)

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
        self.assertEqual(self.pr_check(self.base,None)['files'],2)
        self.commit()
        head = check.commit(self.root,'HEAD')
        self.write('rogue.txt','outside')
        self.assertEqual(self.pr_check(self.base,head)['files'],2)
        with self.assertRaisesRegex(check.Failure,'越界'):
            self.pr_check(self.base,None)

    def test_missing_chg_and_multiple_chgs_fail(self):
        (self.root/self.path).unlink()
        with self.assertRaises(check.Failure): self.validate()
        self.write(self.path,chg())
        self.write('docs/changes/CHG-2026-0002.md',chg().replace('CHG-2026-0001','CHG-2026-0002'))
        with self.assertRaisesRegex(check.Failure,'每PR'):
            self.pr_check(self.base,None)

    def test_deleted_historical_chg_cannot_hide_behind_new_chg(self):
        self.commit()
        base=check.commit(self.root,'HEAD')
        (self.root/self.path).unlink()
        self.write('docs/changes/CHG-2026-0002.md',chg().replace('CHG-2026-0001','CHG-2026-0002'))
        with self.assertRaisesRegex(check.Failure,'每PR'):
            self.pr_check(base,None)

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
        for path in ['/etc/passwd','../escape','./x','x/../y','x\\y','C:/Windows','C:relative','//server/share','bad\0path']:
            with self.assertRaises(check.Failure): check.relative(path)
        if sys.platform != 'win32':
            (self.root/'escape').symlink_to(Path(self.temp.name).parent)
            with self.assertRaises(check.Failure): check.Reader(self.root).read('escape/anything')

    def test_action_mandatory_and_conditional_files(self):
        body=chg(files=[(self.path,'新增','必改'),('中文 空格.txt','新增','必改'),('optional.txt','新增','条件')])
        self.write(self.path,body)
        self.assertEqual(self.pr_check(self.base,None)['unused_conditional'],['optional.txt'])
        self.write(self.path,body.replace('optional.txt | 模块 | 开发者 | 验证边界 | 仅测试夹具 | 条件','optional.txt | 模块 | 开发者 | 验证边界 | 仅测试夹具 | 必改'))
        with self.assertRaisesRegex(check.Failure,'必改缺失'): self.pr_check(self.base,None)
        self.write(self.path,chg().replace('| 新增 | 中文 空格.txt','| 修改 | 中文 空格.txt'))
        with self.assertRaisesRegex(check.Failure,'文件操作'): self.pr_check(self.base,None)

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
        with self.assertRaisesRegex(check.Failure,'base保护'): self.pr_check(self.base,None)

    def test_approval_is_separate_from_pr_and_binds_exact_plan(self):
        self.validate()  # proposal无批准可供审核
        with self.assertRaisesRegex(check.Failure,'批准|审批'):
            self.pr_check(self.base,None,'implementation')
        self.approve()
        self.pr_check(self.base,None,'implementation')  # 无PR URL也能正确批准方案
        original=(self.root/self.path).read_text(encoding='utf-8')
        self.write(self.path,original.replace('fixture-r1','fixture-r2',1))
        with self.assertRaisesRegex(check.Failure,'版本'):self.pr_check(self.base,None,'implementation')
        self.write(self.path,original.replace('复用原测试入口实现范围检查','新增未经批准的实现方法'))
        with self.assertRaisesRegex(check.Failure,'实质变化'):self.pr_check(self.base,None,'implementation')
        self.write(self.path,original+'\n补登技术事实，不改方案。\n')
        self.pr_check(self.base,None,'implementation')
        # 排版与换行不等于实质方案变化，方法文本变化仍由上面的反例拒绝。
        self.write(self.path,original.replace('| --- | --- |','| ------ | ------- |').replace('\n','\r\n'))
        self.pr_check(self.base,None,'implementation')
        self.write(self.path,original.replace('实施中（in-progress）','不存在（fake）'))
        with self.assertRaises(check.Failure):self.validate()

    def test_new_format_needs_actual_method_and_acceptance(self):
        for text in ['复用原测试入口实现范围检查','真实Git范围与状态结果一致']:
            self.write(self.path,chg().replace(text,''))
            with self.assertRaisesRegex(check.Failure,'说明'):self.validate()
        self.write(self.path,chg().replace(check.FORMAT,'unknown-format'))
        with self.assertRaisesRegex(check.Failure,'格式'):self.validate()

    def test_delivery_requires_acceptance_and_unchanged_code(self):
        self.approve()
        with self.assertRaisesRegex(check.Failure,'交付'):self.pr_check(self.base,None,'delivery')
        self.commit()
        accepted=check.commit(self.root,'HEAD')
        body=(self.root/self.path).read_text(encoding='utf-8')
        facts=[('交付验收人','夹具审核人'),('交付验收时间','2026-10-21'),
               ('交付验收证据','夹具明确验收'),('交付验收提交',accepted),('验证证据','夹具报告'),
               ('Linux验证结果','passed'),('Windows验证结果','passed'),('Linux验证提交',accepted),
               ('Windows验证提交',accepted),('Linux验证证据','夹具Linux报告'),('Windows验证证据','夹具Windows报告')]
        body=body.replace('| 交付验收状态 | pending |','| 交付验收状态 | approved |')
        body=body.replace('| 当前状态 |', ''.join('| '+k+' | '+v+' |\n' for k,v in facts)+'| 当前状态 |')
        self.write(self.path,body)
        self.pr_check(self.base,None,'delivery')
        self.write('中文 空格.txt','验收后的未审核代码')
        with self.assertRaisesRegex(check.Failure,'验收后'):self.pr_check(self.base,None,'delivery')
        closed=body.replace('实施中（in-progress）','已完成（closed）').replace('| 当前状态 |',
            '| 合入提交 | '+accepted+' |\n| 平台PR URL | https://github.com/example/fixture/pull/1 |\n| 当前状态 |')
        self.write(self.path,closed)
        # 未来其他变更不推翻已交付历史；原交付closed也不能冒充本轮活动修订。
        check.validate(check.Reader(self.root),'implementation')
        with self.assertRaisesRegex(check.Failure,'状态|批准'):self.pr_check(self.base,None,'implementation')

    def test_scoped_delivery_keeps_historical_facts_and_current_acceptance_strict(self):
        historical='docs/changes/CHG-2026-0002.md'
        self.write(historical,chg().replace('CHG-2026-0001','CHG-2026-0002'))
        self.approve(historical)
        # 只提交历史记录；当前提案和业务文件仍是本轮变更。
        self.command('add',historical)
        self.command('commit','-qm','historical approved without Windows acceptance')
        self.base=check.commit(self.root,'HEAD')
        historical_bytes=(self.root/historical).read_bytes()
        self.approve()
        self.commit()
        accepted=check.commit(self.root,'HEAD')
        body=(self.root/self.path).read_text(encoding='utf-8')
        facts=[('交付验收人','夹具审核人'),('交付验收时间','2026-11-04'),
               ('交付验收证据','本轮准确验收'),('交付验收提交',accepted),('验证证据','真实夹具报告'),
               ('Linux验证结果','passed'),('Windows验证结果','passed'),('Linux验证提交',accepted),
               ('Windows验证提交',accepted),('Linux验证证据','Linux报告'),('Windows验证证据','Windows报告')]
        body=body.replace('| 交付验收状态 | pending |','| 交付验收状态 | approved |')
        body=body.replace('| 当前状态 |',''.join('| '+k+' | '+v+' |\n' for k,v in facts)+'| 当前状态 |')
        self.write(self.path,body)
        def cli(*args):
            return subprocess.run([sys.executable,str(ROOT/'governance/check.py'),'--root',str(self.root),
                                   'validate',*args],capture_output=True,text=True)
        scoped=['--stage','delivery','--base',self.base,'--worktree']
        result=cli(*scoped)
        self.assertEqual(result.returncode,0,result.stderr)
        # 显式的全历史审计继续失败，不把旧平台缺项伪造为通过。
        self.assertNotEqual(cli('--stage','delivery').returncode,0)
        for broken in [body.replace('| Windows验证结果 | passed |','| Windows验证结果 | pending |'),
                       body.replace('| 交付验收状态 | approved |','| 交付验收状态 | pending |'),
                       body.replace('| Windows验证提交 | '+accepted+' |','| Windows验证提交 | '+self.base+' |'),
                       body.replace('| 交付验收提交 | '+accepted+' |','| 交付验收提交 | '+self.base+' |'),
                       body.replace('最小真实Git与格式校验夹具。','未获批准的方案变化。')]:
            self.write(self.path,broken)
            self.assertNotEqual(cli(*scoped).returncode,0)
        self.write(self.path,body)
        self.write('中文 空格.txt','验收后代码变化')
        self.assertNotEqual(cli(*scoped).returncode,0)
        self.write('中文 空格.txt','data')
        self.write(historical,(self.root/historical).read_text(encoding='utf-8')+'\n本轮未经范围登记修改历史\n')
        self.assertNotEqual(cli(*scoped).returncode,0)
        (self.root/historical).write_bytes(historical_bytes)
        self.commit()
        head=check.commit(self.root,'HEAD')
        result=cli('--stage','delivery','--base',self.base,'--head',head)
        self.assertEqual(result.returncode,0,result.stderr)
        for incomplete in [['--stage','delivery','--base',self.base],
                           ['--stage','delivery','--head',head],
                           ['--stage','implementation','--base',self.base,'--head',head]]:
            self.assertNotEqual(cli(*incomplete).returncode,0)
        self.assertEqual((self.root/historical).read_bytes(),historical_bytes)

    def test_shared_pr_requires_explicit_versions_and_independent_approval(self):
        other='docs/changes/CHG-2026-0004.md'
        self.write('other.txt','第二部分')
        secondary=chg(files=[(other,'新增','必改'),('other.txt','新增','必改')]).replace('CHG-2026-0001','CHG-2026-0004')
        self.write(other,secondary)
        with self.assertRaisesRegex(check.Failure,'每PR'):self.pr_check(self.base,None)
        body=chg().replace('| 关联修订 | 无 |','| 关联修订 | CHG-2026-0004@fixture-r1 |')
        self.write(self.path,body)
        self.approve()
        with self.assertRaisesRegex(check.Failure,'批准'):self.pr_check(self.base,None,'implementation')
        self.approve(other)
        self.assertEqual(self.pr_check(self.base,None,'implementation')['linked'],{'CHG-2026-0004':'fixture-r1'})
        self.write(other,(self.root/other).read_text(encoding='utf-8').replace('fixture-r1','fixture-r2',1))
        with self.assertRaisesRegex(check.Failure,'版本'):self.pr_check(self.base,None,'implementation')
        self.write(other,(self.root/other).read_text(encoding='utf-8').replace('fixture-r2','fixture-r1',1))
        self.commit()
        accepted=check.commit(self.root,'HEAD')
        facts=[('交付验收人','夹具审核人'),('交付验收时间','2026-11-04'),
               ('交付验收证据','双方独立验收'),('交付验收提交',accepted),('验证证据','夹具报告'),
               ('Linux验证结果','passed'),('Windows验证结果','passed'),('Linux验证提交',accepted),
               ('Windows验证提交',accepted),('Linux验证证据','Linux报告'),('Windows验证证据','Windows报告')]
        def delivery_body(path):
            value=(self.root/path).read_text(encoding='utf-8')
            value=value.replace('| 交付验收状态 | pending |','| 交付验收状态 | approved |')
            return value.replace('| 当前状态 |',''.join('| '+k+' | '+v+' |\n' for k,v in facts)+'| 当前状态 |')
        main,linked=delivery_body(self.path),delivery_body(other)
        self.write(self.path,main)
        # 先独立证明主CHG已通过，随后缺项必须来自关联修订。
        metadata,_=check.load_chg(check.Reader(self.root),self.path,check.profile_for(check.Reader(self.root)))
        check.check_stage(check.Reader(self.root),self.path,metadata,'delivery')
        def scoped():
            return subprocess.run([sys.executable,str(ROOT/'governance/check.py'),'--root',str(self.root),
                                   'validate','--stage','delivery','--base',self.base,'--worktree'],
                                  capture_output=True,text=True)
        result=scoped()
        self.assertNotEqual(result.returncode,0)
        self.assertIn('交付验收人',result.stderr)
        self.write(other,linked.replace('| Windows验证结果 | passed |','| Windows验证结果 | pending |'))
        result=scoped()
        self.assertNotEqual(result.returncode,0)
        self.assertIn('Windows',result.stderr)
        self.write(other,linked)
        result=scoped()
        self.assertEqual(result.returncode,0,result.stderr)

    def test_overlap_needs_both_responsibilities_and_stricter_protection(self):
        other='docs/changes/CHG-2026-0004.md'
        secondary=chg(files=[(other,'新增','必改'),('中文 空格.txt','新增','必改')]).replace('CHG-2026-0001','CHG-2026-0004')
        self.write(other,secondary)
        self.write(self.path,chg().replace('| 关联修订 | 无 |','| 关联修订 | CHG-2026-0004@fixture-r1 |'))
        with self.assertRaisesRegex(check.Failure,'重叠'):self.pr_check(self.base,None)
        for path,peer in [(self.path,'CHG-2026-0004'),(other,'CHG-2026-0001')]:
            body=(self.root/path).read_text(encoding='utf-8')
            body=body.replace('完成标准与证据 |','完成标准与证据 | 共同修改职责 |')
            body=body.replace('| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |',
                              '| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |')
            body=body.replace('真实Git范围与状态结果一致 |','真实Git范围与状态结果一致 | 与'+peer+'共同维护，双方分别审核职责 |')
            self.write(path,body)
        self.pr_check(self.base,None)

    def test_history_metadata_and_explanation_are_controlled(self):
        # 旧CHG范围必须保留，历史维护不把closed误当新的活动实施。
        self.commit()
        baseline=check.commit(self.root,'HEAD')
        historical=(self.root/self.path).read_text(encoding='utf-8')
        primary='docs/changes/CHG-2026-0004.md'
        self.write(primary,chg(files=[(primary,'新增','必改'),(self.path,'修改','必改')])
                   .replace('CHG-2026-0001','CHG-2026-0004').replace('CHG-2026-0004.md | 模块','CHG-2026-0001.md | 模块',1))
        # 用明确字符串构造，避免把旧CHG自己的登记路径误改。
        new=(self.root/primary).read_text(encoding='utf-8')
        rows=check.tables(new)['## 4. 允许修改文件']
        rows[0]['文件']=primary;rows[1]['文件']=self.path
        header=list(rows[0]);start=new.index('\n|',new.index('## 4.'))+1;end=new.index('\n\n',start)
        new=new[:start]+table(header,[[r[k] for k in header] for r in rows]).rstrip()+new[end:]
        new=new.replace('| 历史元数据维护 | 无 |','| 历史元数据维护 | '+self.path+' |')
        self.write(primary,new)
        self.write(self.path,historical+'\n补登真实状态记录。\n')
        self.pr_check(baseline,None)
        self.write(self.path,historical.replace('验证边界','扩充历史原因')+'\n补登真实状态记录。\n')
        with self.assertRaisesRegex(check.Failure,'历史维护'):self.pr_check(baseline,None)
        self.write(primary,new.replace('| 历史元数据维护 | '+self.path+' |','| 历史元数据维护 | 无 |')
                   .replace('| 历史说明维护 | 无 |','| 历史说明维护 | '+self.path+' |'))
        self.pr_check(baseline,None)
        self.write(self.path,historical.replace('中文 空格.txt | 模块','rogue.txt | 模块')+'\n补登记录。\n')
        with self.assertRaises(check.Failure):self.pr_check(baseline,None)

    def test_legacy_is_pinned_and_cannot_self_declare_exemption(self):
        body=chg().replace('| 项目CHG格式 | '+check.FORMAT+' |\n','')
        self.write(self.path,body)
        with self.assertRaisesRegex(check.Failure,'格式'):self.validate()
        self.commit()
        pinned=check.commit(self.root,'HEAD')
        profile=(self.root/check.PROFILE).read_text(encoding='utf-8')
        self.write(check.PROFILE,profile+'\n[[legacy_changes]]\npath="'+self.path+'"\nref="'+pinned+'"\n')
        self.validate()
        self.write(self.path,body.replace('CHG-2026-0001','CHG-2026-0002'))
        with self.assertRaisesRegex(check.Failure,'身份'):self.validate()

    def test_history_accepts_formatting_and_pr_metadata_but_protects_tests(self):
        before=chg()
        formatted=before.replace('| --- | --- |','| ------ | ------- |').replace('测试提案','已核实PR说明')
        check.historical_update(before,formatted,False)
        for bad in [formatted.replace('完成真实命令','降低验收要求'),
                    formatted.replace('最小真实Git与格式校验夹具','重写原原因'),
                    formatted.replace('| openpaw | 已核实PR说明','| another | 已核实PR说明')]:
            with self.assertRaises(check.Failure):check.historical_update(before,bad,False)

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
