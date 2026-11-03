#!/usr/bin/env node
// 共用首次安装逻辑；只生成本机新配置，不读取或复制其他安装的数据。
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const cp = require('node:child_process');
const readline = require('node:readline');
const { Writable } = require('node:stream');

const root = path.resolve(__dirname, '..');
const windows = process.platform === 'win32';
const note = message => console.log(`openpaw-office: ${message}`);
const fail = message => { throw new Error(message); };
const inside = (parent, child) => {
  const relative = path.relative(parent, child);
  return !relative || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
};

function locations(env = process.env, home = os.homedir()) {
  const base = path.resolve(env.OPENPAW_SETUP_HOME || path.join(home, '.openpaw'));
  return {
    base,
    env: path.resolve(env.OPENPAW_ENV_FILE || path.join(base, '.env')),
    config: path.resolve(env.OPENPAW_HOST_CONFIG || path.join(base, 'engine-host-office.toml')),
    key: path.resolve(env.OPENPAW_OFFICE_KEY_FILE || path.join(base, '.office-service-key')),
    office: path.resolve(env.OPENPAW_OFFICE_SOURCE || path.join(root, '..', 'ai-office-react')),
    hostPort: port(env.OPENPAW_HOST_PORT || '32885'),
    officePort: port(env.OPENPAW_OFFICE_PORT || '8765'),
  };
}

function port(value) {
  if (!/^\d+$/.test(value) || Number(value) < 1024 || Number(value) > 65535) fail('端口须为1024—65535之间的整数');
  return Number(value);
}

function privateDirectory(directory) {
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  if (windows) {
    // 使用当前用户SID避免域用户名称、空格及本地化组名引起ACL错误。
    const who = cp.execFileSync('whoami.exe', ['/user', '/fo', 'csv', '/nh'], { encoding: 'utf8' });
    const sid = who.match(/S-1-[\d-]+/);
    if (!sid) fail('无法取得Windows用户SID；未写入密钥');
    cp.execFileSync('icacls.exe', [directory, '/inheritance:r', '/grant:r', `*${sid[0]}:(OI)(CI)F`, '*S-1-5-18:(OI)(CI)F'], { stdio: 'pipe' });
  } else {
    fs.chmodSync(directory, 0o700);
  }
}

