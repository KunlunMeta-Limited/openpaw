"""验证执行器功能测试：运行真实子进程并检查失败记录。"""
from pathlib import Path
import os
import subprocess
import sys
import tempfile
import unittest
ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/'scripts'))
import verify

class RunnerTests(unittest.TestCase):
    def test_source_selection_uses_actual_dirty_and_untracked_content(self):
        with tempfile.TemporaryDirectory(prefix='openpaw-source-') as temp:
            root=Path(temp)
            def git(*args):
                return subprocess.check_output(['git',*args],cwd=root).decode().strip()
            git('init','-q')
            git('config','user.name','夹具')
            git('config','user.email','fixture@example.invalid')
            (root/'data.txt').write_text('已提交',encoding='utf-8')
            git('add','data.txt');git('commit','-qm','原版本')
            first=git('rev-parse','HEAD')
            self.assertEqual(verify.source_context(root,'head',first)['requested_head'],first)
            (root/'data.txt').write_text('当前未提交',encoding='utf-8')
            (root/'untracked.txt').write_text('未跟踪',encoding='utf-8')
            self.assertTrue(verify.source_context(root,'worktree',first)['dirty'])
            with self.assertRaisesRegex(ValueError,'干净'):verify.source_context(root,'head',first)
            output=root/'logs';output.mkdir()
            command={'id':'TEST-SOURCE','cwd':'.','argv':['{python}','-c',
                "from pathlib import Path; print(Path('data.txt').read_text(encoding='utf-8')+Path('untracked.txt').read_text(encoding='utf-8'))"]}
            result=verify.run_one(command,root,output,10,{'{python}':sys.executable},'worktree')
            self.assertEqual(result['exit_code'],0)
            self.assertIn('当前未提交未跟踪',(output/'TEST-SOURCE.stdout.log').read_text(encoding='utf-8'))
            before=verify.metadata(root)['worktree_sha256']
            (root/'untracked.txt').write_text('新增修改',encoding='utf-8')
            self.assertNotEqual(before,verify.metadata(root)['worktree_sha256'])
            git('add','data.txt','untracked.txt');git('commit','-qm','后续版本')
            with self.assertRaisesRegex(ValueError,'指定提交'):verify.source_context(root,'head',first)

    def test_registered_technical_commands_share_actual_source(self):
        import json
        commands=json.loads((ROOT/'scripts/verification/commands.json').read_text(encoding='utf-8'))['commands']
        verify.validate_registry(commands)
        identifiers={command['id'] for command in commands}
        self.assertTrue({'TEST-OFFICE-PRODUCT','TEST-COMMENTS','TEST-VERIFIER'} <= identifiers)
        self.assertFalse(any(identifier.startswith('TEST-GOV') for identifier in identifiers))
        for command in commands:
            actual=verify.command_argv(command,{'{python}':sys.executable,'{head}':'fixed-sha','{base}':'base'},'worktree')
            if command['id']=='TEST-COMMENTS':
                self.assertIn('--worktree',actual)
                self.assertNotIn('--head',actual)
                self.assertEqual(actual[actual.index('--base')+1],'base')
                with self.assertRaises(ValueError):verify.command_argv(command,{'{head}':'fixed-sha'},'head')
            clean=verify.command_argv(command,{'{python}':sys.executable,'{head}':'fixed-sha','{base}':'base'},'head')
            if '--head' in clean:self.assertEqual(clean[clean.index('--head')+1],'fixed-sha')

    def test_zero_or_filtered_subtests_cannot_pass(self):
        with tempfile.TemporaryDirectory(prefix='openpaw-empty-tests-') as temp:
            root=Path(temp)
            for kind,empty,valid in [('rust','test result: ok. 0 passed; 0 failed; 12 filtered out','test result: ok. 2 passed; 0 failed;'),
                                    ('node','# tests 0\n# pass 0','# tests 2\n# pass 2'),
                                    ('node','ℹ tests 0\nℹ pass 0','ℹ tests 2\nℹ pass 2'),
                                    ('python','Ran 0 tests in 0.0s','Ran 2 tests in 0.1s')]:
                for text,expected in [(empty,1),(valid,0)]:
                    command={'id':'TEST-COUNT','argv':[sys.executable,'-c',f'print({text!r})'],'cwd':'.','require_test_output':kind}
                    self.assertEqual(verify.run_one(command,root,root,10,{})['exit_code'],expected)

    def test_registry_cannot_overwrite_or_escape_evidence(self):
        command={'id':'TEST-ONE','argv':['python','-V'],'suite':'runner','platforms':['linux','windows']}
        verify.validate_registry([command])
        for commands in [[command,command],[dict(command,id='../escape')],[dict(command,argv='echo pass')],[]]:
            with self.assertRaises(ValueError):verify.validate_registry(commands)

    def test_real_exit_missing_command_timeout_and_dependency_guard(self):
        with tempfile.TemporaryDirectory(prefix='openpaw-runner-') as temp:
            root=Path(temp)
            for i,(argv,expected) in enumerate([([sys.executable,'-c','raise SystemExit(7)'],7),
                    (['openpaw-command-does-not-exist'],127),([sys.executable,'-c','import time; time.sleep(5)'],124)]):
                result=verify.execute(argv,root,root/f'test-{i}',0.2)
                self.assertEqual(result['exit_code'],expected)
            result=verify.run_one({'id':'TREE','argv':[sys.executable,'-c',"print('axum v1')"], 'cwd':'.','reject_stdout_prefix':'axum v'},root,root,10,{})
            self.assertEqual(result['exit_code'],1)
            with self.assertRaises(ValueError): verify.run_one({'id':'X','argv':[], 'cwd':'..'},root,root,10,{})

    def test_redirected_legacy_encoding_and_child_chinese_output(self):
        # 在Linux也重现Windows的CP1252管道；验证入口和真实Python子命令两端。
        with tempfile.TemporaryDirectory(prefix='openpaw-encoding-') as temp:
            output=Path(temp)/'existing'
            output.mkdir()
            env=dict(os.environ,PYTHONIOENCODING='cp1252',PYTHONUTF8='0')
            result=subprocess.run([sys.executable,str(ROOT/'scripts/verify.py'),
                '--output',str(output),'--timeout','0'],capture_output=True,env=env)
            self.assertNotEqual(result.returncode,0)
            self.assertIn('timeout必须大于零',result.stderr.decode('utf-8'))
            result=verify.execute([sys.executable,'-c',"print('中文验证')"],Path(temp),Path(temp)/'child',10)
            self.assertEqual(result['exit_code'],0)
            self.assertEqual((Path(temp)/'child.stdout.log').read_bytes(),'中文验证\n'.encode('utf-8') if os.name != 'nt' else '中文验证\r\n'.encode('utf-8'))

    def test_no_overwrite_existing_report(self):
        with tempfile.TemporaryDirectory(prefix='openpaw-report-') as temp:
            output=Path(temp)/'already'
            output.mkdir()
            result=subprocess.run([sys.executable,str(ROOT/'scripts/verify.py'),'--output',str(output)],capture_output=True)
            self.assertNotEqual(result.returncode,0)
            self.assertFalse((output/'report.json').exists())
