# Paperboy design system

Home printing for a family. The interface combines the simplicity of a small macOS utility
with apple.com's spacious typography and restrained controls.

References: [Apple HIG](https://developer.apple.com/design/human-interface-guidelines/),
[Impeccable](https://impeccable.style/), [Taste Skill](https://tasteskill.dev/),
[Apple Mac](https://www.apple.com/mac/).
Impeccable frontend-design, onboarding, and polish guidance inform the implementation.
Taste Skill's contextual hierarchy, restraint, consistent geometry, and state handling apply;
its marketing layout and animation defaults do not fit this home utility.

- System font stack, native controls, no external fonts or assets.
- White canvas, softly grouped neutral surfaces, one blue accent. No glass effects or shadows.
- Open lists use fine separators, rather than enclosing every section in a card.
- 8px spacing rhythm, 12px inputs, 24px grouped surfaces, pill-shaped actions.
- 56px desktop page titles, 40px mobile titles, 17px body text. Larger type earns space;
  files and settings stay compact enough to scan. Owner access uses a centered 64px headline.
- Quiet 64px header, simple text navigation with a visible active underline.
- Activity filters use a neutral capsule and a high-contrast selected pill.
- One primary action per setup step. Advanced details expand inline.
- 44px minimum interactive targets, visible focus, explicit labels, live error feedback.
- Color supplements readable status labels. Light and dark themes respect device preference.
- Reduced motion disables transitions. No decorative or continuous motion.
- Queue says “Sent to printer” until CUPS reports completion. Preview data is clearly labeled.
- Setup: owner access → Resend → printer → approved senders → first email.
- Navigation: Activity, People, Settings. Mobile uses the same labels and actions.
- Updates stay in the existing grouped Settings layout: readable installed version, one
  install/check action, a labeled automatic-update switch, and quiet release notes.
  Taste Skill's preserve-mode redesign and theme/accessibility checks guide this iteration.
  Pinned, waiting, offline, and unmanaged states use plain copy and progressive disclosure.
