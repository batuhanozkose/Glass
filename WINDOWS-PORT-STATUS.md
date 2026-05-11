# Glass Windows Port — Status

## Current State (May 2025)

### ✅ Fixed Issues

| Issue | Fix | Commit |
|-------|-----|--------|
| E0659/E0603: `util` import ambiguity in GPUI | Use `::util::ResultExt` in 5 files | gpui@9d8f6ea |
| E0283: D3D11CreateDevice adapter type inference | Use `None::<&IDXGIAdapter>` | Glass@d040fab |
| Missing SF Symbol icon mappings (doc.text, terminal) | Added Segoe Fluent mappings | gpui@7b20615 |
| RenderState missing on Linux | Added `Option<()>` stub | Glass@1809788 |
| windows crate unconditional dep in browser | Moved to target-gated section | Glass@9212820 |
| Zed.exe references throughout Windows components | Rebranded to Glass.exe | Multiple commits |
| No runtime smoke test in CI | Added smoke test step | Glass@57f6b8b |
| URI scheme zed:// | Rebranded to glass:// | Glass@3ee86d3 |
| No Windows dev run script | Added run-glass.ps1 | Glass@3db5f48 |

### ✅ Already Correct (Codex Audit False Positives)

| Audit Finding | Status |
|--------------|--------|
| #1 GPUI D3D renderer API shapes | Already correct for windows 0.61 |
| #2 BrowserEvent::FrameReady cross-platform | Match arm unconditional, emit gated |
| #3 current_frame() non-macOS path | Properly cfg-gated |
| #4 Windows browser ignores layout bounds | Uses surface() + ObjectFit::Fill |
| #5 GPUI draw_surfaces() bypass | Uses normal render pipeline |
| #15-16 macOS deps leaking | Correctly target-gated |
| #18 artifact_mode doesn't gate CEF | Already gated properly |
| #19 CEF staging hardcoded x86_64 | Uses workflow input parameter |
| #20 Artifact validation incomplete | Has strict validation |
| #22 Icon fallback incomplete | All used symbols are mapped |

### ⚠️ Known Limitations (Not Blockers)

| Item | Impact | Priority |
|------|--------|----------|
| Per-frame GPU staging texture allocation | Performance | Low |
| BGRA format assumed without validation | Edge case | Low |
| Hybrid GPU D3D11 shared texture | Rare hardware config | Low |
| CEF_PATH trust gate restrictive in dev | DX only | Low |
| Nightly upload script still references Zed | Needs infra changes | Medium |

### 📋 CI Pipeline Status

- **Workflow**: `.github/workflows/build-windows.yml`
- **Runner**: `windows-2022`
- **Timeout**: 360 min
- **Modes**: `full_with_cef` (browser+editor), `editor_only` (no CEF)
- **Arch**: `x86_64`, `aarch64`
- **Steps**: Build → Stage CEF → Bundle MSVC → Validate DLLs → Smoke Test → Upload

### 🔗 Key Repositories

- **Glass** (main app): `batuhanozkose/Glass` (fork of zed-industries/zed)
- **GPUI** (UI framework): `batuhanozkose/gpui` (fork of Glass-HQ/gpui)
- **GPUI pinned rev**: `7b206150ca8bb7291f8894055b68c3461aa98f05`
