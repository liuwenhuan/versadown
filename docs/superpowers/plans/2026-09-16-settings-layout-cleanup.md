# Settings Layout Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorganize the settings tabs, remove user-facing magnet metadata controls and descriptive helper copy, and align the network save actions with their final input rows.

**Architecture:** Keep the existing settings state and persistence contracts intact while changing only `SettingsPanel` composition and its CSS grid layout. Preserve hidden magnet metadata values in `AppSettings`, move the existing privacy form without changing its submit behavior, and verify the user-visible structure with the existing component tests.

**Tech Stack:** React 18, TypeScript, CSS, Vitest, Testing Library, Tauri build tooling

**Spec:** User-approved settings-page requirements in the current task.

## Global Constraints

- Rename the Display tab to Common.
- Move privacy download settings to Advanced without changing password behavior.
- Remove the magnet metadata service UI while preserving the internal settings contract.
- Remove all setting helper descriptions except the privacy download explanation.
- Place proxy credentials and their save button on the final row.
- Place BT seed ratio and the aria2 save button on the final row.
- Run only targeted SettingsPanel tests and the required builds; do not run full suites.

---

### Task 1: Protect the new settings structure with component tests

**Files:**
- Modify: `src/components/SettingsPanel.test.tsx`

**Interfaces:**
- Consumes: `SettingsPanel` roles, labels, forms, and existing `onChange` callbacks.
- Produces: Regression coverage for tab labels, moved privacy controls, removed metadata UI and inline save rows.

- [x] **Step 1: Write the failing tests**

Add assertions that the first tab is named `常用`, privacy controls appear only after selecting `高级`, magnet metadata controls are absent, helper copy is absent except for the privacy explanation, and the two save buttons share their final fieldset row with the requested inputs.

- [x] **Step 2: Run the tests to verify they fail**

Run:

```powershell
node node_modules/vitest/vitest.mjs run src/components/SettingsPanel.test.tsx --exclude '.worktrees/**' --exclude '.pnpm-store/**' --exclude '.tmp-*/**'
```

Expected: failures for the old `显示` label, privacy controls on the old tab, visible magnet metadata controls, helper copy, and detached action rows.

### Task 2: Recompose the settings panel and align final action rows

**Files:**
- Modify: `src/components/SettingsPanel.tsx`
- Modify: `src/styles.css`

**Interfaces:**
- Consumes: existing `savePrivacyDownload`, `saveProxy`, `saveAria2`, `currentSettings`, and `AppSettings.magnetMetadata` state.
- Produces: the approved settings layout without changing persistence payloads.

- [x] **Step 1: Implement the minimal component changes**

Change the tab label to `常用`; move the existing privacy `<form>` into the Advanced branch; remove the magnet metadata `<form>` and its now-unused local save handler/status; remove all descriptive `<small>` elements except the privacy explanation; place the proxy submit button after the password field in the proxy fieldset; place the aria2 submit button after BT seed ratio in the aria2 fieldset.

- [x] **Step 2: Implement the minimal CSS changes**

Add specific grid classes so the proxy credentials and proxy save button occupy the last row, and so BT share ratio and the aria2 save button occupy the last row. Keep buttons at the existing 38px control height and keep status messages below each fieldset.

- [x] **Step 3: Run the targeted tests**

Run the SettingsPanel Vitest command from Task 1 and require zero failures.

- [x] **Step 4: Run frontend build verification**

Run:

```powershell
pnpm run build
```

Expected: TypeScript and Vite build complete successfully.

- [x] **Step 5: Commit and build the Windows binary**

After `git diff --check`, commit all approved current changes on `master`, then run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1
```

Expected: release binary and artifact copy are generated successfully.
