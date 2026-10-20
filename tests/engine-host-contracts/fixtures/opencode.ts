// Compile against the actual SDK root (v1), never ./v2. No forced type casts.
import type {
  Session, UserMessage, AssistantMessage, TextPart, ToolPart, Permission,
  Event, GlobalEvent, ProviderListResponse, SessionPromptAsyncData,
  PostSessionIdPermissionsPermissionIdData, SessionMessagesResponse, Todo,
} from '@opencode-ai/sdk'

export const session = {
  id: 'ses_fixture', projectID: 'prj_fixture', directory: '/workspace/project',
  title: '读取文件', version: 'openpaw-host-v1',
  time: { created: 1792458000000, updated: 1792458001000 },
} satisfies Session

export const user = {
  id: 'msg_user', sessionID: session.id, role: 'user', time: { created: 1792458000000 },
  agent: 'build', model: { providerID: 'mock', modelID: 'mock-1' },
} satisfies UserMessage

export const assistant = {
  id: 'msg_assistant', sessionID: session.id, role: 'assistant', parentID: user.id,
  time: { created: 1792458000001, completed: 1792458001000 },
  modelID: 'mock-1', providerID: 'mock', mode: 'build',
  path: { cwd: session.directory, root: session.directory }, cost: 0,
  tokens: { input: 1, output: 2, reasoning: 0, cache: { read: 0, write: 0 } }, finish: 'stop',
} satisfies AssistantMessage

export const text = {
  id: 'prt_text', sessionID: session.id, messageID: assistant.id, type: 'text', text: '完成',
} satisfies TextPart

const toolBase = {
  id: 'prt_tool', sessionID: session.id, messageID: assistant.id, callID: 'call_1', tool: 'write_file',
}
export const pendingTool = {
  ...toolBase, type: 'tool', state: { status: 'pending', input: { path: 'note.txt' }, raw: '{"path":"note.txt"}' },
} satisfies ToolPart
export const runningTool = {
  ...toolBase, type: 'tool', state: { status: 'running', input: { path: 'note.txt' }, time: { start: 1792458000010 } },
} satisfies ToolPart
export const completedTool = {
  ...toolBase, type: 'tool', state: {
    status: 'completed', input: { path: 'note.txt' }, output: 'written', title: 'write_file',
    metadata: {}, time: { start: 1792458000010, end: 1792458000020 },
  },
} satisfies ToolPart
export const failedTool = {
  ...toolBase, type: 'tool', state: {
    status: 'error', input: { path: 'note.txt' }, error: 'permission rejected',
    time: { start: 1792458000010, end: 1792458000020 },
  },
} satisfies ToolPart

export const permission = {
  id: 'prm_fixture', type: 'write_file', pattern: ['note.txt'], sessionID: session.id,
  messageID: assistant.id, callID: 'call_1', title: '允许本次写入？',
  metadata: { inputDigest: 'fixture-digest' }, time: { created: 1792458000005 },
} satisfies Permission

export const events = [
  { type: 'server.connected', properties: {} },
  { type: 'session.created', properties: { info: session } },
  { type: 'session.updated', properties: { info: session } },
  { type: 'session.status', properties: { sessionID: session.id, status: { type: 'busy' } } },
  { type: 'message.updated', properties: { info: user } },
  { type: 'message.updated', properties: { info: assistant } },
  { type: 'message.part.updated', properties: { part: pendingTool } },
  { type: 'permission.updated', properties: permission },
  { type: 'permission.replied', properties: { sessionID: session.id, permissionID: permission.id, response: 'once' } },
  { type: 'message.part.updated', properties: { part: runningTool } },
  { type: 'message.part.updated', properties: { part: completedTool } },
  { type: 'message.part.updated', properties: { part: failedTool } },
  { type: 'message.part.updated', properties: { part: { ...text, text: '' } } },
  { type: 'message.part.updated', properties: { part: text, delta: text.text } },
  { type: 'todo.updated', properties: { sessionID: session.id, todos: [] } },
  { type: 'session.status', properties: { sessionID: session.id, status: { type: 'idle' } } },
  { type: 'session.idle', properties: { sessionID: session.id } },
] satisfies Event[]

export const globalEvent = { directory: session.directory, payload: events[1] } satisfies GlobalEvent
export const messages = [{ info: user, parts: [] }, { info: assistant, parts: [text, completedTool] }] satisfies SessionMessagesResponse
export const todos = [{ id: 'todo_1', content: '验收', status: 'pending', priority: 'medium' }] satisfies Todo[]
export const providers = {
  all: [{
    id: 'mock', name: 'OpenPaw mock', env: [],
    models: { 'mock-1': {
      id: 'mock-1', name: 'mock-1', release_date: '2026-10-20',
      attachment: false, reasoning: false, temperature: false, tool_call: true,
      limit: { context: 4096, output: 1024 }, options: {},
    } },
  }],
  default: { mock: 'mock-1' }, connected: ['mock'],
} satisfies ProviderListResponse

export const prompt = {
  url: '/session/{id}/prompt_async', path: { id: session.id },
  body: { messageID: user.id, model: { providerID: 'mock', modelID: 'mock-1' }, parts: [{ type: 'text', text: '读取文件' }] },
} satisfies SessionPromptAsyncData
export const reply = {
  url: '/session/{id}/permissions/{permissionID}', path: { id: session.id, permissionID: permission.id }, body: { response: 'once' },
} satisfies PostSessionIdPermissionsPermissionIdData

// Regressions for mixed-version examples in the superseded master-plan.
// @ts-expect-error v1 has permission.updated, not permission.asked
export const invalidPermission: Event = { type: 'permission.asked', properties: permission }
// @ts-expect-error v1 session.created wraps info
export const invalidSession: Event = { type: 'session.created', properties: session }
// @ts-expect-error v1 reply uses response, not reply
export const invalidReply: PostSessionIdPermissionsPermissionIdData = { path: reply.path, body: { reply: 'once' } }
