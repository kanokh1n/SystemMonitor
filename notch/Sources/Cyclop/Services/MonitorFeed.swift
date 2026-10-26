import AppKit

// Вкладки System Monitor берут данные у `sysmon-helper` — Rust-процесса из
// `crates/sysmon-core` в корне репозитория. Тот же код собирает цифры для окна
// System Monitor на Tauri, так что панель и окно показывают одно и то же.
//
// Устроено как `NowPlayingFeed`: дочерний процесс, строки JSON в обе стороны.
// Помощник опрашивает только то, что ему велено смотреть, а когда смотреть
// нечего, его нет вовсе: процесс завершается и не стоит ничего.

// MARK: - Что приходит от помощника

struct SystemSnapshot: Decodable {
    struct Cpu: Decodable {
        var usage: Double
        var perCore: [Double]
        var cores: Int
        var loadAvg: [Double]
    }

    struct Breakdown: Decodable {
        var app: UInt64
        var wired: UInt64
        var compressed: UInt64
        var cached: UInt64
        var pressure: Int?
        var pressureLevel: String?
    }

    struct Memory: Decodable {
        var total: UInt64
        var used: UInt64
        var available: UInt64
        var swapTotal: UInt64
        var swapUsed: UInt64
        var breakdown: Breakdown?
    }

    struct Disk: Decodable, Identifiable {
        var name: String
        var mountPoint: String
        var total: UInt64
        var available: UInt64
        var removable: Bool
        var id: String { mountPoint }
        var used: UInt64 { total > available ? total - available : 0 }
    }

    struct Io: Decodable {
        var readPerSec: UInt64
        var writePerSec: UInt64
    }

    struct Network: Decodable {
        var rxPerSec: UInt64
        var txPerSec: UInt64
    }

    struct Battery: Decodable {
        var percent: Double?
        var state: String
        var minutesToEmpty: Int?
        var minutesToFull: Int?
    }

    struct Proc: Decodable, Identifiable {
        var pid: Int
        var name: String
        var app: String?
        var cpu: Double
        var memory: UInt64
        var id: Int { pid }
        var title: String { app ?? name }
    }

    struct Info: Decodable {
        var hostName: String
        var uptimeSecs: UInt64
    }

    var cpu: Cpu
    var memory: Memory
    var disks: [Disk]
    var diskIo: Io
    var network: Network
    var battery: Battery?
    var processes: [Proc]
    var system: Info
}

struct BluetoothDevice: Decodable, Identifiable {
    struct Level: Decodable, Hashable {
        var part: String
        var percent: Int
    }

    var id: String
    var name: String
    var kind: String
    var connected: Bool
    var levels: [Level]
    var live: Bool
    var levelsAt: UInt64?
    var lastSeen: UInt64?
}

struct DockerSnapshot: Decodable {
    struct Container: Decodable, Identifiable {
        var id: String
        var name: String
        var image: String
        var state: String
        var status: String
        var ports: [String]
        var cpuPercent: Double?
        var memoryUsed: UInt64?
        var memoryLimit: UInt64?
        var isRunning: Bool { state == "running" }
    }

    var available: Bool
    var error: String?
    var containers: [Container]
}

struct RemoteServer: Decodable, Identifiable {
    struct Memory: Decodable {
        var total: UInt64
        var available: UInt64
    }

    struct Disk: Decodable {
        var mountPoint: String
        var total: UInt64
        var used: UInt64
    }

    struct Stats: Decodable {
        var uptimeSecs: UInt64
        var cpuPercent: Double?
        var memory: Memory
        var disks: [Disk]
    }

    var id: String
    var name: String
    var host: String
    /// `checking`, `online`, `down`, `error` или `noNetwork`.
    var status: String
    var error: String?
    var stats: Stats?
    var latencyMs: UInt64?
    var lastOk: UInt64?
    var downSince: UInt64?
    var busy: Bool
}

// MARK: - Помощник

@MainActor
final class MonitorFeed: ObservableObject {
    /// Что сейчас опрашивать. Решает модель: тема включена, пока её вкладку
    /// смотрят в открытой панели, а серверы — всё время, пока их вкладка на рельсе,
    /// чтобы упавший сервер было видно и с закрытой панелью.
    struct Watch: Equatable {
        var system = false
        var devices = false
        var docker = false
        var servers = false

        var isEmpty: Bool { !system && !devices && !docker && !servers }
    }

