"""Bounded, in-process telemetry and a passive Qt raster backdrop."""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field
from enum import StrEnum
from math import ceil, isfinite, sin
from random import Random
from threading import Lock
from time import monotonic

from PySide6.QtCore import QEvent, QSize, Qt, QTimer, Signal
from PySide6.QtGui import QColor, QFontDatabase, QFontMetrics, QPainter, QPen, QPixmap
from PySide6.QtWidgets import QWidget


class TelemetryKind(StrEnum):
    LOCAL = "LOCAL"
    CLOUD = "CLOUD"
    MESH = "MESH"
    ALERT = "ALERT"
    WORKTREE = "WORKTREE"
    AST = "AST"
    USAGE = "USAGE"


class BackdropMode(StrEnum):
    TELEMETRY_RAIN = "Telemetry rain"
    STATIC_GLASS = "Static glass"
    STEALTH_SOLID = "Stealth solid"


class TelemetryFilter(StrEnum):
    ALL = "All telemetry"
    MESH = "Mesh & handshakes only"
    AI = "AI usage & tokens only"


def _clean(value: str, limit: int) -> str:
    printable = "".join(char if char.isprintable() else " " for char in value[:limit])
    return " ".join(printable.split())


@dataclass(frozen=True, slots=True)
class TelemetryEvent:
    kind: TelemetryKind
    source: str
    action: str
    subject: str = ""
    tokens_per_second: float | None = None
    workers: int | None = None
    active: bool | None = None
    critical: bool = False
    fields: tuple[tuple[str, str | int | float], ...] = ()
    fragment: str = ""

    def line(self, *, include_fragments: bool = False) -> str:
        parts = [f"[{self.kind.value}::{_clean(self.action, 32)}]", _clean(self.subject, 90)]
        if self.tokens_per_second is not None and isfinite(self.tokens_per_second) and self.tokens_per_second >= 0:
            parts.append(f"| {self.tokens_per_second:.1f} tok/s")
        if self.workers is not None:
            parts.append(f"| workers={max(0, self.workers)}")
        for key, value in self.fields[:8]:
            parts.append(f"| {_clean(key, 28)}={_clean(str(value), 52)}")
        if include_fragments and self.fragment:
            parts.append(f"| {_clean(self.fragment, 120)}")
        return " ".join(part for part in parts if part)[:320]


@dataclass(frozen=True, slots=True)
class _Sample:
    kind: TelemetryKind
    source: str
    text: str
    glyphs: bytes
    time: float
    rate: float | None
    workers: int | None
    active: bool | None
    critical: bool


class TelemetryStreamBuffer:
    """Thread-safe ingestion. Producers never schedule GUI events per token."""

    def __init__(self, *, include_fragments: bool = False):
        self._entries: deque[_Sample] = deque(maxlen=300)
        self._lock = Lock()
        self._revision = 0
        self._include_fragments = include_fragments
        self._collecting = True

    def set_collecting(self, enabled: bool) -> None:
        with self._lock:
            self._collecting = enabled

    def publish(self, event: TelemetryEvent) -> None:
        with self._lock:
            if not self._collecting:
                return
        line = event.line(include_fragments=self._include_fragments)
        rate = event.tokens_per_second
        if rate is not None:
            rate = max(0.0, min(rate, 1000.0)) if isfinite(rate) else None
        glyphs = bytes(value - 32 for value in line.encode("ascii", "replace"))
        with self._lock:
            if not self._collecting:
                return
            self._entries.append(_Sample(
                event.kind, _clean(event.source, 64), line, glyphs, monotonic(),
                rate, event.workers, event.active, event.critical,
            ))
            self._revision += 1

    def snapshot(self, since: int = -1) -> tuple[int, tuple[_Sample, ...] | None]:
        with self._lock:
            if self._revision == since:
                return self._revision, None
            return self._revision, tuple(self._entries)


