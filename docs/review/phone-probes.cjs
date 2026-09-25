// Exercise the real app.js with a minimal DOM double, not a browser renderer.
// Regression checks for R06, R10 and R12; each assertion fails on the old code.
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const path = require('node:path');
class Element {
  constructor(tag) {
    this.tag = tag; this.children = []; this.dataset = {}; this.value = '';
    this.style = {}; this.className = ''; this.classList = { add() {}, remove() {}, toggle() {} };
    if (tag === 'select') Object.defineProperty(this, 'value', {
      get() { const o = this.children.filter(c => c instanceof Element); return this._value ?? (o.find(c => c.selected) || o[0])?.value ?? ''; },
      set(v) { this._value = v; },
    });
  }
  addEventListener() {}
  setAttribute(k, v) {
    if (k.startsWith('data-')) this.dataset[k.slice(5)] = v;
    else this[k] = v;
  }
  append(...kids) { this.children.push(...kids); }
  replaceChildren(...kids) { this.children = kids; }
  querySelector(sel) { return this.querySelectorAll(sel)[0] || null; }
  querySelectorAll(sel) {
    const parts = sel.split(' ');
    const matches = (e, s) => s.startsWith('.') ? e.className.split(' ').includes(s.slice(1)) : e.tag === s;
    const all = [];
    const walk = (e) => { for (const c of e.children || []) if (c instanceof Element) { all.push(c); walk(c); } };
    walk(this);
    if (parts.length > 1) return all.filter(e => matches(e, parts[0])).flatMap(e => e.querySelectorAll(parts.slice(1).join(' ')));
    return all.filter(e => matches(e, sel));
  }
}
const nodes = Object.fromEntries(['view', 'title', 'back', 'conn', 'gear'].map(id => [id, new Element('div')]));
const document = {
  querySelector: s => s.startsWith('#') ? nodes[s.slice(1)] : nodes.view.querySelector(s),
  querySelectorAll: s => nodes.view.querySelectorAll(s),
  createElement: t => new Element(t), createTextNode: t => t,
  addEventListener() {}, body: new Element('body'),
};
let artifacts = null;
const context = vm.createContext({ document, Node: Element, console,
  DOMParser: class { parseFromString() { return { body: { childNodes: [] } }; } },
  window: { addEventListener() {} }, navigator: {}, history: { length: 1 },
  location: { hash: '#/new' },
  localStorage: { getItem: k => k === 'cb.token' ? 'test-token' : null, setItem() {}, removeItem() {} },
  setTimeout() {}, clearTimeout() {},
  fetch: async (url) => url === '/api/artifacts'
    ? new Promise(resolve => { artifacts = () => resolve({ ok: true, status: 200, json: async () => ({ artifacts: [{ project: 'x', title: 'a', kind: 'html', modified: 0 }] }) }); })
    : { ok: true, status: 200, json: async () => ({ workflows: [], entries: [], html: '' }) },
});
const source = fs.readFileSync(path.join(__dirname, '../../src/phone/app.js'), 'utf8');
vm.runInContext(source.split('// ---------------------------------------------------------------- start\n')[0], context);
const run = code => vm.runInContext(code, context);

// R06: live state updates keep the new-task form as typed.
run(`state = {running:true, projects:[{id:'p',name:'project',tasks:[]},{id:'q',name:'other',tasks:[]}], agents:['claude','codex']}; render();`);
const prompt = nodes.view.querySelector('textarea');
const [project, agent] = nodes.view.querySelectorAll('select');
prompt.value = 'unsent\ntask prompt';
project.value = 'q';
agent.value = 'codex';
run(`state = {...state, projects: state.projects.map(p => ({...p, tasks:[{id:'t'+Math.random(),title:'x',agent:'claude',kind:'working'}]}))}; render(); render();`);
assert.equal(nodes.view.querySelector('textarea'), prompt, 'prompt element kept, so focus and caret stay');
assert.equal(prompt.value, 'unsent\ntask prompt');
assert.equal(nodes.view.querySelectorAll('select')[0].value, 'q');
assert.equal(nodes.view.querySelectorAll('select')[1].value, 'codex');
run(`state = {...state, projects: [...state.projects, {id:'r',name:'new',tasks:[]}]}; render();`);
assert.equal(project.value, 'q', 'a new project keeps the chosen one');
assert.equal(project.children.length, 3);
console.log('OK R06: state updates keep the new-task form');

// R10: a cold task deep link gains chat tabs once the task list arrives.
for (const agentName of ['claude', 'codex']) {
  run(`state = null; watching = null; view.replaceChildren(); location.hash = '#/task/t'; render();`);
  assert.equal(nodes.view.querySelector('.tabs').hidden, true);
  run(`state = {running:true, projects:[{id:'p',name:'project',tasks:[{id:'t',title:'task',agent:'${agentName}',kind:'idle'}]}]}; render();`);
  assert.ok(!nodes.view.querySelector('.tabs').hidden, agentName + ' tabs shown');
  assert.equal(run('tab'), 'chat');
}
console.log('OK R10: cold task links gain chat tabs');

// R12: an artifacts reply that arrives after leaving the page is dropped.
(async () => {
  run(`location.hash = '#/artifacts'; render();`);
  run(`location.hash = '#/new'; render();`);
  const form = nodes.view.children[0];
  nodes.view.querySelector('textarea').value = 'draft';
  artifacts();
  await new Promise(r => setImmediate(r));
  await new Promise(r => setImmediate(r));
  assert.equal(nodes.view.children[0], form);
  assert.equal(nodes.view.querySelector('textarea').value, 'draft');
  run(`location.hash = '#/artifacts'; render();`);
  artifacts();
  await new Promise(r => setImmediate(r));
  await new Promise(r => setImmediate(r));
  assert.equal(nodes.view.querySelectorAll('.row').length, 1, 'the current artifacts request still shows');
  console.log('OK R12: stale artifact responses leave the page alone');
})().catch(e => { console.error(e); process.exit(1); });
