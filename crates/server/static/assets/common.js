// Shared by every teacher page: the theme, HTML escaping, small formatters, and
// the header's "more" menu. Loaded after the header markup, before the page's own
// script, so a page can use these as globals.

// ---- theme: OS default + manual override (dark "chalkboard" / light) ----
const THEME_KEY = 'hermione.theme';
const lightMQ = window.matchMedia('(prefers-color-scheme: light)');
const THEME_ICONS = {
  sun: '<svg class="ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" aria-hidden="true"><circle cx="8" cy="8" r="3.2"/><path d="M8 1v1.8M8 13.2V15M1 8h1.8M13.2 8H15M3 3l1.3 1.3M11.7 11.7 13 13M13 3l-1.3 1.3M4.3 11.7 3 13"/></svg>',
  moon: '<svg class="ico" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M6.2 2.3a5.8 5.8 0 1 0 7.5 7.5A4.7 4.7 0 0 1 6.2 2.3z"/></svg>',
};
// Effective theme: an explicit data-theme wins; otherwise follow the OS.
function effectiveTheme() {
  const pinned = document.documentElement.dataset.theme;
  if (pinned === 'light' || pinned === 'dark') return pinned;
  return lightMQ.matches ? 'light' : 'dark';
}
// Reflect the live theme in the menu item, and tell the page — the board has
// terminals whose colours are set from script, not from CSS.
function applyTheme() {
  const t = effectiveTheme();
  const btn = document.getElementById('theme-toggle');
  if (btn) {
    btn.innerHTML = (t === 'dark' ? THEME_ICONS.sun : THEME_ICONS.moon)
      + (t === 'dark' ? 'Light theme' : 'Chalkboard theme');
  }
  document.dispatchEvent(new Event('hermione:theme'));
}
function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  try { localStorage.setItem(THEME_KEY, theme); } catch (_) {}
  applyTheme();
}
// When the teacher hasn't pinned a theme, track OS changes live.
lightMQ.addEventListener('change', () => {
  if (!document.documentElement.dataset.theme) applyTheme();
});
document.getElementById('theme-toggle')?.addEventListener('click', () =>
  setTheme(effectiveTheme() === 'dark' ? 'light' : 'dark'));
applyTheme(); // paint the right label for the resolved theme

// ---- projector mode: larger type, and student names blurred ----
// A teacher's screen is often a room's screen. "Larger text" scales the type
// tokens; "Blur names" hides student names until the pointer or keyboard focus is
// on them, so the teacher can still tell who is who but the class isn't shown
// each other's names. Both are remembered, and shared by every teacher page.
const DISPLAY_KEY = 'hermione.display';
const display = (() => {
  let saved = {};
  try { saved = JSON.parse(localStorage.getItem(DISPLAY_KEY) || '{}') || {}; } catch (_) {}
  return { large: !!saved.large, hideNames: !!saved.hideNames };
})();
function applyDisplay() {
  document.documentElement.classList.toggle('projector', display.large);
  document.documentElement.classList.toggle('hide-names', display.hideNames);
  for (const [id, on, label] of [
    ['large-toggle', display.large, 'Larger text'],
    ['names-toggle', display.hideNames, 'Blur student names'],
  ]) {
    const b = document.getElementById(id);
    if (!b) continue;
    b.textContent = label + (on ? ': on' : '');
    b.setAttribute('aria-checked', String(on));
  }
  document.dispatchEvent(new Event('hermione:display'));
}
function setDisplay(patch) {
  Object.assign(display, patch);
  try { localStorage.setItem(DISPLAY_KEY, JSON.stringify(display)); } catch (_) {}
  applyDisplay();
}
(function () {
  const theme = document.getElementById('theme-toggle');
  if (!theme) return;
  for (const [id, key] of [['large-toggle', 'large'], ['names-toggle', 'hideNames']]) {
    const b = document.createElement('button');
    b.id = id; b.type = 'button';
    b.setAttribute('role', 'menuitemcheckbox');
    b.addEventListener('click', () => setDisplay({ [key]: !display[key] }));
    theme.before(b);
  }
  applyDisplay();
})();

// ---- helpers ----
// Escape untrusted strings (student names, file paths, exercises, course
// names) before putting them in HTML — they originate from students.
const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
function esc(s) {
  s = String(s == null ? '' : s);
  // Most spans of source code contain nothing to escape, and this runs
  // thousands of times per file render.
  return /[&<>"']/.test(s) ? s.replace(/[&<>"']/g, c => ESC[c]) : s;
}
function hashHue(str) {
  let h = 0;
  for (let i = 0; i < str.length; i++) h = (h * 31 + str.charCodeAt(i)) % 360;
  return h;
}
function initials(name) {
  const parts = name.split(/[\s._:\-]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}
function fmtDuration(secs) {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60), s = secs % 60;
  if (m < 60) return s ? `${m}m ${s}s` : `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}h ${String(m % 60).padStart(2, '0')}m`;
}

// The header's "more" menu: a button that opens a short list. It closes on
// Escape (returning focus to its button), on a click elsewhere, when focus
// leaves it, and after an item is chosen; the arrow keys, Home and End move
// between items. Markup: #more-wrap > #more (button) + #more-menu (role=menu).
(function () {
  const wrap = document.getElementById('more-wrap');
  const btn = document.getElementById('more');
  const menu = document.getElementById('more-menu');
  if (!wrap || !btn || !menu) return;
  const items = () => [...menu.querySelectorAll('[role="menuitem"]')];
  const setOpen = (open, focusFirst = false) => {
    menu.hidden = !open;
    btn.setAttribute('aria-expanded', String(open));
    if (open && focusFirst) items()[0].focus();
  };
  btn.addEventListener('click', () => setOpen(menu.hidden, true));
  // In the capture phase, so it runs before the chosen item's own handler: focus
  // goes back to the button first, which is what a dialog the item opens will
  // hand focus back to — the item itself is hidden once the menu closes.
  menu.addEventListener('click', () => { btn.focus(); setOpen(false); }, true);
  document.addEventListener('click', (e) => { if (!wrap.contains(e.target)) setOpen(false); });
  wrap.addEventListener('focusout', (e) => { if (!wrap.contains(e.relatedTarget)) setOpen(false); });
  wrap.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !menu.hidden) { setOpen(false); btn.focus(); return; }
    const list = items(), at = list.indexOf(document.activeElement);
    if (e.target === btn && e.key === 'ArrowDown') { e.preventDefault(); setOpen(true, true); }
    else if (at >= 0 && e.key === 'ArrowDown') { e.preventDefault(); list[(at + 1) % list.length].focus(); }
    else if (at >= 0 && e.key === 'ArrowUp') { e.preventDefault(); list[(at - 1 + list.length) % list.length].focus(); }
    else if (at >= 0 && e.key === 'Home') { e.preventDefault(); list[0].focus(); }
    else if (at >= 0 && e.key === 'End') { e.preventDefault(); list[list.length - 1].focus(); }
  });
})();
