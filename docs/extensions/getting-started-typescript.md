# TypeScript Extension Developer Guide

This guide walks you through authoring, building, validating, and testing a Shilpo desktop extension using TypeScript and the official `@shilpo/ext-sdk` SDK.

---

## 1. Scaffolding a New Extension

Use the Shilpo CLI to generate a fully configured TypeScript project with lockfile, manifest, and starter source code:

```bash
shilpo ext new my-extension --typescript --starter bar-widget --npm
```

### Available Starters

- `bar-widget`: Compact status bar pill with click interactions.
- `desktop-widget`: Resizable desktop canvas card widget.
- `settings-page`: Configuration settings panel.
- `side-panel`: Full-height dockable side panel.
- `action`: Command palette action with keybinding.
- `empty`: Minimal skeleton with lifecycle hooks.

---

## 2. Project Layout

```
my-extension/
├── extension.toml          # Extension manifest (ID, contributions, capabilities)
├── settings.schema.json    # JSON schema for configurable settings
├── package.json            # NPM configuration with pinned @shilpo/ext-sdk and @bytecodealliance/jco
├── package-lock.json       # Committed lockfile for reproducible builds
├── .npmrc                  # JSR registry configuration (@jsr:registry=https://npm.jsr.io)
├── tsconfig.json           # TypeScript configuration
├── src/
│   └── extension.ts        # Main extension implementation
└── README.md
```

---

## 3. Authoring the Extension

In `src/extension.ts`, define your extension using `defineExtension` and declarative ViewTree builder primitives:

```typescript
import { buildViewTree, button, defineExtension, icon, row, text } from "@shilpo/ext-sdk";
import type { Activation, ExtensionEvent, ViewTree } from "@shilpo/ext-sdk";

let clicks = 0;

const ext = defineExtension({
  onActivate(act: Activation) {
    console.log(`Activated by ${act.origin}`);
  },

  onEvent(event: ExtensionEvent) {
    if (event.tag === "input" && event.val.eventId === "btn-click") {
      clicks += 1;
    }
  },

  view(contributionId: string): ViewTree | undefined {
    if (contributionId === "my-bar-widget") {
      return buildViewTree(
        row({
          gap: 6,
          alignItems: "center",
          children: [
            icon("stars", { size: 16 }),
            text(`Clicks: ${clicks}`, { bold: true }),
            button("Increment", "btn-click", { style: { padding: 4 } }),
          ],
        }),
      );
    }
    return undefined;
  },
});

export const activate = ext.activate;
export const deactivate = ext.deactivate;
export const onEvent = ext.onEvent;
export const view = ext.view;
```

---

## 4. Building the WebAssembly Component

Compile TypeScript into a WebAssembly Component targeting `shilpo:extension@0.1.0` using the pinned QuickJS backend:

```bash
shilpo ext build
```

Under the hood, this executes `jco componentize` with the synchronous QuickJS backend (`--backend qjs --backend-qjs-disable-async`) to produce a compact, fast-starting component binary.

---

## 5. Validating Manifest and Component

Run the bounded extension checker to verify manifest validity, settings schemas, and component interface conformity:

```bash
shilpo ext check my-extension
```

Output:

```
info[manifest.valid]: 'org.example.my-extension' 0.1.0
info[wasm.valid]: component interface validated at extension.wasm
info[settings.valid]: schema and defaults validated at settings.schema.json
```

---

## 6. Live Hot-Reloading Development

To test your extension live in a running Shilpo shell with hot-reloading on every file change:

```bash
# Terminal 1: the normal daemon denies development sessions
systemctl --user stop shilpo-shell.service
shilpo daemon --developer-mode

# Terminal 2
shilpo ext dev my-extension
```

Developer mode allows local extension code to be loaded only for the lifetime of that daemon process. It is not a
configuration setting and is disabled again on the next normal daemon start. Because it expands the session-bus
control plane, enable it only while actively developing, then stop the foreground daemon and restart
`shilpo-shell.service`.

---

## 7. Packaging for Distribution

Package your extension into a signed or unsigned `.shilpo-ext` archive:

```bash
shilpo ext pack my-extension --output-dir dist/
```

---

## Next Steps

- Explore the [Manifest Reference](manifest-reference.md) for all contribution options.
- Read [Testing Guide](testing-guide.md) to write hermetic unit tests with `FakeHost`.
- Review [Security & Capabilities](security-and-capabilities.md) for least-privilege guidelines.
