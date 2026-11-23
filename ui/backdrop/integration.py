"""Qt host and Settings integration. Run with python -m ui.backdrop.integration."""

from __future__ import annotations

import sys
from collections.abc import Callable, Iterable
from pathlib import Path

from PySide6.QtCore import QAbstractNativeEventFilter, QEvent, QSettings, Qt, Signal
from PySide6.QtGui import QKeySequence, QShortcut
from PySide6.QtWidgets import (
    QApplication, QComboBox, QFormLayout, QFrame, QHBoxLayout, QLabel,
    QLineEdit, QMainWindow, QPushButton, QSlider, QStackedWidget, QVBoxLayout, QWidget,
)

from .telemetry_rain import BackdropMode, CodeRainBackdrop, TelemetryFilter, TelemetryStreamBuffer


def enable_native_backdrop(window: QMainWindow) -> bool:
    """Best-effort optional Windows Mica or macOS AppKit material."""
    if QApplication.platformName() == "windows" and sys.getwindowsversion().build >= 22621:
        import ctypes
        from ctypes import wintypes

        dwm = ctypes.WinDLL("dwmapi")
        apply_attribute = dwm.DwmSetWindowAttribute
        apply_attribute.argtypes = (wintypes.HWND, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD)
        apply_attribute.restype = ctypes.c_long
        mica = ctypes.c_int(2)  # DWMSBT_MAINWINDOW
        return apply_attribute(int(window.winId()), 38, ctypes.byref(mica), ctypes.sizeof(mica)) == 0
    if QApplication.platformName() == "cocoa":
        import ctypes

        try:
            import AppKit
            import objc
        except ImportError:
            return False
        if AppKit.NSWorkspace.sharedWorkspace().accessibilityDisplayShouldReduceTransparency():
            return False
        view = objc.objc_object(c_void_p=ctypes.c_void_p(int(window.winId())))
        parent = view.superview()
        if parent is None:
            return False
        blur = AppKit.NSVisualEffectView.alloc().initWithFrame_(view.frame())
        blur.setBlendingMode_(AppKit.NSVisualEffectBlendingModeBehindWindow)
        blur.setMaterial_(AppKit.NSVisualEffectMaterialUnderWindowBackground)
        blur.setAutoresizingMask_(AppKit.NSViewWidthSizable | AppKit.NSViewHeightSizable)
        parent.addSubview_positioned_relativeTo_(blur, AppKit.NSWindowBelow, view)
        return True
    return False


def register_windows_hotkey(callback: Callable[[], None]) -> Callable[[], None] | None:
    """Install Ctrl+Shift+M with the OS. No system keyboard hook is needed."""
    if QApplication.platformName() != "windows":
        return None
    import ctypes
    from ctypes import wintypes

    app = QApplication.instance()
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    hotkey_id = 0x43A7
    register = user32.RegisterHotKey
    register.argtypes = (wintypes.HWND, ctypes.c_int, wintypes.UINT, wintypes.UINT)
    register.restype = wintypes.BOOL
    if not register(None, hotkey_id, 0x4006, ord("M")):  # MOD_NOREPEAT | MOD_CONTROL | MOD_SHIFT
        return None

    class HotkeyFilter(QAbstractNativeEventFilter):
        def nativeEventFilter(self, event_type, message):
            if event_type == b"windows_dispatcher_MSG":
                msg = wintypes.MSG.from_address(int(message))
                if msg.message == 0x0312 and msg.wParam == hotkey_id:  # WM_HOTKEY
                    callback()
                    return True, 0
            return False, 0

    event_filter = HotkeyFilter()
    app.installNativeEventFilter(event_filter)

    def unregister() -> None:
        user32.UnregisterHotKey(None, hotkey_id)
        app.removeNativeEventFilter(event_filter)

    return unregister


class _HoverSidebar(QFrame):
    def __init__(self, parent: QWidget):
        super().__init__(parent)
        self.setObjectName("hudSidebar")
        self.setFixedWidth(82)

    def enterEvent(self, event) -> None:
        self.setFixedWidth(150)
        super().enterEvent(event)

    def leaveEvent(self, event) -> None:
        self.setFixedWidth(82)
        super().leaveEvent(event)


