import '@testing-library/jest-dom/vitest';

// jsdom has no layout. The Preview tree is virtualised and draws only the rows that fit its scroll
// container, so give `.tree` a height (25 rows of 24 px) and make `scrollTo` move it like a browser.
Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
  configurable: true,
  get(this: HTMLElement) { return this.classList.contains('tree') ? 600 : 0; },
});
Object.defineProperty(HTMLElement.prototype, 'offsetWidth', {
  configurable: true,
  get(this: HTMLElement) { return this.classList.contains('tree') ? 800 : 0; },
});
Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
  configurable: true,
  get(this: HTMLElement) { return this.classList.contains('tree') ? 600 : 0; },
});
// What the tree's spacer says the content is (the virtualiser clamps scrolls to it).
Object.defineProperty(HTMLElement.prototype, 'scrollHeight', {
  configurable: true,
  get(this: HTMLElement) {
    if (!this.classList.contains('tree')) return 0;
    return parseFloat((this.lastElementChild as HTMLElement | null)?.style.height ?? '') || 0;
  },
});
HTMLElement.prototype.scrollTo = function (this: HTMLElement, opts?: ScrollToOptions | number) {
  if (typeof opts === 'object' && opts.top !== undefined) this.scrollTop = opts.top;
  this.dispatchEvent(new Event('scroll'));
} as typeof HTMLElement.prototype.scrollTo;
