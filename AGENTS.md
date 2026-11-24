# AGENTS.md — Inscriptio

Inscriptio is a native Rust 2024 Win32 tray app that trims tracking parameters from copied links on Windows 10 1703+ x64.

## Commands

- Toolchain: Rust 1.88+ with `x86_64-pc-windows-msvc` and Visual Studio Build Tools with the Windows SDK resource compiler.
- Run settings from the repo root: `cargo run --locked -- --settings`.
- Run all tests: `cargo test --locked`.
- Run one URL test: `cargo test --locked rules::tests::youtube -- --exact`.
- Build the executable: `cargo build --release --locked` produces `target\release\inscriptio.exe`.
- Build the installer from PowerShell: `& .\installer\build.ps1` uses GloriaSetup and verifies the versioned setup executable in `dist/` with a silent dry run.

## Architecture

- `src/main.rs` owns the single-instance mutex, hidden Win32 window, clipboard listener, and message loop. A shortcut or second `--settings` launch signals the running instance.
- `src/clipboard.rs` handles Unicode clipboard reads and writes, privacy markers, self-write detection, and Undo.
- `src/url.rs` parses and reassembles URLs. `src/rules.rs` owns the pure site rewrites, sensitive-link detection, and rewrite tests.
- `src/tray.rs` owns the notification icon and menu. `src/ui.rs` owns the settings controls and DPI layout, `src/paint.rs` handles GDI drawing, and `src/sound.rs` synthesizes the chime.
- `src/settings.rs` stores settings and stats in `HKCU\Software\Inscriptio` and manages the HKCU Run value. `src/history.rs` stores up to 50 cleaned links in `%LOCALAPPDATA%\Inscriptio\history.tsv`.
- `build.rs` compiles `app.rc`, which embeds `app.manifest` and `Inscriptio.ico`. The linker receives `/MANIFEST:NO` to avoid a duplicate manifest.
- **Version Manifests:** Sync `Cargo.toml`, the `inscriptio` entry in `Cargo.lock`, `gloria.toml`, all file/product version fields in `app.rc`, and the Inscriptio assembly identity in `app.manifest`. Update the version badge in `README.md` with them.

## Personal Rules (Brain)

- **Commit Format:** Write one plain sentence stating what changed and why. Omit conventional-commit prefixes.
- **Commit & Push Policy:** Run `git commit`, `git tag`, or `git push` only when explicitly asked. Never force-push to `main`. Push after the build and tests pass.
- **Path Privacy:** Use `C:\Users\you` for user paths in documentation, comments, and examples.
- **Scrollbars:** Use thin themed scrollbars with transparent tracks and zero layout shift.
- **Interactive Controls:** Custom-style form controls with explicit hover, active, keyboard-focus, and disabled states and 120–180 ms transitions.
- **Git Branching:** Work directly on `main`. Create a branch or worktree only when explicitly told to.
- **Versioning (Minimal Changes):** For runtime or packaging changes, increment the alpha number across all version manifests at most once per uncommitted change set. Check `git diff HEAD` first and skip docs-only or read-only work.
- **Versioning (Major/Minor Changes):** Ask for the exact target before changing the major or minor version or dropping `-alpha` for breaking changes or major features.

## Playbooks (Read Before Task)

- **Windows Packaging:** Read `.brain/building-the-project.md` before packaging, compiling release binaries, or building an installer. Follow the Inscriptio packaging guardrail below.
- **GitHub Actions & Releases:** Read `.brain/github-actions.md` before changing workflows or release automation. Follow the Inscriptio release guardrail below.

## Repo Guardrails & Gotchas

- **Clipboard Privacy:** Check monitor-exclusion formats before reading and again after acquiring the clipboard lock. Preserve history/cloud markers on rewrite and Undo. Keep private and sensitive links out of history.
- **Clipboard Sequence:** Record the sequence while the clipboard is still locked. Ignore a self-write only when the sequence is nonzero and matches.
- **Window Lifecycle:** Keep `InscriptioWnd` as a hidden top-level window so it receives Explorer's `TaskbarCreated` broadcast. The class routes settings requests from a second launch.
- **Window Chrome:** Preserve the settings window's native Win32 caption and minimize/close controls.
- **Desktop Shell Lock:** Keep the settings window fixed-size in device-independent pixels and recalculate its layout on `WM_DPICHANGED`.
- **Settings Accessibility:** Preserve dialog keyboard navigation and the history list's `LBS_HASSTRINGS` style for screen readers and type-ahead.
- **Packaging:** Use the per-user GloriaSetup manifest in `gloria.toml`. Keep `id = "Inscriptio"` and declare the history folder for migration from Inno Setup.
- **Release Workflow:** Dispatch `.github/workflows/release.yml` on main. The shared GloriaSetup jobs validate the version, verify the installer, and publish the matching tag and checksums. Use the dry-run input for verification without publication.

## Done Criteria

- For runtime changes, run `cargo test --locked` and `cargo build --release --locked`. For a new site rule, include an input/output regression case in `src/rules.rs`.
- For clipboard or tray changes, smoke-test a tracked URL, Undo, Pause / Resume, and opening settings while a tray instance is running.
- For packaging changes, run `& .\installer\build.ps1` and check the versioned output in `dist/`. For version changes, confirm the version manifests and README badge agree.

<!-- brain-sync: 4401790b0df9 -->
