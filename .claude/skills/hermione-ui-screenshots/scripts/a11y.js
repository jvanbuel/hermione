// Automated accessibility audit (axe-core) of every teacher page, in both themes,
// with the interesting states open: the tree, the header menu, a file pane, a diff
// pane and the broadcast dialog. Prints violations grouped by rule; exits 1 if any.
//
//   PORT=8799 node scripts/server.js &      # the fixtures server
//   npm i axe-core                          # once, anywhere on NODE_PATH
//   PORT=8799 node scripts/a11y.js
//
// It finds real problems the contrast table can't (missing landmarks, ARIA that
// a widget needs), and it read the *rendered* colours, so it also catches a token
// used on a ground nobody listed.
const { chromium } = require('playwright'); const fs = require('fs');
const axeSrc = fs.readFileSync(require.resolve('axe-core/axe.min.js'), 'utf8');
const BASE = `http://localhost:${process.env.PORT || 8799}`;
const scenes = {
  'board': async (p) => {},
  'tree': async (p) => { await p.locator('button[data-view="tree"]').click(); await p.waitForTimeout(500); },
  'menu open': async (p) => { await p.locator('#more').click(); await p.waitForTimeout(200); },
  'file pane': async (p) => { await p.locator('.card', { hasText: 'Ada Lovelace' }).first().click(); await p.locator('.pane').last().locator('.modes button[data-mode="file"]').click(); await p.waitForTimeout(900); },
  'diff pane': async (p) => { await p.locator('.card', { hasText: 'Ada Lovelace' }).first().click(); await p.locator('.pane').last().locator('.modes button[data-mode="file"]').click(); await p.locator('.pane').last().locator('.diff').click(); await p.waitForTimeout(900); },
  'broadcast modal': async (p) => { await p.locator('#broadcast').click(); await p.waitForTimeout(300); },
  'analytics': null, 'transcripts': null, 'login': null,
};
(async () => {
  const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium' });
  const seen = new Map();
  for (const theme of ['dark', 'light']) {
    for (const [name, act] of Object.entries(scenes)) {
      const ctx = await b.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: theme });
      await ctx.addInitScript((t) => { localStorage.setItem('hermione.hintSeen', '1'); localStorage.setItem('hermione.theme', t); }, theme);
      const page = await ctx.newPage();
      const url = act ? '/' : '/' + name;
      await page.goto(BASE + url, { waitUntil: 'networkidle' }); await page.waitForTimeout(600);
      if (act) await act(page);
      await page.addScriptTag({ content: axeSrc });
      const res = await page.evaluate(() => axe.run(document, { resultTypes: ['violations'] }));
      for (const v of res.violations) {
        const key = v.id + '|' + name;
        const e = seen.get(key) || { id: v.id, impact: v.impact, help: v.help, scene: name, themes: new Set(), nodes: [] };
        e.themes.add(theme);
        for (const n of v.nodes.slice(0, 3)) e.nodes.push(n.target.join(' ') + '  ::  ' + (n.failureSummary || '').split('\n').slice(1, 2).join(' ').trim());
        seen.set(key, e);
      }
      await ctx.close();
    }
  }
  await b.close();
  const list = [...seen.values()].sort((a, b) => ['critical','serious','moderate','minor'].indexOf(a.impact) - ['critical','serious','moderate','minor'].indexOf(b.impact));
  if (!list.length) console.log('axe: no violations in any scene or theme');
  process.exitCode = list.length ? 1 : 0;
  for (const e of list) { console.log(`\n[${e.impact}] ${e.id} — ${e.help}\n  scene: ${e.scene}  themes: ${[...e.themes].join(',')}`); for (const n of [...new Set(e.nodes)].slice(0, 4)) console.log('   -', n.slice(0, 220)); }
})();
