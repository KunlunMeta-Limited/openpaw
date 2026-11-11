'use strict';
// 新机器安装回归：仅使用全新临时目录，不读取开发机的模型或历史。
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const cp = require('node:child_process');
const setup = require('../office-setup.cjs');
const temporary = t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'openpaw setup 中文 '));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
};
const pathsFor = dir => setup.locations({ OPENPAW_SETUP_HOME: path.join(dir, '私有 data') });
const envText = () => setup.modelEnvironment('openai', 'deepseek-chat', 'https://api.deepseek.com/v1', 'test-only-$not-expanded');

test('fresh installation creates private configuration, 32 empty roots and no imported history', t => {
  const paths = pathsFor(temporary(t));
  assert.equal(setup.installState(paths), 'fresh');
  setup.createFresh(paths, envText(), false);
  assert.equal(setup.installState(paths), 'existing');
  assert.deepEqual(fs.readdirSync(path.join(paths.base, 'office-host-home')), []);
  const dirs = fs.readdirSync(path.join(paths.base, 'workspaces'));
  assert.equal(dirs.length, 32);
  for (const dir of dirs) assert.deepEqual(fs.readdirSync(path.join(paths.base, 'workspaces', dir)), []);
  const config = fs.readFileSync(paths.config, 'utf8');
  assert.match(config, /^max_active_runs = 24$/m, '全局容量应容纳多个办公室的完整协作组');
  assert.match(config, /auto_approve_tools = \[\]/);
  assert.match(config, /auto_approve = false/);
  assert.equal((config.match(/office-\d+ = /g) || []).length, 64);
  assert.equal((config.match(/"office-\d+"/g) || []).length, 63);
  assert.doesNotMatch(config, /echo|test-only|cheng|\/tmp\/openpaw-live/);
  const key = fs.readFileSync(paths.key, 'utf8').trim();
  assert.match(key, /^[a-f0-9]{64}$/);
  assert.ok(!envText().includes(key));
  if (process.platform !== 'win32') {
    for (const file of [paths.env, paths.key, paths.config]) assert.equal(fs.statSync(file).mode & 0o777, 0o600);
    assert.equal(fs.statSync(paths.base).mode & 0o777, 0o700);
  } else {
    // 原生Windows确认新目录已关闭继承，不把Linux chmod视为Windows证据。
    const acl = cp.execFileSync('icacls.exe', [paths.base], { encoding: 'utf8' });
    assert.ok(!acl.includes('(I)'));
  }
});

test('persistent write consent uses the original Host allowlist and continuous approval', t => {
  const paths = pathsFor(temporary(t));
  setup.createFresh(paths, envText(), true);
  const config = fs.readFileSync(paths.config, 'utf8');
  assert.match(config, /auto_approve_tools = \["write_file","edit_file","render_visual"\]/);
  assert.match(config, /auto_approve = true/);
  assert.doesNotMatch(config, /"bash"|generate_image/);
});

test('rerun preserves credentials, permissions, chat state and workspace contents byte-for-byte', t => {
  const paths = pathsFor(temporary(t));
  setup.createFresh(paths, envText(), false);
  const sample = path.join(paths.base, 'office-host-home', 'test-state');
  fs.writeFileSync(sample, 'isolated test data');
  const original = [paths.env, paths.config, paths.key, sample].map(file => fs.readFileSync(file));
  assert.equal(setup.installState(paths), 'existing');
  assert.throws(() => setup.createFresh(paths, 'replacement', true), /已有配置/);
  for (const [index, file] of [paths.env, paths.config, paths.key, sample].entries()) assert.deepEqual(fs.readFileSync(file), original[index]);
});

test('partial configuration and existing data cannot become a fresh installation', t => {
  const paths = pathsFor(temporary(t));
  fs.mkdirSync(paths.base, { recursive: true });
  fs.writeFileSync(paths.env, 'not a real key');
  assert.throws(() => setup.installState(paths), /不完整/);
  fs.unlinkSync(paths.env);
  fs.mkdirSync(path.join(paths.base, 'office-host-home'));
  fs.writeFileSync(path.join(paths.base, 'office-host-home', 'test-state'), 'do not overwrite');
  assert.throws(() => setup.createFresh(paths, envText(), false), /已有会话/);
  assert.equal(fs.existsSync(paths.config), false);
});

