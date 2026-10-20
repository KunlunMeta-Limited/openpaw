// Fixed upstream schemas are test-only, never part of the Host dependency graph.
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))
const manifest = JSON.parse(await readFile(new URL('../../docs/engine-host/contracts/sources.json', import.meta.url), 'utf8'))
const argIndex = process.argv.indexOf('--office-source')
const localSource = argIndex >= 0 ? process.argv[argIndex + 1] : undefined
if (argIndex >= 0 && !localSource) throw new Error('--office-source requires a checkout directory')
const digest = bytes => createHash('sha256').update(bytes).digest('hex')
let count = 0
for (const item of manifest.office.files) {
  // With a source checkout also verify the linked documentation baseline.
  if (!localSource && !item.testSnapshot) continue
  const output = join(here, 'upstream-office', item.path)
  const bytes = localSource
    ? execFileSync('git', ['-C', resolve(localSource), 'show', manifest.office.commit + ':' + item.path], { maxBuffer: 4 * 1024 * 1024 })
    : await readFile(output)
  if (digest(bytes) !== item.sha256) throw new Error('Office source hash mismatch: ' + item.path)
  if (localSource && item.testSnapshot) {
    await mkdir(dirname(output), { recursive: true })
    await writeFile(output, bytes)
  }
  count++
}
console.log('Verified ' + count + ' Office source files at ' + manifest.office.commit)
