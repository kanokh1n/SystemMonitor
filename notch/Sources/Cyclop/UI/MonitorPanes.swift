import SwiftUI

// Вкладки System Monitor: Система, Диски, Устройства, Docker, Серверы.
// Данные — из `MonitorFeed`, то есть из того же Rust-кода, что у окна на Tauri.
// В панель 620×208 идёт то, на что смотрят мельком; анализ и очистка диска и
// полный список процессов остаются в окне, оно открывается кнопкой отсюда.

// MARK: - Система

struct SystemPane: View {
    @ObservedObject var feed: MonitorFeed

    var body: some View {
        MonitorFrame(feed: feed) {
            if let s = feed.system {
                VStack(spacing: 6) {
                    HStack(spacing: 6) {
                        Tile(
                            title: localized("CPU"),
                            value: MonitorFormat.percent(s.cpu.usage),
                            detail: String(format: "%.1f · %.1f · %.1f", s.cpu.loadAvg[safe: 0] ?? 0, s.cpu.loadAvg[safe: 1] ?? 0, s.cpu.loadAvg[safe: 2] ?? 0)
                        ) {
                            Sparkline(values: feed.cpuHistory, ceiling: 100, color: .green)
                        }
                        Tile(
                            title: localized("Memory"),
                            value: MonitorFormat.bytes(s.memory.used),
                            detail: memoryDetail(s.memory)
                        ) {
                            MeterBar(value: memoryPressure(s.memory), color: pressureColor(s.memory))
                                .frame(maxHeight: .infinity, alignment: .bottom)
                        }
                        Tile(
                            title: localized("Network"),
                            value: "↓ " + MonitorFormat.rate(s.network.rxPerSec),
                            detail: "↑ " + MonitorFormat.rate(s.network.txPerSec)
                        ) {
                            Sparkline(values: feed.rxHistory, ceiling: nil, color: .cyan)
                        }
                    }
                    .frame(height: 68)

                    HStack(alignment: .top, spacing: 6) {
                        VStack(spacing: 2) {
                            ForEach(s.processes.prefix(3)) { p in
                                HStack(spacing: 6) {
                                    Text(p.title)
                                        .font(.system(size: 10.5))
                                        .foregroundStyle(Theme.secondary)
                                        .lineLimit(1)
                                    Spacer(minLength: 4)
                                    Text(MonitorFormat.bytes(p.memory))
                                        .font(.system(size: 10).monospacedDigit())
                                        .foregroundStyle(Theme.tertiary)
                                    Text(MonitorFormat.percent(p.cpu))
                                        .font(.system(size: 10.5, weight: .medium).monospacedDigit())
                                        .foregroundStyle(.white.opacity(0.8))
                                        .frame(width: 36, alignment: .trailing)
                                }
                                .frame(height: 17)
                            }
                        }
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Theme.surface))

                        VStack(alignment: .trailing, spacing: 4) {
                            if let battery = s.battery, let percent = battery.percent {
                                Label(batteryText(battery, percent: percent), systemImage: batterySymbol(battery, percent: percent))
                                    .font(.system(size: 10.5).monospacedDigit())
                                    .foregroundStyle(Theme.secondary)
                            }
                            Text(localized("Up %@", MonitorFormat.duration(s.system.uptimeSecs)))
                                .font(.system(size: 10))
                                .foregroundStyle(Theme.tertiary)
                            Spacer(minLength: 0)
                            OpenWindowButton(feed: feed, title: localized("Processes"))
                        }
                        .frame(width: 118, alignment: .trailing)
                    }
                }
                .padding(.top, 2)
            } else {
                Waiting()
            }
        }
    }

    private func memoryPressure(_ m: SystemSnapshot.Memory) -> Double {
        if let p = m.breakdown?.pressure { return Double(p) / 100 }
        return MonitorFormat.ratio(m.used, of: m.total)
    }

    private func pressureColor(_ m: SystemSnapshot.Memory) -> Color {
        switch m.breakdown?.pressureLevel {
        case "critical": return .red
        case "warning": return .orange
        default: return .green
        }
    }

    private func memoryDetail(_ m: SystemSnapshot.Memory) -> String {
        var text = localized("of %@", MonitorFormat.bytes(m.total))
        if m.swapUsed > 0 {
            text += " · " + localized("swap %@", MonitorFormat.bytes(m.swapUsed))
        }
        return text
    }

    private func batteryText(_ b: SystemSnapshot.Battery, percent: Double) -> String {
        var text = MonitorFormat.percent(percent)
        if b.state == "charging", let m = b.minutesToFull, m > 0 {
            text += " · " + MonitorFormat.duration(UInt64(m) * 60)
        } else if b.state == "discharging", let m = b.minutesToEmpty, m > 0 {
            text += " · " + MonitorFormat.duration(UInt64(m) * 60)
        }
        return text
    }

    private func batterySymbol(_ b: SystemSnapshot.Battery, percent: Double) -> String {
        if b.state == "charging" || b.state == "full" { return "battery.100.bolt" }
        switch percent {
        case ..<13: return "battery.0"
        case ..<38: return "battery.25"
        case ..<63: return "battery.50"
        case ..<88: return "battery.75"
        default: return "battery.100"
        }
    }
}

