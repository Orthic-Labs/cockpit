// cockpit-probe --pid N: prints exactly one JSON sample line for one same-user process.
// Passive and read-only: no task_for_pid, no entitlements, no signals, no process control.
//
// Darwin names, verified against the macOS SDK headers (usr/include under
// /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk):
//   libproc.h       proc_pidinfo(int pid, int flavor, uint64_t arg, void *buffer, int buffersize)
//                   proc_pid_rusage(int pid, int flavor, rusage_info_t *buffer)
//   sys/proc_info.h PROC_PIDTBSDINFO (3), struct proc_bsdinfo { pbi_start_tvsec, pbi_start_tvusec, ... }
//                   (PROC_PIDLISTFDS lists file descriptors only; it does not cover Mach ports)
//   sys/resource.h  RUSAGE_INFO_V4 (4), struct rusage_info_v4 { ri_user_time, ri_system_time,
//                   ri_resident_size, ri_phys_footprint, ... }
//   mach/mach_traps.h / mach/mach_port.h  mach_timebase_info, mach_port_names (own task only)
//
// Output: one JSON object, keys sorted. status is one of ok, vanished, pid_reused, unavailable.
// Exit: 0 ok; 1 vanished/unavailable; 2 usage; 3 pid_reused.
// Start time is read before and after the rusage read; a difference prints pid_reused and no metrics.
// rusage_info_v4 ri_user_time/ri_system_time are already nanoseconds.
import Darwin
import Foundation

func emit(_ object: [String: Any]) {
    var doc = object
    doc["schema_version"] = 1
    doc["kind"] = "cockpit.probe.sample"
    guard let data = try? JSONSerialization.data(withJSONObject: doc, options: [.sortedKeys]),
          let text = String(data: data, encoding: .utf8) else {
        FileHandle.standardError.write(Data("cockpit-probe: cannot encode output\n".utf8))
        exit(1)
    }
    print(text)
}

func usage() -> Never {
    FileHandle.standardError.write(Data("usage: cockpit-probe --pid N\n".utf8))
    exit(2)
}

let args = CommandLine.arguments
guard args.count == 3, args[1] == "--pid", let pidValue = Int32(args[2]), pidValue > 0 else { usage() }
let pid = pidValue

enum Failure: Error {
    case vanished
    case unavailable(String)
}

func startTime() throws -> (sec: UInt64, usec: UInt64) {
    var info = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.size)
    let got = proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size)
    if got <= 0 {
        if errno == ESRCH { throw Failure.vanished }
        throw Failure.unavailable("proc_pidinfo_errno_\(errno)")
    }
    if got != size { throw Failure.unavailable("proc_pidinfo_short_read") }
    return (info.pbi_start_tvsec, info.pbi_start_tvusec)
}

func readRusage() throws -> rusage_info_v4 {
    var info = rusage_info_v4()
    let rc = withUnsafeMutablePointer(to: &info) { ptr in
        ptr.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) { proc_pid_rusage(pid, RUSAGE_INFO_V4, $0) }
    }
    if rc != 0 {
        if errno == ESRCH { throw Failure.vanished }
        throw Failure.unavailable("proc_pid_rusage_errno_\(errno)")
    }
    return info
}

/// Own-task Mach port count; other tasks would need task_for_pid, which this probe never uses.
func ownMachPortCount() -> Int? {
    var names: mach_port_name_array_t? = nil
    var namesCount: mach_msg_type_number_t = 0
    var types: mach_port_type_array_t? = nil
    var typesCount: mach_msg_type_number_t = 0
    guard mach_port_names(mach_task_self_, &names, &namesCount, &types, &typesCount) == KERN_SUCCESS else { return nil }
    let count = Int(namesCount)
    if let names = names {
        _ = vm_deallocate(mach_task_self_, vm_address_t(UInt(bitPattern: names)), vm_size_t(count * MemoryLayout<mach_port_name_t>.stride))
    }
    if let types = types {
        _ = vm_deallocate(mach_task_self_, vm_address_t(UInt(bitPattern: types)), vm_size_t(Int(typesCount) * MemoryLayout<mach_port_type_t>.stride))
    }
    return count
}

do {
    let before = try startTime()
    let ru = try readRusage()
    let after = try startTime()
    func start(_ t: (sec: UInt64, usec: UInt64)) -> [String: Any] { ["sec": t.sec, "usec": t.usec] }
    if before.sec != after.sec || before.usec != after.usec {
        emit(["status": "pid_reused", "pid": pid, "start_time": start(before), "start_time_after": start(after)])
        exit(3)
    }
    var ports: [String: Any] = ["available": false, "reason": "task_for_pid_not_allowed"]
    if pid == getpid(), let count = ownMachPortCount() {
        ports = ["available": true, "count": count]
    }
    emit([
        "status": "ok",
        "pid": pid,
        "start_time": start(before),
        "physical_footprint_bytes": ru.ri_phys_footprint,
        "resident_bytes": ru.ri_resident_size,
        "user_cpu_ns": ru.ri_user_time,
        "system_cpu_ns": ru.ri_system_time,
        "mach_ports": ports
    ])
} catch Failure.vanished {
    emit(["status": "vanished", "pid": pid])
    exit(1)
} catch Failure.unavailable(let reason) {
    emit(["status": "unavailable", "pid": pid, "reason": reason])
    exit(1)
} catch {
    emit(["status": "unavailable", "pid": pid, "reason": "unexpected_error"])
    exit(1)
}
