import Foundation
import CoreGraphics

/// Pure M0 rules. AppKit/Accessibility integration supplies the observations.
public enum VisibilityGeometry {
    /// Valid means finite, non-null and with positive area.
    private static func normalized(_ rect: CGRect) -> CGRect? {
        guard !rect.isNull, !rect.isInfinite else { return nil }
        let r = rect.standardized
        guard r.width > 0, r.height > 0, r.minX.isFinite, r.minY.isFinite,
              r.width.isFinite, r.height.isFinite else { return nil }
        return r
    }

    /// Positive-area overlap only; edge-touching neighbours do not intersect.
    private static func overlaps(_ a: CGRect, _ b: CGRect) -> Bool {
        guard let a = normalized(a), let b = normalized(b) else { return false }
        let i = a.intersection(b)
        return !i.isNull && i.width > 0 && i.height > 0
    }

    public static func covers(_ window: CGRect, monitor: CGRect) -> Bool {
        guard let window = normalized(window), let monitor = normalized(monitor) else { return false }
        return window.minX <= monitor.minX && window.minY <= monitor.minY &&
            window.maxX >= monitor.maxX && window.maxY >= monitor.maxY
    }

    public static func isLikelyBorderless(_ title: String, window: CGRect, monitor: CGRect) -> Bool {
        title.isEmpty && covers(window, monitor: monitor)
    }

    public static func shouldHide(
        accessibilityTrusted: Bool,
        axFullscreen: Bool?,
        focusedWindow: CGRect?,
        geometryFallback: Bool,
        monitor: CGRect
    ) -> Bool {
        guard accessibilityTrusted else { return false }
        if let focusedWindow, overlaps(focusedWindow, monitor), let axFullscreen {
            return axFullscreen
        }
        return geometryFallback
    }
}
