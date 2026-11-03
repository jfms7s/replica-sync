/** The longest trash age accepted (100 years); Rust refuses more too. */
export const MAX_TRASH_DAYS = 36500;

/** A trash age typed into a field, or null when it is empty, fractional, below 1 or above the maximum. */
export function parseTrashDays(text: string): number | null {
  const n = Number(text);
  return text.trim() !== '' && Number.isInteger(n) && n >= 1 && n <= MAX_TRASH_DAYS ? n : null;
}