    @Published private(set) var system: SystemSnapshot?
    /// Последние 60 замеров CPU и сети — на маленькие графики.
    @Published private(set) var cpuHistory: [Double] = []
    @Published private(set) var rxHistory: [Double] = []
    @Published private(set) var txHistory: [Double] = []
    @Published private(set) var devices: [BluetoothDevice]?
    @Published private(set) var docker: DockerSnapshot?
    @Published private(set) var servers: [RemoteServer]?
    /// Последняя ошибка действия (контейнер не запустился и т. п.). Сама гаснет.
    @Published private(set) var message: String?
    /// В бандле нет помощника — сборка без Rust.
    @Published private(set) var helperMissing = false

    var serversDown: Int { servers?.filter { $0.status == "down" }.count ?? 0 }

    private static let historyLength = 60

    private var watch = Watch()
    private var process: Process?
    private var input: FileHandle?
    private var buffer = Data()
    private var failures = 0
    private var messageTask: Task<Void, Never>?
    private var lastServersUpdate: Date?

    private var helperURL: URL? {
        if let path = ProcessInfo.processInfo.environment["SYSMON_HELPER"] {
            return URL(fileURLWithPath: path)
        }
        return Bundle.main.url(forAuxiliaryExecutable: "sysmon-helper")
    }

    // MARK: Управление

    func setWatch(_ next: Watch) {
        let turnedOnServers = next.servers && !watch.servers
        guard next != watch else { return }
        let previous = watch
        watch = next
        // Ушли с вкладки — старые цифры не нужны, а при возвращении график
        // начнётся с текущего момента, без провала на месте паузы.
        if previous.system && !next.system {
            cpuHistory = []
            rxHistory = []
            txHistory = []
        }
        if next.isEmpty {
            terminate()
            return
        }
        launchIfNeeded()
        send(["cmd": "watch", "system": next.system, "devices": next.devices, "docker": next.docker, "servers": next.servers])
        if turnedOnServers { lastServersUpdate = Date() }
    }

    /// Открыли вкладку серверов: если данные старше 20 секунд, опросить сейчас,
    /// а не ждать фоновой минуты.
    func refreshServersIfStale() {
        guard watch.servers else { return }
        if let last = lastServersUpdate, Date().timeIntervalSince(last) < 20 { return }
        refresh("servers")
    }

    func refresh(_ topic: String) {
        send(["cmd": "refresh", "topic": topic])
    }

    func docker(_ id: String, _ action: String) {
        send(["cmd": "docker", "id": id, "action": action])
    }

    /// Открывает окно System Monitor на Tauri — для того, что в панель не помещается:
    /// анализ и очистка диска, полный список процессов.
    func openWindow() {
        let workspace = NSWorkspace.shared
        // Сначала установленная копия: по одному идентификатору macOS может
        // выбрать отладочную сборку из папки проекта, если она зарегистрирована.
        let installed = [
            FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Applications/System Monitor.app"),
            URL(fileURLWithPath: "/Applications/System Monitor.app"),
        ].first { FileManager.default.fileExists(atPath: $0.path) }
        guard let url = installed ?? workspace.urlForApplication(withBundleIdentifier: "com.kanokhin.systemmonitor") else {
            show(localized("System Monitor is not installed"))
            return
        }
        workspace.openApplication(at: url, configuration: NSWorkspace.OpenConfiguration())
    }

    func stop() {
        watch = Watch()
        terminate()
    }

    // MARK: Процесс

    private func launchIfNeeded() {
        guard process == nil else { return }
        guard let helperURL, FileManager.default.isExecutableFile(atPath: helperURL.path) else {
            helperMissing = true
            return
        }
        helperMissing = false

        let task = Process()
        task.executableURL = helperURL
        let output = Pipe()
        let commands = Pipe()
        task.standardOutput = output
        task.standardInput = commands
        task.standardError = FileHandle.nullDevice

        // `@Sendable` явно — по той же причине, что в `NowPlayingFeed`: зовут с
        // чужой очереди, и унаследованная изоляция главного актора уронила бы процесс.
        output.fileHandleForReading.readabilityHandler = { @Sendable [weak self] handle in
            let chunk = handle.availableData
            guard !chunk.isEmpty else { return }
            Task { @MainActor in self?.consume(chunk) }
        }
        task.terminationHandler = { @Sendable [weak self] finished in
            let pid = finished.processIdentifier
            Task { @MainActor in self?.handleTermination(of: pid) }
        }

        do {
            try task.run()
        } catch {
            NSLog("Cyclop: sysmon-helper failed to launch: \(error.localizedDescription)")
            helperMissing = true
            return
        }
        process = task
        input = commands.fileHandleForWriting
        buffer.removeAll()
    }