function assertLocation(target) {
  // 配置不得落入仓库、现有软链接或其他安装的工作区，避免误写/意外提交。
  if (inside(root, target)) fail('本机私有配置/数据不能放在OpenPaw仓库内；请设置仓库外的OPENPAW_SETUP_HOME');
  let current = path.resolve(target);
  while (true) {
    let entry;
    try { entry = fs.lstatSync(current); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (entry?.isSymbolicLink()) fail('安装配置路径不可经过软链接；请使用真实目录');
    const parent = path.dirname(current);
    if (current === parent) break;
    current = parent;
  }
}

function preflight() {
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (!(major === 22 && minor >= 13 || major >= 24)) fail('需要Node.js 24 LTS（或22.13+的22.x），请从 https://nodejs.org 安装后重试');
  const check = (command, args, help) => {
    const result = cp.spawnSync(command, args, { encoding: 'utf8', shell: windows && command.endsWith('.cmd') });
    if (result.error || result.status !== 0) fail(help);
    return result.stdout;
  };
  check('git', ['--version'], '缺少Git：https://git-scm.com/downloads');
  const version = check('rustc', ['--version'], '缺少Rust：https://rustup.rs；Windows还需Visual Studio C++ Build Tools，Linux需C编译器');
  const match = version.match(/rustc (\d+)\.(\d+)/);
  if (!match || Number(match[1]) < 1 || Number(match[1]) === 1 && Number(match[2]) < 88) fail('需要Rust 1.88+；运行 rustup update stable');
  check('cargo', ['--version'], 'cargo不在PATH中；安装Rust后重新打开终端');
  check(windows ? 'npm.cmd' : 'npm', ['--version'], '缺少npm；请重新安装Node.js LTS');
  if (!windows) {
    check('cc', ['--version'], '缺少C编译器；Debian/Ubuntu安装 build-essential pkg-config libssl-dev，Fedora安装 gcc pkgconf-pkg-config openssl-devel');
    check('curl', ['--version'], '缺少curl；请通过系统软件包管理器安装curl');
  } else {
    const linker = cp.spawnSync('where.exe', ['link.exe'], { stdio: 'ignore' });
    const vswhere = path.join(process.env['ProgramFiles(x86)'] || '', 'Microsoft Visual Studio', 'Installer', 'vswhere.exe');
    const vs = fs.existsSync(vswhere) && cp.spawnSync(vswhere, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath'], { encoding: 'utf8' });
    if (linker.status !== 0 && !(vs && vs.status === 0 && vs.stdout.trim())) fail('请安装Visual Studio Build Tools的“使用C++的桌面开发”及Windows SDK，然后重开终端');
  }
  note(`依赖检查通过：Node ${process.versions.node}；${version.trim()}`);
}

function configuration(paths, allowWrites) {
  const q = value => JSON.stringify(value.replaceAll('\\', '/'));
  const all = Array.from({ length: 32 }, (_, index) => `office-${index + 1}`);
  const tools = ['read_file', 'glob', 'grep', 'todo_list', 'think', 'web_search', 'web_fetch', 'write_file', 'edit_file', 'render_visual'];
  return `# 由setup生成的本机配置；不提交到仓库。撤销预授权后重启生效。\n` +
    `home = ${q(path.join(paths.base, 'office-host-home'))}\n` +
    `max_active_runs = 6\nrun_timeout_secs = 0\nstream_idle_secs = 180\ntool_timeout_secs = 300\napproval_timeout_secs = 300\n` +
    `tools = ${JSON.stringify(tools)}\nauto_approve_tools = ${JSON.stringify(allowWrites ? ['write_file', 'edit_file', 'render_visual'] : [])}\n` +
    `\n[roots]\n${all.map(id => `${id} = ${q(path.join(paths.base, 'workspaces', id))}`).join('\n')}\n` +
    `\n[slots]\n${all.map(id => `${id} = ${q(id)}`).join('\n')}\n` +
    `\n[office]\nbase_url = "http://127.0.0.1:${paths.officePort}"\napi_key_env = "PAWOFFICE_API_KEY"\n` +
    `allowed_origins = ["http://127.0.0.1:${paths.officePort}"]\nactivity_window = true\ncreation_slots = ${JSON.stringify(all.slice(1))}\n` +
    `\n[agents]\nenabled = true\nmax_active_children = 5\nmax_delegations = 0\nmax_model_requests = 0\nmax_output_tokens = 0\nmax_response_tokens = 16384\n` +
    `\n[agents.continuous]\nenabled = true\nmax_open_items = 4\noutput_dir = "outputs"\nauto_approve = ${allowWrites}\nstage_pause_ms = 250\n`;
}

function modelEnvironment(provider, model, baseUrl, key) {
  for (const value of [model, baseUrl, key]) {
    if (!value.trim() || /['\r\n\0]/.test(value)) fail('模型字段不能为空或含单引号/换行/空字符；密钥内容已隐藏');
  }
  if (!['openai', 'anthropic'].includes(provider)) fail('仅支持openai兼容协议或anthropic');
  let url;
  try { url = new URL(baseUrl); } catch { fail('服务URL格式不正确'); }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash || /\/chat\/completions\/?$/.test(url.pathname)) fail('填写无凭据/查询参数的HTTP(S)基础URL，不含/chat/completions');
  // dotenvy单引号保持$等字符原样；不把此文件source到子进程环境。
  return `# 仅OpenPaw官方模型环境读取；勿提交或发送此文件。\nOPENPAW_PROVIDER=${provider}\nOPENPAW_MODEL='${model}'\nOPENPAW_BASE_URL='${baseUrl}'\n${provider === 'anthropic' ? 'ANTHROPIC_API_KEY' : 'OPENAI_API_KEY'}='${key}'\n`;
}

function installState(paths) {
  if (new Set([paths.env, paths.config, paths.key]).size !== 3) fail('模型、Host配置、Office密钥必须使用三个不同文件');
  if (paths.hostPort === paths.officePort) fail('Host与Office端口必须不同');
  if (paths.base === path.parse(paths.base).root || paths.base === os.homedir()) fail('安装根须为独立数据目录，不能使用磁盘根或整个用户目录');
  for (const file of [paths.env, paths.config, paths.key]) {
    if (['office-host-home', 'workspaces'].some(name => inside(path.join(paths.base, name), file))) fail('配置与密钥必须与聊天数据及工作文件分开');
  }
  for (const file of [paths.base, paths.env, paths.config, paths.key]) {
    assertLocation(file);
    if (inside(paths.office, file)) fail('本机私有配置/数据不能放在Office仓库内；请使用仓库外的OPENPAW_SETUP_HOME');
  }
  const present = [paths.env, paths.config, paths.key].map(file => fs.existsSync(file));
  if (present.every(Boolean)) {
    if (![paths.env, paths.config, paths.key].every(file => fs.statSync(file).isFile() && fs.statSync(file).size)) fail('已有配置为空或不是普通文件；不覆盖，请先检查');
    return 'existing';
  }
  if (present.some(Boolean)) fail('检测到不完整的已有配置；不会覆盖。请核对.env、Host配置与.office-service-key，或使用新的OPENPAW_SETUP_HOME');
  for (const directory of ['office-host-home', 'workspaces']) {
    const target = path.join(paths.base, directory);
    if (fs.existsSync(target) && fs.readdirSync(target).length) fail('目标目录已有会话或工作文件；空白安装不使用旧数据。请选新的OPENPAW_SETUP_HOME');
  }
  return 'fresh';
}

function createFresh(paths, envText, allowWrites) {
  if (installState(paths) !== 'fresh') fail('已有配置，不允许覆盖');
  const text = configuration(paths, allowWrites);
  const serviceKey = crypto.randomBytes(32).toString('hex');
  // 自定义配置文件仍须位于私有安装根中，避免给既有Desktop等目录改ACL。
  for (const file of [paths.env, paths.config, paths.key]) {
    if (!inside(paths.base, file)) fail('setup生成的配置必须位于OPENPAW_SETUP_HOME中；已有自定义配置仍可通过start读取');
  }
  for (const directory of new Set([paths.base, path.dirname(paths.env), path.dirname(paths.config), path.dirname(paths.key)])) privateDirectory(directory);
  for (const [file, bytes] of [[paths.env, envText], [paths.config, text], [paths.key, `${serviceKey}\n`]]) {
    fs.writeFileSync(file, bytes, { flag: 'wx', mode: 0o600 });
  }
  // Windows子目录继承私有根ACL，避免为32个空根启动重复ACL进程。
  fs.mkdirSync(path.join(paths.base, 'office-host-home'), { recursive: true, mode: 0o700 });
  fs.mkdirSync(path.join(paths.base, 'workspaces'), { recursive: true, mode: 0o700 });
  for (let index = 1; index <= 32; index++) fs.mkdirSync(path.join(paths.base, 'workspaces', `office-${index}`), { recursive: true, mode: 0o700 });
}

function prompts() {
  let hidden = false;
  const output = new Writable({ write(chunk, encoding, callback) { if (!hidden) process.stdout.write(chunk, encoding); callback(); } });
  const rl = readline.createInterface({ input: process.stdin, output, terminal: Boolean(process.stdin.isTTY) });
  const lines = rl[Symbol.asyncIterator]();
  return {
    async ask(label, fallback, secret = false) {
      process.stdout.write(`${label}${fallback ? ` [${fallback}]` : ''}: `);
      hidden = secret;
      let next;
      try { next = await lines.next(); } finally { hidden = false; }
      if (secret) process.stdout.write('\n');
      if (next.done) fail('输入已结束，安装未完成；请在终端重新运行setup');
      return next.value.trim() || fallback || '';
    },
    close() { rl.close(); },
  };
}

function run(command, args, options = {}) {
  const result = cp.spawnSync(command, args, { stdio: 'inherit', ...options });
  if (result.error || result.status !== 0) fail('命令执行失败，配置和已有数据已保留；请查看上方错误并修复后重新运行setup');
}

async function main() {
  preflight();
  const paths = locations();
  const state = installState(paths);
  const prompt = prompts();
  try {
    note(`配置位置：${paths.base}；Office源码：${paths.office}`);
    if (state === 'fresh') {
      note('全新安装：不复制聊天、文章、密钥或备份。DeepSeek请选择openai兼容协议。');
      const provider = (await prompt.ask('协议 openai / anthropic', 'openai')).toLowerCase();
      const model = await prompt.ask('模型名称', provider === 'anthropic' ? 'claude-sonnet-4-5' : 'deepseek-chat');
      const url = await prompt.ask('服务基础URL', provider === 'anthropic' ? 'https://api.anthropic.com' : 'https://api.deepseek.com/v1');
      const key = await prompt.ask('模型API密钥（隐藏输入）', undefined, true);
      const envText = modelEnvironment(provider, model, url, key);
      note('权限选择：1 每次写文件/修改/绘图都审批；2 持久允许这些工具。只作用于配置中的工作目录，逐次保留审计。不开放Shell执行。');
      const permission = await prompt.ask('选择1或2', '1');
      if (!['1', '2'].includes(permission)) fail('权限选项只能是1或2；未写入配置');
      createFresh(paths, envText, permission === '2');
      note(`已生成全新配置、独立Office密钥和32个空工作目录；工作文件在 ${path.join(paths.base, 'workspaces')}。`);
    } else {
      note('已有完整配置：全部保留，不覆盖模型、权限、工作区或聊天数据。');
    }
    if (!fs.existsSync(path.join(paths.office, 'package-lock.json'))) {
      if (fs.existsSync(paths.office)) fail('Office目录存在但缺少package-lock.json；请核对路径，不自动覆盖');
      const manifest = JSON.parse(fs.readFileSync(path.join(root, 'docs/engine-host/contracts/sources.json'), 'utf8')).office;
      if (await prompt.ask(`Office源码缺失。克隆已适配版本 ${manifest.commit}？y/n`, 'y') !== 'y') fail('需要Office源码；请设置OPENPAW_OFFICE_SOURCE后重新运行setup');
      run('git', ['clone', manifest.repository, paths.office]);
      run('git', ['-C', paths.office, 'checkout', '--detach', manifest.commit]);
    }
    note('普通聊天模型并不提供图片生成；render_visual可绘制SVG图表。独立生图服务可稍后按文档配置。');
    const action = await prompt.ask('下一步：1 仅生成配置；2 安装依赖并编译；3 安装、编译并启动', '3');
    if (!['1', '2', '3'].includes(action)) fail('下一步只能选择1、2或3');
    if (action !== '1') {
      run(windows ? 'npm.cmd' : 'npm', ['ci', '--no-audit', '--no-fund'], { cwd: paths.office, shell: windows });
      const wrapper = windows ? 'powershell.exe' : 'bash';
      const prefix = windows ? ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(root, 'scripts/openpaw-office.ps1')] : [path.join(root, 'scripts/openpaw-office.sh')];
      run(wrapper, [...prefix, 'build']);
      if (action === '3') run(wrapper, [...prefix, 'start']);
    }
    note(`setup完成。模型配置：${paths.env}；权限/工作区：${paths.config}；服务密钥：${paths.key}（不要提交）。`);
    note(`页面：http://127.0.0.1:${paths.officePort}/office/；模型密钥有效性需实际聊天验证，setup不发送付费模型请求。`);
    note('若设置了OPENPAW_SETUP_HOME/其他覆盖变量，后续build/start/stop/status须使用相同设置；详见docs/engine-host/setup.md。');
  } finally { prompt.close(); }
}

module.exports = { locations, port, configuration, modelEnvironment, installState, createFresh, inside };
if (require.main === module) main().catch(error => { console.error(`openpaw-office: ${error.message}`); process.exitCode = 2; });
