# Paperboy design system

Home printing for a family. The interface should feel as simple as a small macOS utility.

References: [Apple HIG](https://developer.apple.com/design/human-interface-guidelines/),
[Impeccable](https://impeccable.style/), [Taste Skill](https://tasteskill.dev/).
Impeccable frontend-design, onboarding, and polish guidance inform the implementation.
Taste Skill's contextual hierarchy, restraint, consistent geometry, and state handling apply;
its marketing layout and animation defaults do not fit this home utility.

- System font stack, native controls, no external fonts or assets.
- Neutral off-white canvas, solid white surfaces, one blue accent. No glass effects.
- 8px spacing rhythm, 10px controls, 16px grouped surfaces. Border before shadow.
- One primary action per setup step. Advanced details expand inline.
- 44px minimum interactive targets, visible focus, explicit labels, live error feedback.
- Color supplements readable status labels. Light and dark themes respect device preference.
- Reduced motion disables transitions. No decorative or continuous motion.
- Queue says “Sent to printer” until CUPS reports completion. Preview data is clearly labeled.
- Setup: owner access → Resend → printer → approved senders → first email.
- Navigation: Activity, People, Settings. Mobile uses the same labels and actions.