test('rejects invalid credential/URL input without echoing secret content', () => {
  for (const key of ['', "secret'bad", 'secret\nline']) {
    assert.throws(() => setup.modelEnvironment('openai', 'model', 'https://example.com/v1', key), error => !error.message.includes('secret'));
  }
  for (const url of ['ftp://example.com', 'https://user:pw@example.com', 'https://example.com?key=secret', 'https://example.com/chat/completions']) {
    assert.throws(() => setup.modelEnvironment('openai', 'model', url, 'test'));
  }
  assert.match(setup.modelEnvironment('anthropic', 'model', 'https://api.anthropic.com', 'test'), /ANTHROPIC_API_KEY='test'/);
  assert.match(envText(), /'test-only-\$not-expanded'/);
});

test('rejects path/port collisions and config paths inside the Git repository', t => {
  const paths = pathsFor(temporary(t));
  assert.throws(() => setup.installState({ ...paths, key: paths.env }), /三个不同文件/);
  assert.throws(() => setup.installState({ ...paths, officePort: paths.hostPort }), /端口必须不同/);
  assert.throws(() => setup.installState({ ...paths, env: path.join(paths.base, 'office-host-home', '.env') }), /必须与聊天数据/);
  assert.throws(() => setup.installState({ ...paths, base: path.parse(paths.base).root }), /独立数据目录/);
  assert.throws(() => setup.installState({ ...paths, base: path.resolve(__dirname, '..', '..', '.test-local') }), /仓库内/);
  assert.throws(() => setup.installState({ ...paths, base: path.join(paths.office, '.test-local') }), /Office仓库内/);
  for (const value of ['0', '65536', 'not-a-port']) assert.throws(() => setup.port(value));
});

test('does not follow dangling links when generating private configuration', t => {
  const dir = temporary(t);
  const paths = pathsFor(dir);
  fs.symlinkSync(path.join(dir, 'not-created'), paths.base, process.platform === 'win32' ? 'junction' : 'dir');
  assert.throws(() => setup.createFresh(paths, envText(), false), /软链接/);
  assert.equal(fs.existsSync(path.join(dir, 'not-created')), false);
});

test('fresh installations get different service keys', t => {
  const dir = temporary(t);
  const a = pathsFor(path.join(dir, 'a')), b = pathsFor(path.join(dir, 'b'));
  setup.createFresh(a, envText(), false);
  setup.createFresh(b, envText(), false);
  assert.notEqual(fs.readFileSync(a.key, 'utf8'), fs.readFileSync(b.key, 'utf8'));
});

test('actual platform wrapper exposes setup and rejects incomplete input without leaking secrets', { timeout: 45000 }, t => {
  const dir = temporary(t);
  const win = process.platform === 'win32';
  const wrapper = path.resolve(__dirname, '..', `openpaw-office.${win ? 'ps1' : 'sh'}`);
  const args = win ? ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', wrapper, 'setup'] : [wrapper, 'setup'];
  const result = cp.spawnSync(win ? 'powershell.exe' : 'bash', args, {
    input: 'openai\nmodel\nhttps://example.com/v1\nsetup-test-secret\nINVALID\n',
    // Windows的冷PowerShell启动与杀进程耗时不能使用Linux子进程的8秒预算。
    // 这不是交互延迟验收；仍要求实际进入权限校验，而不能靠超时算作拒绝。
    env: { ...process.env, OPENPAW_SETUP_HOME: dir }, encoding: 'utf8', timeout: win ? 30000 : 8000,
  });
  assert.ifError(result.error);
  assert.notEqual(result.status, 0);
  assert.ok(!`${result.stdout}${result.stderr}`.includes('setup-test-secret'));
  // 此测试在具备Rust/Git/Node的原生CI运行，实际验证到权限输入拒绝。
  assert.match(`${result.stdout}${result.stderr}`, /权限选项只能是/);
  assert.ok(!fs.existsSync(path.join(dir, '.env')));
});

