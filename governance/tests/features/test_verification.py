"""验证执行器功能测试：运行真实子进程并检查失败记录。"""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
ROOT=Path(__file__).resolve().parents[3]
sys.path.insert(0,str(ROOT/'governance'))
import verify
import check

class RunnerTests(unittest.TestCase):
    def test_registry_cannot_overwrite_or_escape_evidence(self):
        command={'id':'TEST-ONE','argv':['python','-V'],'suite':'governance','platforms':['linux','windows']}
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

    def test_no_overwrite_existing_report(self):
        with tempfile.TemporaryDirectory(prefix='openpaw-report-') as temp:
            output=Path(temp)/'already'
            output.mkdir()
            result=subprocess.run([sys.executable,str(ROOT/'governance/verify.py'),'--output',str(output)],capture_output=True)
            self.assertNotEqual(result.returncode,0)
            self.assertFalse((output/'report.json').exists())



class ProjectLifecycleTests(unittest.TestCase):
    def test_phase_exit_changes_target_branch_rule(self):
        # 项目配置自身受L4审核；这里验证正常CHG不会永久锁在集成分支。
        import tomllib
        profile=tomllib.loads((ROOT/'governance/project-profile.toml').read_text(encoding='utf-8'))
        self.assertTrue(profile['initial_tx_phase'])
        self.assertEqual(profile['normal_target_branch'],'main')
        self.assertEqual(profile['platforms'],['linux','windows'])
        for path in (ROOT/'.github/workflows').glob('*.yml'):
            self.assertNotIn('macos-latest',path.read_text(encoding='utf-8').lower())
