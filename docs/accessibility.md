# Accessibility Checklist

This checklist covers the desktop UI and is verified by the frontend component tests, production type-check/build, and the contrast calculations recorded below.

## Verified

- [x] Setup, review, execution, and dialogs use named landmarks and headings.
- [x] Every input and select has a visible label or an accessible name; icon-only controls have `aria-label` text.
- [x] Folder validation, scan progress, result counts, review decisions, and completion/error states are announced with status or alert semantics.
- [x] Review categories use tab semantics and support Left/Right/Home/End navigation; the virtualized review table supports Arrow, Home/End, and Page Up/Down navigation.
- [x] Search, status/decision filters, duplicate-group pagination, and dialogs are operable without a pointer.
- [x] Keyboard focus has a visible 3px outline. Light focus outline `#126b59` against white is 6.42:1; dark focus outline `#8ce0bf` against `#1d2a26` exceeds 9:1.
- [x] Reduced-motion preferences disable smooth scrolling and minimize transitions.
- [x] System, light, and dark themes are available and the selected preference persists locally. Dark-theme main/muted text contrast is 12.53:1 / 7.93:1; light main/muted text contrast is at least 11.76:1 / 6.05:1. Primary button text contrast is 6.42:1.
- [x] Long paths wrap or truncate with a title/full accessible text, and narrow layouts retain horizontal table access without clipping the page.
- [x] Empty result sets and unavailable history/duplicate data have explicit messages rather than blank panels.

## Automated Evidence

Run `pnpm --dir apps/desktop test` and `pnpm --dir apps/desktop build`. The UI tests cover keyboard-only review through 20,000 virtualized rows, tab navigation, focusable controls, filter behavior, empty states, history browsing, theme persistence, and duplicate reporting. Rust tests verify persistent rejection memory, profile/history data, and exact duplicate evidence.

Contrast ratios use WCAG relative luminance calculations for the foreground/background pairs listed above. Native OS dialog behavior and screen-reader pronunciation are platform-dependent and should still be checked during release testing.
