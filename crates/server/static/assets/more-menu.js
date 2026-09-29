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
  menu.addEventListener('click', () => setOpen(false));
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