_PALETTES = (
    ("#00F0FF", QColor(0, 240, 255, 56)),
    ("#A78BFA", QColor(138, 92, 246, 56)),
    ("#00FF9D", QColor(0, 230, 140, 51)),
    ("#FFAA00", QColor(255, 170, 0, 89)),
    ("#FF4155", QColor(255, 65, 85, 102)),
    ("#A1BBCF", QColor(161, 187, 207, 42)),
)
_KIND_PALETTE = {
    TelemetryKind.LOCAL: 0, TelemetryKind.CLOUD: 1, TelemetryKind.MESH: 2,
    TelemetryKind.ALERT: 3, TelemetryKind.WORKTREE: 5, TelemetryKind.AST: 5,
    TelemetryKind.USAGE: 5,
}
_AI_KINDS = frozenset((TelemetryKind.LOCAL, TelemetryKind.CLOUD, TelemetryKind.USAGE))
_IDLE_GLYPHS = b"\x0b\x0e\x0d\x0e\x0a"
_TAIL = 7


@dataclass(slots=True)
class _Column:
    x: int
    y: float
    speed: float
    phase: int
    glyphs: bytes = field(default=_IDLE_GLYPHS)
    palette: int = 5
    positions: list[int] = field(default_factory=lambda: [0] * _TAIL)
    symbols: list[int] = field(default_factory=lambda: [0] * _TAIL)


