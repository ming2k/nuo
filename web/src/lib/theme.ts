/**
 * Theme control for the 山水 design language.
 *
 * Two modes: 宣纸 light (default) and 夜山 dark, applied via
 * `data-theme` on <html> and persisted to localStorage. The initial
 * preference is resolved before Svelte mounts (see main.ts) so there is
 * no flash of the wrong paper.
 */

export type Theme = "light" | "dark";

const STORAGE_KEY = "muta.theme";

export function resolveInitialTheme(): Theme {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === "light" || stored === "dark") return stored;
  } catch {
    // localStorage unavailable (privacy mode) — fall through to media query.
  }
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // Persisting is best-effort; the in-DOM attribute already took effect.
  }
}

/** Toggle helper returning the new theme. */
export function toggleTheme(current: Theme): Theme {
  const next: Theme = current === "light" ? "dark" : "light";
  applyTheme(next);
  return next;
}