// MARK: - Диски

struct DisksPane: View {
    @ObservedObject var feed: MonitorFeed

    var body: some View {
        MonitorFrame(feed: feed) {
            if let s = feed.system {
                VStack(spacing: 6) {
                    ScrollView(showsIndicators: false) {
                        VStack(spacing: 4) {
                            ForEach(s.disks) { disk in
                                VStack(alignment: .leading, spacing: 5) {
                                    HStack(spacing: 7) {
                                        Image(systemName: disk.removable ? "externaldrive.fill" : "internaldrive.fill")
                                            .font(.system(size: 11))
                                            .foregroundStyle(Theme.tertiary)
                                        Text(disk.mountPoint == "/" ? localized("Macintosh HD") : disk.name)
                                            .font(.system(size: 11.5, weight: .medium))
                                            .foregroundStyle(.white)
                                            .lineLimit(1)
                                        Spacer(minLength: 6)
                                        Text(localized("%@ free of %@", MonitorFormat.bytes(disk.available), MonitorFormat.bytes(disk.total)))
                                            .font(.system(size: 10.5).monospacedDigit())
                                            .foregroundStyle(Theme.secondary)
                                    }
                                    let used = MonitorFormat.ratio(disk.used, of: disk.total)
                                    MeterBar(value: used, color: used > 0.9 ? .red : used > 0.75 ? .orange : .blue)
                                }
                                .padding(.horizontal, 10)
                                .padding(.vertical, 8)
                                .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Theme.surface))
                            }
                        }
                    }
                    HStack(spacing: 10) {
                        Label(MonitorFormat.rate(s.diskIo.readPerSec), systemImage: "arrow.down.doc")
                        Label(MonitorFormat.rate(s.diskIo.writePerSec), systemImage: "arrow.up.doc")
                        Spacer()
                        OpenWindowButton(feed: feed, title: localized("Analyze & Clean"))
                    }
                    .font(.system(size: 10.5).monospacedDigit())
                    .foregroundStyle(Theme.secondary)
                }
                .padding(.top, 2)
            } else {
                Waiting()
            }
        }
    }
}

// MARK: - Устройства

struct DevicesPane: View {
    @ObservedObject var feed: MonitorFeed

