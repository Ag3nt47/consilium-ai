# Consilium Qt telemetry backdrop

Run the standalone shell from the workspace root with Python 3.11 or later and
PySide6-Essentials 6.8 or later.

```text
python -m pip install "PySide6-Essentials>=6.8"
python -m ui.backdrop.integration
```

Open **Settings → Backdrop** to choose **Telemetry rain**, **Static glass**, or
**Stealth solid**. The Performance chip cycles the same setting. Settings persist
through QSettings. The opacity control covers 5–35%, and the frame-rate choices
are 15, 30, and 60 FPS. The default is 30 FPS.

## Connect the worker event signals

Use this setup in the Qt host once the worker objects and dashboard exist.
Have each adapter emit a `Signal(object)` carrying a `TelemetryEvent`. Replace
the example worker names with the corresponding objects in the host.

```python
from ui.backdrop.integration import (
    ConsiliumHudWindow, enable_native_backdrop, register_windows_hotkey,
)
from ui.backdrop.telemetry_rain import TelemetryStreamBuffer

stream = TelemetryStreamBuffer()
window = ConsiliumHudWindow(
    stream,
    telemetry_signals=(
        peerlink.telemetry,
        escalation_router.telemetry,
        ollama_runner.telemetry,
        cli_runner.telemetry,
        worktree_manager.telemetry,
        validator.telemetry,
    ),
    dashboard=dashboard,
    register_global_hotkey=register_windows_hotkey,
)
window.show()
```

The window owns the backdrop as a sibling below its foreground and
`QStackedWidget`. Mouse events pass through the backdrop. Supply the actual
dashboard widget in place of the example page and set `glassCard=True` on its
card frames. Use object names `hudSidebar` or `hudInput` for the matching QSS
styles. The example contains a hover sidebar with ordinary keyboard-accessible
navigation buttons.

The direct Qt connection writes only to the locked 300-entry buffer on the
producer thread. The GUI reads bounded snapshots at its render rate. A non-Qt
async worker can call `stream.publish(event)` directly. Emit telemetry at
provider sampling cadence rather than for every token. Include a stable source
identifier per worker, observed tokens per second, worker count, and a final
`active=False` update. Unrefreshed activity decays after eight seconds.

```python
from ui.backdrop.telemetry_rain import TelemetryEvent, TelemetryKind as Kind

# Call from the producers after the corresponding event has actually happened.
ollama_runner.telemetry.emit(TelemetryEvent(
    Kind.LOCAL, "ollama-qwen", "OLLAMA", "qwen3-coder:30b",
    active=True, workers=1, tokens_per_second=48.2,
    fields=(("VRAM_GiB", 18.4),),
))
escalation_router.telemetry.emit(TelemetryEvent(
    Kind.CLOUD, "claude-worker", "ESCALATE", "claude-sonnet-4",
    active=True, fields=(("consensus_vote", 0.92), ("context_tokens", 14200)),
))
peerlink.telemetry.emit(TelemetryEvent(
    Kind.MESH, "mac-node", "SPAKE2", "MBP-M3",
    fields=(("key_exchange", "VERIFIED"), ("cipher", "ChaCha20-Poly1305")),
))
worktree_manager.telemetry.emit(TelemetryEvent(
    Kind.WORKTREE, "swarm-02", "SYNC", "agent-swarm-02",
    fields=(("added", 42), ("removed", 11)),
))
validator.telemetry.emit(TelemetryEvent(
    Kind.ALERT, "ast-worker", "AST_FAILURE", "Invalid syntax", critical=True,
    fields=(("latency_ms", 4),),
))
```

Use the same representation for quota status, verified BLE events, and offline
fallbacks. Emit BLE or protocol labels only when that path or protocol was
actually observed. For example, an Ethernet status update can use
`Kind.MESH` with the action `HEARTBEAT` and a verified adapter or memory
field. A quota update can use `Kind.USAGE` and an exhausted limit can use
`Kind.ALERT`. Raw code and diffs are excluded by default. To show snippets,
pass approved redacted fragments and create the dedicated stream with
`include_fragments=True`. Keep credentials, prompts, and private paths out of
all event fields.

## Native window behavior

To try native material, run the shell with `--native-backdrop` or construct it
with `frameless=True, native_backdrop=enable_native_backdrop`. Windows Mica
requires Windows 11 build 22621 or later. macOS uses an AppKit visual-effect
view when `pyobjc-framework-Cocoa` is installed and Reduce Transparency is off.
The shell uses its solid obsidian base when a native backdrop is unavailable.
[Windows backdrop API](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type) · [AppKit visual effects](https://developer.apple.com/documentation/appkit/nsvisualeffectview).

The default shell registers the global shortcut on Windows. On macOS and
Linux, `Ctrl+Shift+M` works while the application has focus. To make it global,
pass a host shortcut registrar that accepts a callback and returns a cleanup
function, or returns `None` if registration is unavailable. This lets the host
use the OS or desktop portal and avoids a system-wide keyboard monitor.

Rain pauses for hidden or minimized windows and on platforms that report
occlusion through `QWindow.isExposed()`. Static mode draws a cached grid tile
without a periodic timer. Stealth stops animation, skips capture in the
dedicated backdrop buffer, and uses opaque surfaces. Qt cannot reliably report
full occlusion on every compositor. The paint loop reuses its painter,
columns, and raster glyphs, but Python and Qt can still allocate internally.
[Qt window exposure semantics](https://doc.qt.io/qtforpython-6/PySide6/QtGui/QWindow.html#PySide6.QtGui.QWindow.isExposed).

The shipped Consilium desktop currently uses Tauri and React. This package is
the requested Qt component and integration example. Adding it to the shipped
desktop requires a native sidecar or a renderer port.
