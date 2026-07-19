// Drive the real Hermione pages with Playwright and capture PNGs of every
// screen, in both the dark "chalkboard" and light "whiteboard" themes.
//
// Assumes server.js is already listening on PORT. Freezes the clock so relative
// times ("6s ago", the freshness counter) are identical every run.
//
// Env:
//   PORT        server.js port (default 8799)
//   FIXED_NOW   epoch ms — MUST match the value server.js used (default date)
//   OUT_DIR     directory to write PNGs into (required)
//   LABEL       filename prefix, e.g. "before" / "after" (default "shot")
//   PW_CHROMIUM chromium executable (default /opt/pw-browsers/chromium)
//
// Playwright is often only installed globally; run with:
//   NODE_PATH=$(npm root -g) node shoot.js
const { chromium } = require('playwright');

const PORT = Number(process.env.PORT || 8799);
const NOW = Number(process.env.FIXED_NOW || Date.parse('2025-07-19T13:30:00Z'));
const OUT = process.env.OUT_DIR;
const LABEL = process.env.LABEL || 'shot';
const EXE = process.env.PW_CHROMIUM || '/opt/pw-browsers/chromium';
const BASE = `http://localhost:${PORT}`;

if (!OUT) { console.error('OUT_DIR is required'); process.exit(1); }

// Freeze Date so relative timestamps and the freshness clock are deterministic.
const freezeTime = `
  (() => {
    const FIXED = ${NOW};
    const _Date = Date;
    class FrozenDate extends _Date {
      constructor(...a){ if (a.length === 0) { super(FIXED); } else { super(...a); } }
      static now(){ return FIXED; }
    }
    globalThis.Date = FrozenDate;
  })();
`;

async function shoot(page, url, file, opts = {}) {
  await page.goto(url, { waitUntil: 'networkidle' });
  if (opts.action) await opts.action(page);
  await page.waitForTimeout(opts.wait || 500);
  await page.screenshot({ path: `${OUT}/${file}`, fullPage: !!opts.fullPage });
  console.log('  ✓ ' + file);
}

(async () => {
  const browser = await chromium.launch({ executablePath: EXE });

  for (const theme of ['dark', 'light']) {
    const ctx = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      deviceScaleFactor: 2,
      colorScheme: theme === 'light' ? 'light' : 'dark',
    });
    await ctx.addInitScript(freezeTime);
    // Pin the theme + skip the one-time "click a student" hint so the board is
    // clean. These localStorage keys are read by the pages before first paint.
    await ctx.addInitScript(t => {
      try {
        localStorage.setItem('hermione.theme', t);
        localStorage.setItem('hermione.hintSeen', '1');
      } catch (_) {}
    }, theme);
    const page = await ctx.newPage();

    console.log(`[${LABEL}] ${theme}`);
    await shoot(page, `${BASE}/`, `${LABEL}-board-${theme}.png`);

    // Board with a student's terminal open (click the first card).
    await shoot(page, `${BASE}/`, `${LABEL}-terminal-${theme}.png`, {
      action: async (pg) => { await pg.locator('.card').first().click(); },
      wait: 900,
    });

    await shoot(page, `${BASE}/analytics`, `${LABEL}-analytics-${theme}.png`, {
      action: async (pg) => { await pg.selectOption('#student', { label: 'Ada Lovelace' }).catch(() => {}); },
      wait: 600,
    });

    await shoot(page, `${BASE}/transcripts`, `${LABEL}-transcripts-${theme}.png`, {
      action: async (pg) => { await pg.locator('.conv').first().click(); },
      wait: 600,
    });

    await shoot(page, `${BASE}/login`, `${LABEL}-login-${theme}.png`);

    // Broadcast composer modal over the board.
    await shoot(page, `${BASE}/`, `${LABEL}-broadcast-${theme}.png`, {
      action: async (pg) => {
        await pg.click('#broadcast');
        await pg.fill('#bc-text', 'Reminder: push your work before the bell.');
      },
      wait: 500,
    });

    await ctx.close();
  }

  await browser.close();
  console.log('done → ' + OUT);
})();
