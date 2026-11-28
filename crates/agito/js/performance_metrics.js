// Reads the page's Core Web Vitals from the browser's buffered performance entries. Needs
// cssPath, for naming the elements a layout shift moved.
(() => {
  const buffered = (type, extra) => {
    try {
      const observer = new PerformanceObserver(() => {});
      observer.observe(Object.assign({ type, buffered: true }, extra));
      const records = observer.takeRecords();
      observer.disconnect();
      return records;
    } catch (e) {
      return [];
    }
  };

  // LCP: the last candidate the browser reported, which stops changing at the first input.
  const lcpEntries = buffered('largest-contentful-paint');
  const lcp = lcpEntries.length ? lcpEntries[lcpEntries.length - 1].startTime : null;

  const paint = performance.getEntriesByName('first-contentful-paint')[0];
  const fcp = paint ? paint.startTime : null;

  const navigation = performance.getEntriesByType('navigation')[0];
  const ttfb = navigation && navigation.responseStart > 0
    ? navigation.responseStart - navigation.startTime
    : null;

  // CLS: the largest session window of shifts not caused by input, a window being shifts
  // under a second apart, at most five seconds long.
  const shifts = buffered('layout-shift').filter(e => !e.hadRecentInput);
  let cls = 0, window_ = 0, first = 0, last = 0;
  for (const e of shifts) {
    if (window_ && (e.startTime - last >= 1000 || e.startTime - first >= 5000)) {
      window_ = 0;
    }
    if (!window_) first = e.startTime;
    window_ += e.value;
    last = e.startTime;
    cls = Math.max(cls, window_);
  }
  const layoutShifts = shifts.map(e => ({
    score: e.value,
    time: e.startTime,
    sources: (e.sources || [])
      .map(s => (s.node && s.node.nodeType === 1 ? cssPath(s.node) : null))
      .filter(Boolean),
  }));

  // INP: the slowest interaction, ignoring one per fifty when there are many. The browser
  // buffers only interactions of 104 ms or more, so the first input, always buffered, stands
  // in when none were that slow.
  const worst = new Map();
  for (const e of buffered('event', { durationThreshold: 16 }).concat(buffered('first-input'))) {
    if (!e.interactionId) continue;
    worst.set(e.interactionId, Math.max(worst.get(e.interactionId) || 0, e.duration));
  }
  const durations = [...worst.values()].sort((a, b) => b - a);
  const inp = durations.length
    ? durations[Math.min(Math.floor(durations.length / 50), durations.length - 1)]
    : null;

  return { lcp, fcp, ttfb, cls, inp, layoutShifts };
})()
