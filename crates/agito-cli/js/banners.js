// Removes cookie-consent and privacy banners before a capture, and returns how many it removed.
//
// Known consent managers are removed by their ids. Anything else whose class or id mentions
// cookies, consent, GDPR or privacy is removed only through its outermost fixed or sticky
// ancestor, the part that floats over the page. Matching names alone would take out a footer's
// privacy link, an article about cookies, or the whole page when <body> carries such a class.
(() => {
  const vendors = [
    '#onetrust-banner-sdk',
    '#onetrust-consent-sdk',
    '.onetrust-pc-dark-filter',
    '#CybotCookiebotDialog',
    '#CybotCookiebotDialogBodyUnderlay',
    '#usercentrics-root',
    '#didomi-host',
    '.qc-cmp2-container',
    '[id^="sp_message_container"]',
    '.cc-window',
    '.cc-banner',
    '#cookieConsent',
  ];
  const named = ['cookie', 'consent', 'gdpr', 'privacy']
    .flatMap((word) => [`[class*="${word}" i]`, `[id*="${word}" i]`]);

  const root = document.documentElement;
  const body = document.body;
  const targets = new Set();

  for (const selector of vendors) {
    for (const el of document.querySelectorAll(selector)) {
      if (el !== root && el !== body) targets.add(el);
    }
  }

  const floating = (el) => {
    const position = getComputedStyle(el).position;
    return position === 'fixed' || position === 'sticky';
  };
  for (const el of document.querySelectorAll(named.join(','))) {
    let outermost = null;
    for (let node = el; node && node !== body && node !== root; node = node.parentElement) {
      if (floating(node)) outermost = node;
    }
    if (outermost) targets.add(outermost);
  }

  // Count only the outermost of nested targets: removing it takes the rest with it.
  const removed = [...targets].filter(
    (el) => ![...targets].some((other) => other !== el && other.contains(el)),
  );
  for (const el of removed) el.remove();

  // Consent dialogs lock scrolling while open; unlock it once they are gone.
  if (removed.length > 0) {
    for (const el of [root, body]) {
      if (!el) continue;
      el.style.overflow = '';
      el.style.position = '';
    }
    body?.classList.remove('no-scroll', 'modal-open', 'overflow-hidden');
  }
  return removed.length;
})()