class ConsiliumHudWindow(QMainWindow):
    """Example Qt shell for the existing command center's pages and worker signals."""

    hotkeyReceived = Signal()

    def __init__(
        self,
        stream: TelemetryStreamBuffer,
        telemetry_signals: Iterable = (),
        *,
        dashboard: QWidget | None = None,
        frameless: bool = False,
        native_backdrop: Callable[[QMainWindow], bool] | None = None,
        register_global_hotkey: Callable[[Callable[[], None]], Callable[[], None] | None] | None = None,
    ):
        super().__init__()
        self.setWindowTitle("Consilium · Telemetry HUD")
        self.resize(1060, 680)
        self._settings = QSettings("Consilium", "Qt Telemetry HUD")
        self._native_backdrop = native_backdrop
        self._unregister_hotkey = None
        self._telemetry_signals = tuple(telemetry_signals)
        self._publish = stream.publish

        if frameless:
            self.setWindowFlag(Qt.WindowType.FramelessWindowHint)
        if native_backdrop is not None:
            self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)

        self._shell = QWidget()
        self._shell.setObjectName("hudShell")
        self.setCentralWidget(self._shell)
        self.backdrop = CodeRainBackdrop(self._shell, stream)
        self.backdrop.setGeometry(self._shell.rect())

        foreground = QWidget(self._shell)
        foreground.setObjectName("hudForeground")
        layout = QVBoxLayout(self._shell)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(foreground)
        content = QVBoxLayout(foreground)
        content.setContentsMargins(18, 16, 18, 18)
        content.setSpacing(14)

        top = QHBoxLayout()
        title = QLabel("Consilium")
        title.setProperty("heading", True)
        top.addWidget(title)
        top.addStretch()
        self._chip = QPushButton()
        self._chip.setObjectName("performanceChip")
        self._chip.setToolTip("Cycle backdrop mode (Ctrl+Shift+M)")
        self._chip.clicked.connect(self.backdrop.cycle_mode)
        top.addWidget(self._chip)
        if frameless:
            title.mousePressEvent = self._start_move
            minimize = QPushButton("Minimize")
            minimize.clicked.connect(self.showMinimized)
            top.addWidget(minimize)
            close = QPushButton("Close")
            close.clicked.connect(self.close)
            top.addWidget(close)
        content.addLayout(top)

        body = QHBoxLayout()
        sidebar = _HoverSidebar(foreground)
        side = QVBoxLayout(sidebar)
        side.setContentsMargins(2, 8, 2, 8)
        dash_button = QPushButton("HUD")
        settings_button = QPushButton("Settings")
        side.addWidget(dash_button)
        side.addWidget(settings_button)
        side.addStretch()
        body.addWidget(sidebar)

        self._pages = QStackedWidget()
        self._pages.setObjectName("hudStack")
        dash = dashboard if dashboard is not None else self._dashboard()
        dash.setProperty("hudPage", True)
        self._pages.addWidget(dash)
        self._pages.addWidget(self._settings_page())
        dash_button.clicked.connect(lambda: self._pages.setCurrentIndex(0))
        settings_button.clicked.connect(lambda: self._pages.setCurrentIndex(1))
        body.addWidget(self._pages, 1)
        content.addLayout(body, 1)
        self._shell.installEventFilter(self)

        self._status = QLabel("Waiting for telemetry")
        self._status.setObjectName("telemetryStatus")
        self._status.setAccessibleName("Latest telemetry")
        self._status.setTextFormat(Qt.TextFormat.PlainText)
        self._status.setWordWrap(True)
        content.addWidget(self._status)
        self.backdrop.telemetryChanged.connect(lambda line: self._status.setText(line or "Waiting for telemetry"))
        self.backdrop.modeChanged.connect(self._mode_changed)
        self.setStyleSheet((Path(__file__).resolve().parents[1] / "styles" / "glassmorphism.qss").read_text(encoding="utf-8"))

        self.hotkeyReceived.connect(self.backdrop.cycle_mode, Qt.ConnectionType.QueuedConnection)
        if register_global_hotkey is not None:
            self._unregister_hotkey = register_global_hotkey(self.hotkeyReceived.emit)
        self._shortcut = QShortcut(QKeySequence("Ctrl+Shift+M"), self)
        self._shortcut.setContext(Qt.ShortcutContext.ApplicationShortcut)
        self._shortcut.setEnabled(self._unregister_hotkey is None)
        self._shortcut.setAutoRepeat(False)
        self._shortcut.activated.connect(self.backdrop.cycle_mode)
        self._restore()

        for signal in self._telemetry_signals:
            # Only the locked ring buffer runs on the producer thread.
            signal.connect(self._publish, Qt.ConnectionType.DirectConnection)

    def _start_move(self, event) -> None:
        if event.button() == Qt.MouseButton.LeftButton and self.windowHandle() is not None:
            self.windowHandle().startSystemMove()

    @staticmethod
    def _dashboard() -> QWidget:
        page = QWidget()
        stack = QVBoxLayout(page)
        stack.setContentsMargins(0, 0, 0, 0)
        card = QFrame()
        card.setProperty("glassCard", True)
        text = QLabel("Council workspace")
        text.setProperty("heading", True)
        card_layout = QVBoxLayout(card)
        card_layout.addWidget(text)
        card_layout.addStretch()
        stack.addWidget(card, 1)
        bar = QFrame()
        bar.setObjectName("hudInput")
        bar_layout = QHBoxLayout(bar)
        example = QLineEdit()
        example.setPlaceholderText("Command input preview")
        example.setAccessibleName("Command input preview")
        bar_layout.addWidget(example)
        stack.addWidget(bar)
        return page

    def _settings_page(self) -> QWidget:
        page = QFrame()
        page.setProperty("glassCard", True)
        form = QFormLayout(page)
        form.setContentsMargins(20, 20, 20, 20)
        heading = QLabel("Backdrop")
        heading.setProperty("heading", True)
        form.addRow(heading)
        self._mode = QComboBox()
        for mode in BackdropMode:
            self._mode.addItem(mode.value)
        self._mode.currentTextChanged.connect(self.backdrop.set_mode)
        form.addRow("&Mode", self._mode)
        self._filter = QComboBox()
        for telemetry_filter in TelemetryFilter:
            self._filter.addItem(telemetry_filter.value)
        self._filter.currentTextChanged.connect(lambda value: self._apply("filter", value, self.backdrop.set_filter))
        form.addRow("&Telemetry", self._filter)
        self._opacity = self._slider(form, "Rain &opacity", "opacity", 5, 35, "%", self.backdrop.set_opacity)
        self._velocity = self._slider(form, "&Velocity sensitivity", "velocity", 0, 200, "%",
                                      lambda value: self.backdrop.set_velocity_sensitivity(value / 100))
        self._frame_rate = QComboBox()
        self._frame_rate.addItems(["15", "30", "60"])
        self._frame_rate.currentTextChanged.connect(lambda value: self._apply("fps", int(value), self.backdrop.set_fps))
        form.addRow("&Frame rate (FPS)", self._frame_rate)
        return page

    def _slider(self, form, title, key, minimum, maximum, suffix, setter):
        row = QWidget()
        layout = QHBoxLayout(row)
        layout.setContentsMargins(0, 0, 0, 0)
        slider = QSlider(Qt.Orientation.Horizontal)
        slider.setRange(minimum, maximum)
        slider.setAccessibleName(title.replace("&", ""))
        value_text = QLabel()
        slider.valueChanged.connect(lambda value: value_text.setText(f"{value}{suffix}"))
        slider.valueChanged.connect(lambda value: self._apply(key, value, setter))
        layout.addWidget(slider, 1)
        layout.addWidget(value_text)
        form.addRow(title, row)
        return slider

    def _apply(self, key, value, setter) -> None:
        setter(value)
        self._settings.setValue(key, value)

    def _restore(self) -> None:
        mode = self._settings.value("mode", BackdropMode.TELEMETRY_RAIN.value)
        telemetry_filter = self._settings.value("filter", TelemetryFilter.ALL.value)
        self._mode.setCurrentText(mode if mode in set(BackdropMode) else BackdropMode.TELEMETRY_RAIN.value)
        self._filter.setCurrentText(telemetry_filter if telemetry_filter in set(TelemetryFilter) else TelemetryFilter.ALL.value)
        self._opacity.setValue(self._settings.value("opacity", 22, type=int))
        self._velocity.setValue(self._settings.value("velocity", 100, type=int))
        fps = self._settings.value("fps", 30, type=int)
        self._frame_rate.setCurrentText(str(fps if fps in (15, 30, 60) else 30))
        self._mode_changed(self.backdrop.mode)

    def _mode_changed(self, mode: BackdropMode) -> None:
        self._mode.setCurrentText(mode.value)
        self._settings.setValue("mode", mode.value)
        self.backdrop.stream.set_collecting(mode is not BackdropMode.STEALTH_SOLID)
        self._chip.setText(f"Performance · {mode.value}")
        self._status.setVisible(mode is BackdropMode.TELEMETRY_RAIN)
        self._shell.setProperty("stealth", mode is BackdropMode.STEALTH_SOLID)
        for frame in self._shell.findChildren(QFrame):
            if frame.property("glassCard") or frame.objectName() in ("hudSidebar", "hudInput"):
                frame.style().unpolish(frame)
                frame.style().polish(frame)
                frame.update()

    def eventFilter(self, watched, event) -> bool:
        if watched is self._shell and event.type() == QEvent.Type.Resize:
            self._pin_backdrop()
        return super().eventFilter(watched, event)

    def _pin_backdrop(self) -> None:
        self.backdrop.setGeometry(self._shell.rect())
        self.backdrop.lower()

    def resizeEvent(self, event) -> None:
        super().resizeEvent(event)
        self._pin_backdrop()

    def showEvent(self, event) -> None:
        super().showEvent(event)
        if self._native_backdrop is not None:
            establish = self._native_backdrop
            self._native_backdrop = None
            try:
                self.backdrop.set_native_tint(establish(self))
            except (AttributeError, OSError, RuntimeError, TypeError):
                self.backdrop.set_native_tint(False)

    def closeEvent(self, event) -> None:
        for signal in self._telemetry_signals:
            signal.disconnect(self._publish)
        self._telemetry_signals = ()
        if self._unregister_hotkey is not None:
            self._unregister_hotkey()
            self._unregister_hotkey = None
        super().closeEvent(event)


def main() -> int:
    native = "--native-backdrop" in sys.argv
    app = QApplication([arg for arg in sys.argv if arg != "--native-backdrop"])
    window = ConsiliumHudWindow(
        TelemetryStreamBuffer(), frameless=native,
        native_backdrop=enable_native_backdrop if native else None,
        register_global_hotkey=register_windows_hotkey,
    )
    window.show()
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