class CodeRainBackdrop(QWidget):
    """GUI-thread widget. Any worker may call its buffer's publish method."""

    modeChanged = Signal(object)
    telemetryChanged = Signal(str)

    def __init__(self, parent: QWidget, stream: TelemetryStreamBuffer | None = None):
        super().__init__(parent)
        self.stream = stream if stream is not None else TelemetryStreamBuffer()
        self.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        self.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)
        self.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self.mode = BackdropMode.TELEMETRY_RAIN
        self.filter = TelemetryFilter.ALL
        self._fps = 30
        self._opacity = 0.22
        self._sensitivity = 1.0
        self._base = QColor("#080B10")
        self._native_tint = False
        self._last_tick = 0.0
        self._window_handle = None
        self._top_level = None
        self._random = Random(0xC051)
        self._painter = QPainter()
        self._font = QFontDatabase.systemFont(QFontDatabase.SystemFont.FixedFont)
        self._font.setPixelSize(13)
        self._metrics = QFontMetrics(self._font)
        self._cell = QSize(self._metrics.horizontalAdvance("W") + 2, self._metrics.height() + 3)
        self._grid_pen = QPen(QColor(104, 150, 165, 26))
        self._atlas = ()
        self._atlas_ratio = 0.0
        self._static = QPixmap()
        self._columns: list[_Column] = []
        self._visible_count = 0
        self._active = False
        self._pulse = 1.0
        self._rows = range(_TAIL - 1, -1, -1)
        self._revision = -1
        self._selected: tuple[_Sample, ...] = ()
        self._stream_cache: tuple[_Sample, ...] = ()
        self._latest_workers: tuple[_Sample, ...] = ()
        self._hud_text = ""
        self._timer = QTimer(self)
        self._timer.setTimerType(Qt.TimerType.PreciseTimer)
        self._timer.timeout.connect(self._tick)
        self._visibility_check = QTimer(self)
        self._visibility_check.setSingleShot(True)
        self._visibility_check.timeout.connect(self._reconcile_timer)

    def set_mode(self, mode: BackdropMode) -> None:
        mode = BackdropMode(mode)
        if self.mode == mode:
            return
        self.mode = mode
        if mode is BackdropMode.STATIC_GLASS:
            self._paint_static_cache()
        elif mode is BackdropMode.TELEMETRY_RAIN:
            self._layout_columns()
        self._reconcile_timer()
        self.update()
        self.modeChanged.emit(mode)

    def cycle_mode(self) -> None:
        modes = tuple(BackdropMode)
        self.set_mode(modes[(modes.index(self.mode) + 1) % len(modes)])

    def set_filter(self, telemetry_filter: TelemetryFilter) -> None:
        self.filter = TelemetryFilter(telemetry_filter)
        self._select_streams()
        if self.mode is BackdropMode.STATIC_GLASS:
            self._paint_static_cache()
        if self.mode is not BackdropMode.STEALTH_SOLID:
            self.update()

    def set_fps(self, fps: int) -> None:
        if fps not in (15, 30, 60):
            raise ValueError("Choose 15, 30, or 60 FPS.")
        self._fps = fps
        if self._timer.isActive():
            self._timer.start(ceil(1000 / fps))

    def set_opacity(self, percentage: int) -> None:
        self._opacity = min(35, max(5, percentage)) / 100
        if self.mode is BackdropMode.STATIC_GLASS:
            self._paint_static_cache()
        if self.mode is not BackdropMode.STEALTH_SOLID:
            self.update()

    def set_velocity_sensitivity(self, sensitivity: float) -> None:
        if not isfinite(sensitivity):
            raise ValueError("Velocity sensitivity must be finite.")
        self._sensitivity = min(2.0, max(0.0, sensitivity))

    def set_native_tint(self, enabled: bool) -> None:
        """Use only after the window shell establishes a native backdrop."""
        self._native_tint = enabled
        if self.mode is BackdropMode.STATIC_GLASS:
            self._paint_static_cache()
        self.update()

    def _select_streams(self) -> None:
        if self.filter is TelemetryFilter.MESH:
            self._selected = tuple(s for s in self._stream_cache if s.kind is TelemetryKind.MESH)
        elif self.filter is TelemetryFilter.AI:
            self._selected = tuple(s for s in self._stream_cache if s.kind in _AI_KINDS)
        else:
            self._selected = self._stream_cache
        latest: dict[tuple[TelemetryKind, str], _Sample] = {}
        for sample in reversed(self._selected):
            if sample.kind in (TelemetryKind.LOCAL, TelemetryKind.CLOUD) and (
                sample.active is not None or sample.rate is not None or sample.workers is not None
            ):
                latest.setdefault((sample.kind, sample.source), sample)
        self._latest_workers = tuple(latest.values())

    def _tick(self) -> None:
        now = monotonic()
        dt = min(0.08, now - self._last_tick)
        self._last_tick = now
        revision, snapshot = self.stream.snapshot(self._revision)
        if snapshot is not None:
            self._revision = revision
            self._stream_cache = snapshot
            self._select_streams()

        rate = 0.0
        workers = 0
        active = False
        for sample in self._latest_workers:
            if now - sample.time < 8 and sample.active is not False:
                active = active or bool(sample.active or sample.rate or sample.workers)
                rate += sample.rate or 0.0
                workers += max(0, sample.workers or 0)
        recent = self._selected[-1] if self._selected else None
        is_recent = recent is not None and now - recent.time < 8
        self._active = active or (
            is_recent and (recent.active is True or recent.kind is TelemetryKind.ALERT)
        )
        index = -1
        for sample in reversed(self._selected):
            if now - sample.time > 8:
                break
            index += 1
        count = len(self._columns)
        self._visible_count = min(count, max(1, count // 5 + (min(12, workers * 2 + int(rate / 7)) if self._active else 0)))
        speed = 18 + (16 if self._active else 0) + min(150, rate * 0.55 + workers * 6) * self._sensitivity
        height = self.height()
        cell_height = self._cell.height()
        for number in range(self._visible_count):
            column = self._columns[number]
            if index >= 0:
                sample = self._selected[-1 - number % (index + 1)]
                column.glyphs = sample.glyphs
                column.palette = 4 if sample.kind is TelemetryKind.ALERT and sample.critical else _KIND_PALETTE[sample.kind]
            else:
                column.glyphs = _IDLE_GLYPHS
                column.palette = 5
            column.y += speed * column.speed * dt
            if column.y > height + _TAIL * cell_height:
                column.y = -column.phase % (6 * cell_height) - 6 * cell_height
                column.phase += 7
            row = int(column.y // cell_height)
            for tail in self._rows:
                column.positions[tail] = (row - tail) * cell_height
                column.symbols[tail] = column.glyphs[(row + column.phase - tail) % len(column.glyphs)]
        self._pulse = 0.77 + 0.23 * sin(now * 2.5)
        hud = recent.text if is_recent else ""
        if hud != self._hud_text:
            self._hud_text = hud
            self.telemetryChanged.emit(hud)
        self.update()

    def _build_atlas(self) -> None:
        ratio = self.devicePixelRatioF()
        if ratio == self._atlas_ratio:
            return
        self._atlas_ratio = ratio
        width, height = self._cell.width(), self._cell.height()
        palette_atlases = []
        for lead, trail in _PALETTES:
            variants = []
            for ink in (QColor(lead), trail):
                glyphs = []
                for code in range(32, 127):
                    sprite = QPixmap(ceil(width * ratio), ceil(height * ratio))
                    sprite.setDevicePixelRatio(ratio)
                    sprite.fill(Qt.GlobalColor.transparent)
                    painter = QPainter(sprite)
                    painter.setFont(self._font)
                    painter.setPen(QPen(ink))
                    painter.drawText(1, self._metrics.ascent() + 1, chr(code))
                    painter.end()
                    glyphs.append(sprite)
                variants.append(tuple(glyphs))
            palette_atlases.append(tuple(variants))
        self._atlas = tuple(palette_atlases)

    def _paint_static_cache(self) -> None:
        ratio = self.devicePixelRatioF()
        self._static = QPixmap(ceil(24 * ratio), ceil(24 * ratio))
        self._static.setDevicePixelRatio(ratio)
        self._static.fill(Qt.GlobalColor.transparent)
        painter = QPainter(self._static)
        painter.setPen(self._grid_pen)
        painter.setOpacity(self._opacity)
        painter.drawLine(0, 0, 0, 23)
        painter.drawLine(0, 0, 23, 0)
        painter.end()

    def paintEvent(self, event) -> None:
        painter = self._painter
        if not painter.begin(self):
            return
        try:
            painter.setCompositionMode(QPainter.CompositionMode.CompositionMode_Source)
            rect = event.rect()
            tinted = self._native_tint and self.mode is not BackdropMode.STEALTH_SOLID
            painter.fillRect(rect, self._tint if tinted else self._base)
            painter.setCompositionMode(QPainter.CompositionMode.CompositionMode_SourceOver)
            if self.mode is BackdropMode.STATIC_GLASS and not self._static.isNull():
                painter.drawTiledPixmap(rect.x(), rect.y(), rect.width(), rect.height(), self._static,
                                         rect.x() % 24, rect.y() % 24)
                return
            if self.mode is not BackdropMode.TELEMETRY_RAIN or not self._atlas:
                return
            opacity = self._opacity if self._active else min(0.13, self._opacity)
            for index in range(self._visible_count):
                column = self._columns[index]
                head, trail = self._atlas[column.palette]
                painter.setOpacity(opacity * self._pulse if column.palette in (3, 4) else opacity)
                for depth in self._rows:
                    painter.drawPixmap(column.x, column.positions[depth], (trail if depth else head)[column.symbols[depth]])
        finally:
            painter.end()

    _tint = QColor(8, 11, 16, 177)

    def resizeEvent(self, event) -> None:
        if self.mode is BackdropMode.TELEMETRY_RAIN:
            self._layout_columns()
        super().resizeEvent(event)

    def _layout_columns(self) -> None:
        count = min(120, self.width() // (self._cell.width() * 2) + 1)
        width = self.width()
        self._columns = [
            _Column(int((i + 0.5) * width / count), self._random.uniform(0, self.height()),
                    self._random.uniform(0.65, 1.28), self._random.randrange(512))
            for i in range(count)
        ]
        self._visible_count = 0

    def showEvent(self, event) -> None:
        super().showEvent(event)
        top = self.window()
        if top is not self._top_level:
            if self._top_level is not None:
                self._top_level.removeEventFilter(self)
            self._top_level = top
            top.installEventFilter(self)
        self._visibility_check.start(0)

    def hideEvent(self, event) -> None:
        self._timer.stop()
        self._visibility_check.stop()
        super().hideEvent(event)

    def eventFilter(self, watched, event) -> bool:
        if event.type() in (QEvent.Type.Expose, QEvent.Type.WindowStateChange, QEvent.Type.Show,
                            QEvent.Type.Hide, QEvent.Type.PlatformSurface, QEvent.Type.DevicePixelRatioChange):
            if event.type() in (QEvent.Type.Hide, QEvent.Type.WindowStateChange):
                self._timer.stop()
            self._visibility_check.start(0)
        return False

    def _reconcile_timer(self) -> None:
        top = self.window()
        handle = top.windowHandle()
        if handle is not self._window_handle:
            if self._window_handle is not None:
                self._window_handle.removeEventFilter(self)
            self._window_handle = handle
            if handle is not None:
                handle.installEventFilter(self)
        if self.mode is BackdropMode.STATIC_GLASS and self.isVisible():
            if self._static.isNull() or self._static.devicePixelRatio() != self.devicePixelRatioF():
                self._paint_static_cache()
                self.update()
        if self.mode is not BackdropMode.TELEMETRY_RAIN or not self.isVisible() or top.isMinimized() or (
            handle is not None and not handle.isExposed()
        ):
            self._timer.stop()
            return
        self._build_atlas()
        if not self._timer.isActive():
            self._last_tick = monotonic()
            self._timer.start(ceil(1000 / self._fps))
