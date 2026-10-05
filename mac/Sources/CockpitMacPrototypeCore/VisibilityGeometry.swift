import Foundation

/// Pure M0 rules. AppKit/Accessibility integration supplies the observations.
public enum VisibilityGeometry {
    public static func covers(_ window: CGRect, monitor: CGRect) -> Bool {
        window.minX <= monitor.minX && window.minY <= monitor.minY &&
            window.maxX >= monitor.maxX && window.maxY >= monitor.maxY
    }

    public static func isLikelyBorderless(_ title: String, window: CGRect, monitor: CGRect) -> Bool {
        title.isEmpty && covers(window, monitor) &&
            window.width >= monitor.width * 0.95 && window.height >= monitor.height * 0.95
    }

    public static func shouldHide(
        accessibilityTrusted: Bool,
        axFullscreen: Bool?,
        focusedWindow: CGRect?,
        geometryFallback: Bool,
        monitor: CGRect
    ) -> Bool {
        guard accessibilityTrusted else { return false }
        if let axFullscreen {
            if axFullscreen { return focusedWindow.map { $0.intersects(monitor) } ?? true }
            return false
        }
        return geometryFallback
    }
}