    private func terminate() {
        guard let process else { return }
        // Закрытый stdin — сигнал помощнику выйти; terminate — на случай, если он занят.
        try? input?.close()
        input = nil
        process.terminate()
        self.process = nil
    }

    private func handleTermination(of pid: Int32) {
        guard let process, process.processIdentifier == pid else { return }
        self.process = nil
        input = nil
        failures += 1
        guard !watch.isEmpty, failures <= 5 else { return }
        let delay = Double(min(failures, 5)) * 2
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
            guard let self, !self.watch.isEmpty, self.process == nil else { return }
            let current = self.watch
            self.watch = Watch()
            self.setWatch(current)
        }
    }

    private func send(_ object: [String: Any]) {
        guard let input,
              let data = try? JSONSerialization.data(withJSONObject: object),
              let newline = "\n".data(using: .utf8) else { return }
        do {
            try input.write(contentsOf: data + newline)
        } catch {
            NSLog("Cyclop: sysmon-helper write failed: \(error.localizedDescription)")
        }
    }

    // MARK: Разбор

    private struct Kind: Decodable {
        var type: String
        var message: String?
    }

    private struct Envelope<T: Decodable>: Decodable {
        var data: T
    }

    private func consume(_ chunk: Data) {
        buffer.append(chunk)
        while let newline = buffer.firstIndex(of: 0x0A) {
            let line = Data(buffer[buffer.startIndex..<newline])
            buffer = Data(buffer[buffer.index(after: newline)...])
            if !line.isEmpty { handle(line) }
        }
        if buffer.count > 4_000_000 { buffer.removeAll() }
    }

    private func handle(_ line: Data) {
        let decoder = JSONDecoder()
        guard let kind = try? decoder.decode(Kind.self, from: line) else { return }
        failures = 0
        switch kind.type {
        case "system":
            guard watch.system, let s = try? decoder.decode(Envelope<SystemSnapshot>.self, from: line).data else { return }
            system = s
            Self.push(s.cpu.usage, to: &cpuHistory)
            Self.push(Double(s.network.rxPerSec), to: &rxHistory)
            Self.push(Double(s.network.txPerSec), to: &txHistory)
        case "devices":
            devices = try? decoder.decode(Envelope<[BluetoothDevice]>.self, from: line).data
        case "docker":
            docker = try? decoder.decode(Envelope<DockerSnapshot>.self, from: line).data
        case "servers":
            servers = try? decoder.decode(Envelope<[RemoteServer]>.self, from: line).data
            lastServersUpdate = Date()
        case "error":
            if let text = kind.message { show(text) }
        default:
            break
        }
    }

    private static func push(_ value: Double, to history: inout [Double]) {
        history.append(value)
        if history.count > historyLength { history.removeFirst(history.count - historyLength) }
    }

    private func show(_ text: String) {
        message = text
        messageTask?.cancel()
        messageTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(6))
            guard !Task.isCancelled else { return }
            self?.message = nil
        }
    }
}

// MARK: - Форматирование

enum MonitorFormat {
    static func bytes(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .memory)
    }

    static func rate(_ value: UInt64) -> String {
        localized("%@/s", ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .file))
    }

    static func percent(_ value: Double) -> String {
        String(format: "%.0f%%", value)
    }

    static func ratio(_ part: UInt64, of total: UInt64) -> Double {
        total > 0 ? min(1, Double(part) / Double(total)) : 0
    }

    /// «3 д 4 ч», «5 ч 12 мин», «7 мин».
    static func duration(_ seconds: UInt64) -> String {
        let formatter = DateComponentsFormatter()
        formatter.unitsStyle = .abbreviated
        formatter.maximumUnitCount = 2
        formatter.allowedUnits = seconds >= 86_400 ? [.day, .hour] : [.hour, .minute]
        return formatter.string(from: TimeInterval(seconds)) ?? ""
    }

    /// Сколько прошло с отметки Unix: «5 мин назад».
    static func ago(_ unix: UInt64) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter.localizedString(for: Date(timeIntervalSince1970: TimeInterval(unix)), relativeTo: Date())
    }
}
