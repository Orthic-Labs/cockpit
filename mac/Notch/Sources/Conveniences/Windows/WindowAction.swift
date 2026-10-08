import Foundation

/// Pulse: the window actions, in the order the hub lists them. Each has a
/// stable id (its raw value) that stores shortcut choices, and a default
/// shortcut in the Rectangle style (Control+Option plus a key).
enum WindowAction: String, CaseIterable, Identifiable {
    case leftHalf, rightHalf, topHalf, bottomHalf, centerHalf
    case topLeft, topRight, bottomLeft, bottomRight
    case firstThird, centerThird, lastThird
    case firstTwoThirds, centerTwoThirds, lastTwoThirds
    case firstFourth, secondFourth, thirdFourth, lastFourth
    case firstThreeFourths, lastThreeFourths
    case maximize, almostMaximize, maximizeHeight
    case larger, smaller, center, restore
    case nextDisplay, previousDisplay
    case nudgeLeft, nudgeRight, nudgeUp, nudgeDown

    var id: String { rawValue }

    var title: String {
        switch self {
        case .leftHalf: return "Left half"
        case .rightHalf: return "Right half"
        case .topHalf: return "Top half"
        case .bottomHalf: return "Bottom half"
        case .centerHalf: return "Center half"
        case .topLeft: return "Top left"
        case .topRight: return "Top right"
        case .bottomLeft: return "Bottom left"
        case .bottomRight: return "Bottom right"
        case .firstThird: return "First third"
        case .centerThird: return "Center third"
        case .lastThird: return "Last third"
        case .firstTwoThirds: return "First two thirds"
        case .centerTwoThirds: return "Center two thirds"
        case .lastTwoThirds: return "Last two thirds"
        case .firstFourth: return "First fourth"
        case .secondFourth: return "Second fourth"
        case .thirdFourth: return "Third fourth"
        case .lastFourth: return "Last fourth"
        case .firstThreeFourths: return "First three fourths"
        case .lastThreeFourths: return "Last three fourths"
        case .maximize: return "Maximize"
        case .almostMaximize: return "Almost maximize"
        case .maximizeHeight: return "Maximize height"
        case .larger: return "Larger"
        case .smaller: return "Smaller"
        case .center: return "Center"
        case .restore: return "Restore"
        case .nextDisplay: return "Next display"
        case .previousDisplay: return "Previous display"
        case .nudgeLeft: return "Nudge left"
        case .nudgeRight: return "Nudge right"
        case .nudgeUp: return "Nudge up"
        case .nudgeDown: return "Nudge down"
        }
    }

    var group: WindowActionGroup {
        switch self {
        case .leftHalf, .rightHalf, .topHalf, .bottomHalf, .centerHalf,
             .topLeft, .topRight, .bottomLeft, .bottomRight:
            return .halvesAndQuarters
        case .firstThird, .centerThird, .lastThird, .firstTwoThirds, .centerTwoThirds, .lastTwoThirds:
            return .thirds
        case .firstFourth, .secondFourth, .thirdFourth, .lastFourth, .firstThreeFourths, .lastThreeFourths:
            return .fourths
        case .maximize, .almostMaximize, .maximizeHeight:
            return .maximize
        case .larger, .smaller, .center, .restore:
            return .sizeAndPosition
        case .nextDisplay, .previousDisplay:
            return .displays
        case .nudgeLeft, .nudgeRight, .nudgeUp, .nudgeDown:
            return .nudge
        }
    }

    /// Empty when the action has no default shortcut.
    var defaultShortcut: String {
        switch self {
        case .leftHalf: return "ctrl+opt+left"
        case .rightHalf: return "ctrl+opt+right"
        case .topHalf: return "ctrl+opt+up"
        case .bottomHalf: return "ctrl+opt+down"
        case .centerHalf: return ""
        case .topLeft: return "ctrl+opt+u"
        case .topRight: return "ctrl+opt+i"
        case .bottomLeft: return "ctrl+opt+j"
        case .bottomRight: return "ctrl+opt+k"
        case .firstThird: return "ctrl+opt+d"
        case .centerThird: return "ctrl+opt+f"
        case .lastThird: return "ctrl+opt+g"
        case .firstTwoThirds: return "ctrl+opt+e"
        case .centerTwoThirds: return ""
        case .lastTwoThirds: return "ctrl+opt+t"
        case .firstFourth: return "ctrl+opt+1"
        case .secondFourth: return "ctrl+opt+2"
        case .thirdFourth: return "ctrl+opt+3"
        case .lastFourth: return "ctrl+opt+4"
        case .firstThreeFourths: return ""
        case .lastThreeFourths: return ""
        case .maximize: return "ctrl+opt+return"
        case .almostMaximize: return "ctrl+opt+cmd+return"
        case .maximizeHeight: return "ctrl+opt+shift+return"
        case .larger: return "ctrl+opt+="
        case .smaller: return "ctrl+opt+-"
        case .center: return "ctrl+opt+c"
        case .restore: return "ctrl+opt+delete"
        case .nextDisplay: return "ctrl+opt+cmd+right"
        case .previousDisplay: return "ctrl+opt+cmd+left"
        case .nudgeLeft: return "ctrl+opt+shift+left"
        case .nudgeRight: return "ctrl+opt+shift+right"
        case .nudgeUp: return "ctrl+opt+shift+up"
        case .nudgeDown: return "ctrl+opt+shift+down"
        }
    }
}

/// The hub's grouping of the actions, in display order.
enum WindowActionGroup: String, CaseIterable {
    case halvesAndQuarters, thirds, fourths, maximize, sizeAndPosition, displays, nudge

    var title: String {
        switch self {
        case .halvesAndQuarters: return "Halves and quarters"
        case .thirds: return "Thirds"
        case .fourths: return "Fourths"
        case .maximize: return "Maximize"
        case .sizeAndPosition: return "Size and position"
        case .displays: return "Displays"
        case .nudge: return "Nudge"
        }
    }
}
