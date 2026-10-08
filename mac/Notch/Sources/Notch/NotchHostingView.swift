import AppKit
import SwiftUI

/// The panel's content view: it holds the hosting view and nothing else.
///
/// It exists so that SwiftUI is *not* the content view. As the content view an
/// `NSHostingView` reports the SwiftUI content's ideal size to the window, and
/// this notch's root is a `GeometryReader` whose ideal size is 10x10 — which on
/// a wide, shallow panel had AppKit walking the window down to zero height and
/// then throwing in the constraint pass. The panel's size is `NotchGeometry`'s
/// to decide, and a container is what makes that true.
///
/// It answers `hitTest` with whatever its subview says and never with itself.
/// A plain `NSView` would claim every point inside its bounds — and most of
/// this panel is empty space reserved for the tooltip, so that would turn the
/// hole the notch depends on into a wall.
final class NotchContainerView: NSView {
    override func hitTest(_ point: NSPoint) -> NSView? {
        // `point` arrives in the superview's coordinates, and a subview's own
        // `hitTest` expects it in ours.
        let local = convert(point, from: superview)
        guard bounds.contains(local) else { return nil }
        for subview in subviews.reversed() {
            if let hit = subview.hitTest(local) { return hit }
        }
        return nil
    }
}

/// The panel is much larger than the notch — it reserves room for the tooltip —
/// so everything outside the currently visible chrome has to pass clicks
/// through to whatever is underneath.
final class NotchHostingView<Content: View>: NSHostingView<Content> {
    /// Regions that should receive events, in view coordinates. NSHostingView
    /// is flipped, so these use a top-left origin.
    var interactiveRects: [CGRect] = []

    override func hitTest(_ point: NSPoint) -> NSView? {
        let local = convert(point, from: superview)
        guard interactiveRects.contains(where: { $0.contains(local) }) else { return nil }
        return super.hitTest(point)
    }

    // MARK: - File drops (Pulse fork: the Send cell)

    /// Where a file drag is accepted, in view coordinates (top-left origin):
    /// the Send cell and nothing else. Nil when there is no such cell.
    var dropRect: CGRect?
    /// The drag is over the cell, or left it.
    var onDropTargeted: ((Bool) -> Void)?
    /// Files dropped on the cell; returns whether they were taken.
    var onDropFiles: (([URL]) -> Bool)?
    private var dropIsOver = false

    private func dropURLs(_ sender: NSDraggingInfo) -> [URL] {
        let objects = sender.draggingPasteboard.readObjects(
            forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true])
        return (objects as? [URL]) ?? []
    }

    private func dropOperation(_ sender: NSDraggingInfo) -> NSDragOperation {
        let point = convert(sender.draggingLocation, from: nil)
        let inside = dropRect?.contains(point) == true && !dropURLs(sender).isEmpty
        if inside != dropIsOver {
            dropIsOver = inside
            onDropTargeted?(inside)
        }
        return inside ? .copy : []
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation { dropOperation(sender) }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation { dropOperation(sender) }

    override func draggingExited(_ sender: NSDraggingInfo?) {
        if dropIsOver {
            dropIsOver = false
            onDropTargeted?(false)
        }
    }

    override func prepareForDragOperation(_ sender: NSDraggingInfo) -> Bool {
        dropOperation(sender) != []
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let accepted = dropOperation(sender) != []
        let urls = dropURLs(sender)
        dropIsOver = false
        onDropTargeted?(false)
        return accepted ? (onDropFiles?(urls) ?? false) : false
    }

    /// `NSView.menu` is not inherited by subviews, and the hit test lands on one
    /// of SwiftUI's, so AppKit would otherwise ask a view that has no menu.
    override func menu(for event: NSEvent) -> NSMenu? {
        let local = convert(event.locationInWindow, from: nil)
        guard interactiveRects.contains(where: { $0.contains(local) }) else { return nil }
        return menu
    }
}
