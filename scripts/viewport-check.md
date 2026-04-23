# Manual viewport sweep

Run this checklist before cutting a release. Boot the stack per README, then
for each viewport walk through the checks below using:

| Device | Width |
|---|---|
| Mobile portrait | 375px |
| Tablet | 768px |
| Laptop | 1024px |
| Desktop | 1440px |

## Global
- [ ] No horizontal scroll on the body.
- [ ] Sidebar either shows or collapses to an overlay below 768px (if implemented; if not, verify the sidebar remains usable at 375px without truncating actions).
- [ ] Top bar (`#top-bar`) does not overlap the workspace switcher.
- [ ] Health dot is visible and tappable.

## Chat panel
- [ ] Chat chips (demo) render in a 2x2 grid at >=768px, single column at 375px.
- [ ] Textarea has min-height of 44px on touch viewports.
- [ ] Remediation card (when Ollama is down) is fully readable at 375px with no text cut off.

## Connectors tab
- [ ] Connector cards stack vertically at 375px.
- [ ] Timeline rows are readable (not overflowing) at 375px.
- [ ] Add Connector modal: provider grid switches to single column <=420px.
- [ ] Test-connection button has min tap target of 44x44.

## Signals / Survivors / Approvals
- [ ] List items don't overflow horizontally.
- [ ] Filter controls remain tappable at 375px.

## Connect-to-MCP dialog
- [ ] Tile grid falls to single column <=480px.
- [ ] Clients table scrolls horizontally or wraps at 375px without breaking layout.
- [ ] Copy buttons are tappable (>=44x44) at 375px.

## Activation tracker
- [ ] Checklist list items wrap; check glyph doesn't get cut off.
- [ ] CTA buttons remain tappable at 375px.

## Keyboard
- [ ] Tab through every interactive element on each tab in order.
- [ ] No focus traps except modals (which trap focus intentionally).
- [ ] Escape closes every dialog.
- [ ] Enter submits chat input.

## Screen reader (VoiceOver on macOS or NVDA on Windows)
- [ ] Workspace switcher announces "Switch workspace".
- [ ] Tab bar announces each tab name and selected state.
- [ ] Chips announce the prompt text.
- [ ] Lock glyph announces "Read-only sample workspace".
- [ ] Health dot announces current state (ok or error).
- [ ] Live-region updates (toasts, remediation, SSE reconnecting) are announced.

## Reduced motion
- [ ] Set macOS System Settings -> Accessibility -> Display -> Reduce motion.
- [ ] Toasts appear without fade animation.
- [ ] Progress view stages transition without slide animation.

## Outcome
Record any FAIL items in the PR and either fix in the same PR or open follow-up issues. No FAIL outstanding before tagging a release.
