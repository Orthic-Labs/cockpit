import os

/// An agent app has no window to print into, so anything worth diagnosing has
/// to go somewhere you can read it:
///
///     log stream --predicate 'subsystem == "dev.orthic.pulse"' --level debug
enum Log {
    static let usage = Logger(subsystem: "dev.orthic.pulse", category: "usage")
    static let sessions = Logger(subsystem: "dev.orthic.pulse", category: "sessions")
}
