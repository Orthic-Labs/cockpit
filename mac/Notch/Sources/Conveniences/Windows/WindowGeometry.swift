import CoreGraphics

/// Pulse: where a window goes for each action. Pure arithmetic in the
/// Accessibility coordinate space (origin at the top-left of the primary
/// display), against the visible frame of the screen the window is on, so the
/// menu bar and the Dock are never covered. Results are whole points.
enum WindowGeometry {
    /// Distance that Larger and Smaller add or remove on each side.
    static let resizeStep: CGFloat = 30
    /// Nudges move by this share of the visible frame's size.
    static let nudgeShare: CGFloat = 0.05
    /// Almost maximize leaves this share of the visible frame, centred.
    static let almostShare: CGFloat = 0.95

    /// The frame for an action that works on the current window alone.
    /// Nil for restore and the display moves, which need memory or other screens.
    static func target(for action: WindowAction, current c: CGRect, visible v: CGRect) -> CGRect? {
        let w = v.width, h = v.height
        switch action {
        case .leftHalf: return whole(v.minX, v.minY, w / 2, h)
        case .rightHalf: return whole(v.minX + w / 2, v.minY, w / 2, h)
        case .topHalf: return whole(v.minX, v.minY, w, h / 2)
        case .bottomHalf: return whole(v.minX, v.minY + h / 2, w, h / 2)
        case .centerHalf: return whole(v.minX + w / 4, v.minY, w / 2, h)
        case .topLeft: return whole(v.minX, v.minY, w / 2, h / 2)
        case .topRight: return whole(v.minX + w / 2, v.minY, w / 2, h / 2)
        case .bottomLeft: return whole(v.minX, v.minY + h / 2, w / 2, h / 2)
        case .bottomRight: return whole(v.minX + w / 2, v.minY + h / 2, w / 2, h / 2)
        case .firstThird: return whole(v.minX, v.minY, w / 3, h)
        case .centerThird: return whole(v.minX + w / 3, v.minY, w / 3, h)
        case .lastThird: return whole(v.minX + 2 * w / 3, v.minY, w / 3, h)
        case .firstTwoThirds: return whole(v.minX, v.minY, 2 * w / 3, h)
        case .centerTwoThirds: return whole(v.minX + w / 6, v.minY, 2 * w / 3, h)
        case .lastTwoThirds: return whole(v.minX + w / 3, v.minY, 2 * w / 3, h)
        case .firstFourth: return whole(v.minX, v.minY, w / 4, h)
        case .secondFourth: return whole(v.minX + w / 4, v.minY, w / 4, h)
        case .thirdFourth: return whole(v.minX + w / 2, v.minY, w / 4, h)
        case .lastFourth: return whole(v.minX + 3 * w / 4, v.minY, w / 4, h)
        case .firstThreeFourths: return whole(v.minX, v.minY, 3 * w / 4, h)
        case .lastThreeFourths: return whole(v.minX + w / 4, v.minY, 3 * w / 4, h)
        case .maximize: return v.integral
        case .almostMaximize:
            let pw = w * almostShare, ph = h * almostShare
            return whole(v.midX - pw / 2, v.midY - ph / 2, pw, ph)
        case .maximizeHeight:
            return clamp(whole(c.minX, v.minY, c.width, h), in: v)
        case .center:
            let cw = min(c.width, w), ch = min(c.height, h)
            return whole(v.midX - cw / 2, v.midY - ch / 2, cw, ch)
        case .larger: return grown(c, by: resizeStep, in: v)
        case .smaller: return grown(c, by: -resizeStep, in: v)
        case .nudgeLeft: return nudged(c, dx: -w * nudgeShare, dy: 0, in: v)
        case .nudgeRight: return nudged(c, dx: w * nudgeShare, dy: 0, in: v)
        case .nudgeUp: return nudged(c, dx: 0, dy: -h * nudgeShare, in: v)
        case .nudgeDown: return nudged(c, dx: 0, dy: h * nudgeShare, in: v)
        case .restore, .nextDisplay, .previousDisplay: return nil
        }
    }

    /// The same place on another screen: the window keeps its share of the
    /// visible frame, scaled to the new one and kept inside it.
    static func moved(_ c: CGRect, from source: CGRect, to destination: CGRect) -> CGRect {
        let scaleX = destination.width / source.width
        let scaleY = destination.height / source.height
        let width = min(c.width * scaleX, destination.width)
        let height = min(c.height * scaleY, destination.height)
        let x = destination.minX + (c.minX - source.minX) * scaleX
        let y = destination.minY + (c.minY - source.minY) * scaleY
        return clamp(whole(x, y, width, height), in: destination)
    }

    // MARK: - Helpers

    private static func whole(_ x: CGFloat, _ y: CGFloat, _ width: CGFloat, _ height: CGFloat) -> CGRect {
        CGRect(x: x.rounded(), y: y.rounded(), width: width.rounded(), height: height.rounded())
    }

    /// Grows (or shrinks) around the window's centre, never past the visible
    /// frame and never below a usable minimum.
    private static func grown(_ c: CGRect, by delta: CGFloat, in v: CGRect) -> CGRect {
        let minWidth = min(200, v.width), minHeight = min(150, v.height)
        let width = min(max(c.width + 2 * delta, minWidth), v.width)
        let height = min(max(c.height + 2 * delta, minHeight), v.height)
        return clamp(whole(c.midX - width / 2, c.midY - height / 2, width, height), in: v)
    }

    private static func nudged(_ c: CGRect, dx: CGFloat, dy: CGFloat, in v: CGRect) -> CGRect {
        clamp(whole(c.minX + dx, c.minY + dy, c.width, c.height), in: v)
    }

    /// Slides a rectangle back inside the bounds, and shrinks it if it is larger.
    private static func clamp(_ r: CGRect, in v: CGRect) -> CGRect {
        let width = min(r.width, v.width), height = min(r.height, v.height)
        let x = min(max(r.minX, v.minX), v.maxX - width)
        let y = min(max(r.minY, v.minY), v.maxY - height)
        return CGRect(x: x.rounded(), y: y.rounded(), width: width.rounded(), height: height.rounded())
    }
}
