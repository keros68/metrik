import Foundation
import OSLog

struct MetrikWidgetSnapshot: Decodable {
    let schemaVersion: Int
    let generatedAt: String
    let totalTokens: Int64
    let agents: [MetrikWidgetAgent]
}

struct MetrikWidgetAgent: Decodable, Identifiable {
    let id: String
    let label: String
    let tokens: Int64
    let bindingWindowKey: String?
    let windows: [MetrikWidgetQuotaWindow]

    var bindingWindow: MetrikWidgetQuotaWindow? {
        if let key = self.bindingWindowKey,
           let matched = self.windows.first(where: { $0.key == key && $0.available && !$0.resetExpired })
        {
            return matched
        }
        let live = self.windows.filter { $0.available && !$0.resetExpired }
        guard let shortest = live.first else { return nil }
        let low = live.filter { $0.remainingPercent <= 15 }
        return low.min { $0.remainingPercent < $1.remainingPercent } ?? shortest
    }

    var asset: (name: String, ext: String)? {
        switch self.id {
        case "codex": ("chatgpt-app-icon", "png")
        case "claude": ("claude-app-icon", "jpg")
        case "zcode": ("zcode-app-icon", "png")
        case "opencode": ("opencode-app-icon", "png")
        case "kimi": ("kimi-app-icon", "png")
        case "antigravity": ("antigravity-app-icon", "png")
        case "workbuddy": ("workbuddy-app-icon", "png")
        case "qoder": ("qoder-app-icon", "png")
        case "deepseek": ("deepseek-app-icon", "png")
        case "dsh": ("dsh-app-icon", "png")
        case "grok": ("grok-app-icon", "png")
        case "grokbot": ("grokbot-app-icon", "png")
        case "hermes": ("hermes-app-icon", "png")
        case "pi": ("pi-app-icon", "png")
        case "qwen": ("qwen-app-icon", "png")
        case "cursor": ("cursor-app-icon", "png")
        case "minimax": ("minimax-app-icon", "png")
        default: nil
        }
    }
}

struct MetrikWidgetQuotaWindow: Decodable {
    let key: String
    let label: String
    let available: Bool
    let remainingPercent: Double
    let resetsInMinutes: Double?
    let stale: Bool
    let resetExpired: Bool
    let quality: String

    var roundedRemaining: Int {
        Int(self.remainingPercent.rounded().clamped(to: 0 ... 100))
    }
}

private extension Comparable {
    func clamped(to limits: ClosedRange<Self>) -> Self {
        min(max(self, limits.lowerBound), limits.upperBound)
    }
}

enum MetrikWidgetStore {
    static let snapshotKey = "widgetSnapshotJSON"
    private static let logger = Logger(
        subsystem: "app.metrik.desktop.widget",
        category: "snapshot")

    static func load() -> MetrikWidgetSnapshot? {
        // publisher helper 嵌入了和 widget 一样的 bundle identity，它的
        // UserDefaults.standard 写进的就是 widget container 的同一个 plist；
        // widget 用自己的 UserDefaults.standard 读同一份。不依赖 App Group。
        let sharedDefaults = UserDefaults.standard
        guard let data = sharedDefaults.data(forKey: self.snapshotKey) else {
            self.logger.error("Shared snapshot preference missing")
            return nil
        }
        do {
            let snapshot = try JSONDecoder().decode(MetrikWidgetSnapshot.self, from: data)
            self.logger.notice(
                "Shared snapshot decoded with \(snapshot.agents.count, privacy: .public) agents")
            return snapshot
        } catch {
            let cocoaError = error as NSError
            self.logger.error(
                "Shared snapshot decode failed: domain=\(cocoaError.domain, privacy: .public) code=\(cocoaError.code, privacy: .public)")
            return nil
        }
    }

    static let preview = MetrikWidgetSnapshot(
        schemaVersion: 1,
        generatedAt: ISO8601DateFormatter().string(from: Date()),
        totalTokens: 43_300_000,
        agents: [
            MetrikWidgetAgent(
                id: "codex",
                label: "ChatGPT",
                tokens: 30_400_000,
                bindingWindowKey: "seven_day",
                windows: [
                    MetrikWidgetQuotaWindow(
                        key: "seven_day",
                        label: "每周",
                        available: true,
                        remainingPercent: 89,
                        resetsInMinutes: 7_620,
                        stale: false,
                        resetExpired: false,
                        quality: "official_live")
                ]),
            MetrikWidgetAgent(
                id: "claude",
                label: "Claude",
                tokens: 9_700_000,
                bindingWindowKey: nil,
                windows: []),
            MetrikWidgetAgent(
                id: "zcode",
                label: "GLM",
                tokens: 2_300_000,
                bindingWindowKey: nil,
                windows: []),
            MetrikWidgetAgent(
                id: "opencode",
                label: "OpenCode",
                tokens: 900_000,
                bindingWindowKey: nil,
                windows: []),
            MetrikWidgetAgent(
                id: "kimi",
                label: "Kimi",
                tokens: 620_000,
                bindingWindowKey: nil,
                windows: []),
            MetrikWidgetAgent(
                id: "antigravity",
                label: "Antigravity",
                tokens: 410_000,
                bindingWindowKey: nil,
                windows: [])
        ])

    // 仅供真实桌面运行时读取失败时使用。生产组件绝不能把 gallery 的演示
    // 数字伪装成用户数据；空 Agent 列表会进入明确的“打开 Metrik 刷新”状态。
    static let unavailable = MetrikWidgetSnapshot(
        schemaVersion: 1,
        generatedAt: ISO8601DateFormatter().string(from: Date()),
        totalTokens: 0,
        agents: [])
}

enum MetrikWidgetFormat {
    static func tokens(_ value: Int64) -> String {
        let number = Double(max(value, 0))
        if number >= 1_000_000 {
            return String(format: number >= 10_000_000 ? "%.1fM" : "%.2fM", number / 1_000_000)
        }
        if number >= 1_000 {
            return String(format: number >= 100_000 ? "%.0fK" : "%.1fK", number / 1_000)
        }
        return String(value)
    }

    static func reset(_ minutes: Double?) -> String? {
        guard let minutes, minutes.isFinite, minutes > 0 else { return nil }
        let totalHours = Int(ceil(minutes / 60))
        let days = totalHours / 24
        let hours = totalHours % 24
        if days > 0, hours > 0 { return "\(days) 天 \(hours) 小时后重置" }
        if days > 0 { return "\(days) 天后重置" }
        return "\(max(hours, 1)) 小时后重置"
    }
}
