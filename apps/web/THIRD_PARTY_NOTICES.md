# Third-party notices

## T3 Code

Parts of this UI are adapted from T3 Code (https://github.com/pingdotgg/t3code, `apps/web`,
commit `65b2e6dae66f15caa03abf57ba2fe67da1d769aa`). Only presentational code was taken; none of
T3 Code's data layer, accounts, telemetry, cloud, translation, git, worktree, pull-request,
terminal or browser-preview code is included.

| Vetro file | Taken from T3 Code |
| --- | --- |
| `src/index.css` | Theme tokens (light and dark palettes, sidebar, message and composer surfaces), `surface-glass` / `dropdown-glass` / `dialog-glass` / `dialog-backdrop` / `live-tool-shine` utilities, the 2xs/3xs text sizes, status pulse animation, scrollbar styling and the chat markdown rules, from `src/index.css`. |
| `src/components/ui/button.tsx` | Button variants and sizes from `src/components/ui/button.tsx`. |
| `src/components/ui/primitives.tsx` | Badge, Kbd, Switch and input styling from `src/components/ui/{badge,kbd,switch,input}.tsx`. |
| `src/components/ui/overlay.tsx` | Tooltip, menu, popover and dialog popup styling from `src/components/ui/{tooltip,menu,dialog,dialog-styles}.tsx`. |
| `src/components/chat/WorkLog.tsx` | Work-log row layout from `src/components/chat/WorkLog.tsx`. |
| `src/components/chat/OptionPicker.tsx` | Composer control styling from `src/components/chat/ComposerControl.tsx`. |
| `src/components/chat/Composer.tsx` | Send and stop button styling from `src/components/chat/ComposerPrimaryActions.tsx`; composer surface from `ComposerSurface.tsx`. |
| `src/components/sidebar/Sidebar.tsx` | Project folder header and thread row styling from `src/components/sidebar/SidebarProjectFolderHeader.tsx` and `SidebarThreadRow.tsx`. |
| `src/components/chat/Items.tsx` | User message bubble styling from `TimelineUserMessageRow.tsx`. |
| `src/components/AgentIcon.tsx`, `src/assets/antigravity.png` | OpenAI and Claude marks from `src/components/Icons.tsx`; the Antigravity image from `apps/mobile/assets/antigravity.png`. The marks belong to their owners and are used only to identify each agent. |
| `src/components/QrCode.tsx` | QR path rendering from `src/components/ui/qr-code.tsx`. |
| `src/lib/qrCode.ts` | T3 Code's vendored copy (`packages/shared/src/qrCode.ts`) of the Project Nayuki QR Code generator; its own MIT notice is kept in the file. |

```
MIT License

Copyright (c) 2026 T3 Tools Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Bundled packages

The built UI also contains these npm packages, each under its own license: React and React DOM
(MIT), Base UI (MIT), lucide-react (ISC), zustand (MIT), class-variance-authority (Apache-2.0),
tailwind-merge (MIT), marked (MIT), highlight.js (BSD-3-Clause) and Inter via
`@fontsource-variable/inter` (SIL Open Font License 1.1).
