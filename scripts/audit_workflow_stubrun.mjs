// Stub run of the audit workflow script (.claude/workflows/audit.js): it loads
// the script as the harness runs its body, with stand-ins for agent, parallel,
// phase and log. No agent is spawned and nothing is sent anywhere: each agent()
// call is recorded and answered with an object that holds its schema's
// required fields. It prints the phases, each agent call's label, type and
// model, the findings form's fields, the prompt lines the audit reads its
// previous run from, the log lines and the per-category record.
//
// Usage: node scripts/audit_workflow_stubrun.mjs <workflow file, or - for standard input> [--null <category>] [--throw <category>]
//   --null <category>   that category's auditor returns null, as agent() does when a subagent dies on a terminal error
//   --throw <category>  that category's auditor throws; parallel() gives null for it, as the harness does
import { readFileSync } from 'node:fs'
const argv = process.argv.slice(2)
const opt = (name) => { const i = argv.indexOf(name); return i >= 0 ? argv[i + 1] : undefined }
const nullCat = opt('--null'), throwCat = opt('--throw')
const src = readFileSync(argv[0] === '-' ? 0 : argv[0], 'utf8').replace(/^export const meta =/m, '__box.meta =')
const body = `return (async () => {\n${src}\n})()`
const __box = {}
const calls = [], phases = [], logs = []
const fill = (schema) => {
  if (!schema) return 'text'
  const o = {}
  for (const k of schema.required || []) {
    const p = schema.properties[k]
    o[k] = k === 'rows' ? [{ id: 'R-1', title: 't', location: 'l', tier: 'P1', route: 'auto-fix', fix_approach: 'f' }, { id: 'R-2', title: 't', location: 'l', tier: 'P4', route: 'issue', fix_approach: 'f' }] : k === 'verdict' ? 'VERIFIED' : p.type === 'array' ? [] : p.enum ? p.enum[0] : 'x'
  }
  return o
}
const agent = async (prompt, opts = {}) => {
  if (!__box.schema && opts.schema) __box.schema = opts.schema
  calls.push({ label: opts.label, model: opts.model ?? '(inherit)', agentType: opts.agentType ?? '(default)', phase: opts.phase, prompt })
  if (nullCat !== undefined && opts.label === `audit:${nullCat}`) return null
  if (throwCat !== undefined && opts.label === `audit:${throwCat}`) throw new Error('stub: the auditor stopped')
  return fill(opts.schema)
}
const parallel = async (ts) => Promise.all(ts.map((t) => Promise.resolve().then(t).catch(() => null)))
const fn = new Function('__box', 'args', 'agent', 'parallel', 'phase', 'log', body)
const out = await fn(__box, { implement: true }, agent, parallel, (t) => phases.push(t), (m) => logs.push(m))
console.log('meta.phases:', JSON.stringify(__box.meta.phases.map((p) => p.title)))
console.log('phase() calls:', JSON.stringify(phases))
for (const c of calls) console.log(`${c.label}\t${c.agentType}\t${c.model}`)
const fs_ = __box.schema.properties.findings.items
console.log('findings fields:', Object.keys(fs_.properties).join(','), '| required:', fs_.required.join(','))
console.log('churn line:', (calls[0].prompt.match(/Churn focus: .*/) || [''])[0])
const synth = calls.find((c) => c.label === 'synthesize')
console.log('auditor dedup line:', (calls[0].prompt.split('\n').find((l) => l.startsWith('- Read ')) || '(none)'))
console.log('synthesize intro:', synth.prompt.split('\n')[0])
for (const l of synth.prompt.split('\n').filter((l) => /^[1267]\. /.test(l))) console.log('synthesize step:', l)
for (const m of logs) console.log('log:', m)
console.log('byCategory:', JSON.stringify(out.byCategory))