    var body: some View {
        MonitorFrame(feed: feed) {
            if let devices = feed.devices {
                if devices.isEmpty {
                    EmptyNote(symbol: "headphones", text: localized("No Bluetooth devices with a battery"))
                } else {
                    ScrollView(showsIndicators: false) {
                        VStack(spacing: 3) {
                            ForEach(devices) { device in
                                DeviceRow(device: device)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                }
            } else {
                Waiting()
            }
        }
    }
}

private struct DeviceRow: View {
    let device: BluetoothDevice

    var body: some View {
        HStack(spacing: 9) {
            Image(systemName: symbol)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(device.connected ? Color.white : Theme.tertiary)
                .frame(width: 16)
            VStack(alignment: .leading, spacing: 1) {
                Text(device.name)
                    .font(.system(size: 11.5, weight: .medium))
                    .foregroundStyle(device.connected ? Color.white : Theme.secondary)
                    .lineLimit(1)
                if !device.connected, let seen = device.lastSeen ?? device.levelsAt {
                    Text(MonitorFormat.ago(seen))
                        .font(.system(size: 9.5))
                        .foregroundStyle(Theme.tertiary)
                }
            }
            Spacer(minLength: 6)
            ForEach(device.levels, id: \.self) { level in
                HStack(spacing: 3) {
                    if let part = partLabel(level.part) {
                        Text(part)
                            .font(.system(size: 9, weight: .semibold))
                            .foregroundStyle(Theme.tertiary)
                    }
                    Text("\(level.percent)%")
                        .font(.system(size: 11, weight: .medium).monospacedDigit())
                        .foregroundStyle(levelColor(level.percent))
                }
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 30)
        .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(Theme.surface))
        .opacity(device.connected ? 1 : 0.7)
    }

    private var symbol: String {
        switch device.kind {
        case "headphones": return "headphones"
        case "mouse": return "computermouse.fill"
        case "keyboard": return "keyboard.fill"
        case "trackpad": return "rectangle.and.hand.point.up.left.fill"
        case "gamepad": return "gamecontroller.fill"
        case "speaker": return "hifispeaker.fill"
        case "phone": return "iphone"
        case "watch": return "applewatch"
        default: return "dot.radiowaves.left.and.right"
        }
    }

    private func partLabel(_ part: String) -> String? {
        switch part {
        case "left": return localized("L")
        case "right": return localized("R")
        case "case": return localized("Case")
        default: return nil
        }
    }

    private func levelColor(_ percent: Int) -> Color {
        if !device.live { return Theme.secondary }
        return percent <= 15 ? .red : percent <= 30 ? .orange : .white
    }
}

// MARK: - Docker

struct DockerPane: View {
    @ObservedObject var feed: MonitorFeed

    var body: some View {
        MonitorFrame(feed: feed) {
            if let docker = feed.docker {
                if !docker.available {
                    EmptyNote(symbol: "shippingbox", text: docker.error ?? localized("Docker is not running"))
                } else if docker.containers.isEmpty {
                    EmptyNote(symbol: "shippingbox", text: localized("No containers"))
                } else {
                    ScrollView(showsIndicators: false) {
                        VStack(spacing: 3) {
                            ForEach(docker.containers) { c in
                                ContainerRow(container: c, feed: feed)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                }
            } else {
                Waiting()
            }
        }
    }
}

private struct ContainerRow: View {
    let container: DockerSnapshot.Container
    @ObservedObject var feed: MonitorFeed
    @State private var hovering = false
    /// Нажали «старт» или «стоп» — до следующего снимка показываем, что дело идёт.
    @State private var pending = false

    var body: some View {
        HStack(spacing: 9) {
            StatusDot(color: container.isRunning ? .green : container.state == "paused" ? .orange : Theme.tertiary)
            VStack(alignment: .leading, spacing: 1) {
                Text(container.name)
                    .font(.system(size: 11.5, weight: .medium))
                    .foregroundStyle(.white)
                    .lineLimit(1)
                Text(container.ports.isEmpty ? container.image : container.image + " · " + container.ports.joined(separator: ", "))
                    .font(.system(size: 9.5))
                    .foregroundStyle(Theme.tertiary)
                    .lineLimit(1)
            }
            Spacer(minLength: 6)
            if container.isRunning {
                if let cpu = container.cpuPercent {
                    Text(MonitorFormat.percent(cpu))
                        .font(.system(size: 10.5).monospacedDigit())
                        .foregroundStyle(Theme.secondary)
                }
                if let mem = container.memoryUsed {
                    Text(MonitorFormat.bytes(mem))
                        .font(.system(size: 10.5).monospacedDigit())
                        .foregroundStyle(Theme.tertiary)
                }
            } else {
                Text(container.status)
                    .font(.system(size: 9.5))
                    .foregroundStyle(Theme.tertiary)
                    .lineLimit(1)
            }
            if pending {
                ProgressView().controlSize(.mini).frame(width: 18)
            } else {
                Button {
                    pending = true
                    feed.docker(container.id, container.isRunning ? "stop" : "start")
                } label: {
                    Image(systemName: container.isRunning ? "stop.fill" : "play.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(hovering ? Color.white : Theme.secondary)
                        .frame(width: 18, height: 18)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(localized(container.isRunning ? "Stop" : "Start"))
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 32)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(hovering ? Theme.surfaceHover : Theme.surface)
        )
        .onHover { hovering = $0 }
        .onChange(of: container.state) { pending = false }
        .task(id: pending) {
            // Если Docker так и не ответил, кнопка возвращается через 20 секунд.
            guard pending else { return }
            try? await Task.sleep(for: .seconds(20))
            pending = false
        }
        .animation(Theme.contentAnimation, value: hovering)
    }
}

// MARK: - Серверы

struct ServersPane: View {
    @ObservedObject var feed: MonitorFeed

    var body: some View {
        MonitorFrame(feed: feed) {
            if let servers = feed.servers {
                if servers.isEmpty {
                    VStack(spacing: 10) {
                        EmptyNote(symbol: "server.rack", text: localized("Servers are added in the System Monitor window"))
                        OpenWindowButton(feed: feed, title: localized("Open System Monitor"))
                    }
                } else {
                    ScrollView(showsIndicators: false) {
                        VStack(spacing: 3) {
                            ForEach(servers) { server in
                                ServerRow(server: server)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                }
            } else {
                Waiting()
            }
        }
        .onAppear { feed.refreshServersIfStale() }
    }
}

private struct ServerRow: View {
    let server: RemoteServer

    var body: some View {
        HStack(spacing: 9) {
            if server.busy {
                ProgressView().controlSize(.mini).frame(width: 7)
            } else {
                StatusDot(color: statusColor)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(server.name)
                    .font(.system(size: 11.5, weight: .medium))
                    .foregroundStyle(.white)
                    .lineLimit(1)
                Text(subtitle)
                    .font(.system(size: 9.5))
                    .foregroundStyle(server.status == "online" ? Theme.tertiary : statusColor.opacity(0.85))
                    .lineLimit(1)
            }
            Spacer(minLength: 6)
            if server.status == "online", let stats = server.stats {
                metric(localized("CPU"), stats.cpuPercent.map(MonitorFormat.percent) ?? "—")
                metric(localized("RAM"), MonitorFormat.percent(100 * (1 - MonitorFormat.ratio(stats.memory.available, of: stats.memory.total))))
                if let root = stats.disks.first(where: { $0.mountPoint == "/" }) ?? stats.disks.first {
                    metric(localized("Disk"), MonitorFormat.percent(100 * MonitorFormat.ratio(root.used, of: root.total)))
                }
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 32)
        .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(Theme.surface))
    }

    private var statusColor: Color {
        switch server.status {
        case "online": return .green
        case "down": return .red
        case "error": return .orange
        default: return Theme.tertiary
        }
    }

    private var subtitle: String {
        switch server.status {
        case "online":
            var text = server.host
            if let ms = server.latencyMs { text += " · \(ms) " + localized("ms") }
            if let up = server.stats?.uptimeSecs { text += " · " + localized("up %@", MonitorFormat.duration(up)) }
            return text
        case "checking":
            return localized("Checking…")
        case "down":
            if let since = server.downSince { return localized("Not responding · %@", MonitorFormat.ago(since)) }
            return server.error ?? localized("Not responding")
        default:
            return server.error ?? server.host
        }
    }

    private func metric(_ title: String, _ value: String) -> some View {
        VStack(alignment: .trailing, spacing: 0) {
            Text(value)
                .font(.system(size: 10.5, weight: .medium).monospacedDigit())
                .foregroundStyle(.white.opacity(0.85))
            Text(title)
                .font(.system(size: 8.5, weight: .semibold))
                .foregroundStyle(Theme.tertiary)
        }
        .frame(minWidth: 30, alignment: .trailing)
    }
}

// MARK: - Заголовок и рельс

/// Цифры справа в заголовке панели. Наблюдает за потоком сам, как `NotesCounter`:
/// модель панели не пересылает его обновления, чтобы снимок раз в секунду
/// не перерисовывал всю панель.
struct MonitorHeader: View {
    let tab: NotchViewModel.Tab
    @ObservedObject var feed: MonitorFeed

    var body: some View {
        Group {
            switch tab {
            case .system:
                if let s = feed.system {
                    Text("CPU \(MonitorFormat.percent(s.cpu.usage)) · RAM \(MonitorFormat.percent(100 * MonitorFormat.ratio(s.memory.used, of: s.memory.total)))")
                }
            case .disks:
                if let root = feed.system?.disks.first(where: { $0.mountPoint == "/" }) {
                    Text(localized("%@ free", MonitorFormat.bytes(root.available)))
                }
            case .devices:
                if let n = feed.devices?.filter(\.connected).count, n > 0 {
                    Text("\(n)")
                }
            case .docker:
                if let n = feed.docker?.containers.filter(\.isRunning).count, feed.docker?.available == true {
                    Text(localized("%d running", n))
                }
            case .servers:
                if feed.serversDown > 0 {
                    Text(localized("%d down", feed.serversDown)).foregroundStyle(.red)
                } else if let n = feed.servers?.count, n > 0 {
                    Text("\(n)")
                }
            default:
                EmptyView()
            }
        }
        .font(.system(size: 10, weight: .medium).monospacedDigit())
        .foregroundStyle(Theme.tertiary)
    }
}

/// Красная точка на иконке «Серверы», когда какой-то сервер лежит.
struct ServersAlertDot: View {
    @ObservedObject var feed: MonitorFeed

    var body: some View {
        if feed.serversDown > 0 {
            Circle()
                .fill(Color.red)
                .frame(width: 6, height: 6)
                .offset(x: 9, y: -6)
        }
    }
}

// MARK: - Общие части

/// Рамка вкладки: сообщение об ошибке поверх и заглушка, если помощника нет.
private struct MonitorFrame<Content: View>: View {
    @ObservedObject var feed: MonitorFeed
    @ViewBuilder var content: Content

    var body: some View {
        ZStack(alignment: .bottom) {
            if feed.helperMissing {
                EmptyNote(symbol: "exclamationmark.triangle", text: localized("sysmon-helper is missing from the app bundle. Rebuild with Scripts/bundle.sh."))
            } else {
                content
            }
            if let message = feed.message {
                Text(message)
                    .font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(.white)
                    .lineLimit(2)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(Capsule().fill(Color.red.opacity(0.85)))
                    .transition(.opacity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .animation(Theme.contentAnimation, value: feed.message)
    }
}

private struct Tile<Chart: View>: View {
    let title: String
    let value: String
    let detail: String
    @ViewBuilder var chart: Chart

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title.uppercased())
                .font(.system(size: 8.5, weight: .semibold))
                .tracking(0.6)
                .foregroundStyle(Theme.tertiary)
            Text(value)
                .font(.system(size: 15, weight: .semibold).monospacedDigit())
                .foregroundStyle(.white)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
            Text(detail)
                .font(.system(size: 9.5).monospacedDigit())
                .foregroundStyle(Theme.tertiary)
                .lineLimit(1)
            chart
                .frame(maxHeight: .infinity)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Theme.surface))
    }
}

/// Линия последних замеров. `ceiling` — верх шкалы; без него шкала по максимуму.
private struct Sparkline: View {
    let values: [Double]
    let ceiling: Double?
    let color: Color

    var body: some View {
        GeometryReader { geo in
            let top = max(ceiling ?? (values.max() ?? 1), 1)
            let step = geo.size.width / CGFloat(59)
            let offset = CGFloat(60 - values.count) * step
            Path { path in
                for (i, v) in values.enumerated() {
                    let point = CGPoint(
                        x: offset + CGFloat(i) * step,
                        y: geo.size.height * (1 - CGFloat(min(v, top) / top))
                    )
                    if i == 0 { path.move(to: point) } else { path.addLine(to: point) }
                }
            }
            .stroke(color.opacity(0.85), style: StrokeStyle(lineWidth: 1.2, lineJoin: .round))
        }
    }
}

private struct MeterBar: View {
    let value: Double
    let color: Color

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.surfaceHover)
                Capsule()
                    .fill(color.opacity(0.85))
                    .frame(width: geo.size.width * CGFloat(max(0, min(1, value))))
            }
        }
        .frame(height: 4)
    }
}

private struct StatusDot: View {
    let color: Color

    var body: some View {
        Circle().fill(color).frame(width: 7, height: 7)
    }
}

private struct OpenWindowButton: View {
    @ObservedObject var feed: MonitorFeed
    let title: String

    var body: some View {
        Button {
            feed.openWindow()
        } label: {
            HStack(spacing: 4) {
                Text(title)
                Image(systemName: "arrow.up.forward.app")
            }
            .font(.system(size: 10.5, weight: .medium))
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(Capsule().fill(Theme.surfaceHover))
        }
        .buttonStyle(.plain)
    }
}

private struct EmptyNote: View {
    let symbol: String
    let text: String

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: symbol)
                .font(.system(size: 20, weight: .light))
            Text(text)
                .font(.system(size: 11))
                .multilineTextAlignment(.center)
                .lineLimit(3)
        }
        .foregroundStyle(Theme.tertiary)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct Waiting: View {
    var body: some View {
        ProgressView()
            .controlSize(.small)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