test('actual configure-only setup and rerun preserve a newly generated installation', t => {
  const dir = temporary(t);
  const office = path.join(dir, 'Office 源码');
  fs.mkdirSync(office);
  fs.writeFileSync(path.join(office, 'package-lock.json'), '{}');
  const helper = path.resolve(__dirname, '..', 'office-setup.cjs');
  const env = { ...process.env, OPENPAW_SETUP_HOME: path.join(dir, '本机 data'), OPENPAW_OFFICE_SOURCE: office };
  const fresh = cp.spawnSync(process.execPath, [helper], { env, input: 'openai\n\n\nfixture-secret\n2\ny\n1\n', encoding: 'utf8', timeout: 8000 });
  assert.equal(fresh.status, 0, fresh.stderr);
  assert.ok(!fresh.stdout.includes('fixture-secret'));
  const paths = setup.locations(env);
  const before = [paths.env, paths.config, paths.key].map(file => fs.readFileSync(file));
  const again = cp.spawnSync(process.execPath, [helper], { env, input: '1\n', encoding: 'utf8', timeout: 8000 });
  assert.equal(again.status, 0, again.stderr);
  assert.match(again.stdout, /已有完整配置/);
  for (const [index, file] of [paths.env, paths.config, paths.key].entries()) assert.deepEqual(fs.readFileSync(file), before[index]);
});

test('missing prerequisite fails before creating private state', t => {
  const dir = temporary(t);
  const helper = path.resolve(__dirname, '..', 'office-setup.cjs');
  const result = cp.spawnSync(process.execPath, [helper], { env: { ...process.env, PATH: '', OPENPAW_SETUP_HOME: path.join(dir, 'data') }, encoding: 'utf8', timeout: 5000 });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /缺少Git/);
  assert.equal(fs.existsSync(path.join(dir, 'data')), false);
});

test('failed dependency installation reports failure and leaves fresh config retryable', t => {
  const dir = temporary(t);
  const office = path.join(dir, 'Office 源码');
  fs.mkdirSync(office);
  fs.writeFileSync(path.join(office, 'package-lock.json'), 'invalid lock file');
  const env = { ...process.env, OPENPAW_SETUP_HOME: path.join(dir, 'data'), OPENPAW_OFFICE_SOURCE: office };
  const result = cp.spawnSync(process.execPath, [path.resolve(__dirname, '..', 'office-setup.cjs')], {
    env, input: 'openai\n\n\nfixture-secret\n1\nn\n2\n', encoding: 'utf8', timeout: 8000,
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /命令执行失败/);
  assert.ok(!`${result.stdout}${result.stderr}`.includes('fixture-secret'));
  assert.equal(setup.installState(setup.locations(env)), 'existing');
});


test('interactive questions are explicitly opt-in, independent of write consent, and rerun cannot expand permission', t => {
  for (const writes of [false, true]) {
    for (const questions of [false, true]) {
      const paths = pathsFor(temporary(t));
      setup.createFresh(paths, envText(), writes, questions);
      const config = fs.readFileSync(paths.config, 'utf8');
      const tools = JSON.parse(config.match(/^tools = (.*)$/m)[1]);
      assert.equal(tools.includes('clarify'), questions);
      assert.match(config, writes ? /auto_approve = true/ : /auto_approve = false/);
      assert.throws(() => setup.createFresh(paths, envText(), !writes, !questions), /已有配置/);
      assert.equal(fs.readFileSync(paths.config, 'utf8'), config);
    }
  }
});


test('setup accepts current Rust floor and rejects older or unrecognized toolchains', () => {
  for (const version of ['rustc 1.89.0 (fixture)', 'rustc 1.99.1', 'rustc 2.0.0']) assert.equal(setup.rustVersionSupported(version), true, version);
  for (const version of ['rustc 1.88.9', 'rustc 1.80.0', 'rustc 0.99.0', 'unknown']) assert.equal(setup.rustVersionSupported(version), false, version);
});
