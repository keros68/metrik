import AppKit
import SwiftUI
import WidgetKit

struct MetrikWidgetEntry: TimelineEntry {
    let date: Date
    let snapshot: MetrikWidgetSnapshot
}

struct MetrikTimelineProvider: TimelineProvider {
    func placeholder(in context: Context) -> MetrikWidgetEntry {
        MetrikWidgetEntry(date: Date(), snapshot: MetrikWidgetStore.preview)
    }

    func getSnapshot(in context: Context, completion: @escaping (MetrikWidgetEntry) -> Void) {
        let snapshot = context.isPreview
            ? MetrikWidgetStore.preview
            : MetrikWidgetStore.load() ?? MetrikWidgetStore.unavailable
        completion(MetrikWidgetEntry(date: Date(), snapshot: snapshot))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<MetrikWidgetEntry>) -> Void) {
        let now = Date()
        let snapshot = MetrikWidgetStore.load() ?? MetrikWidgetStore.unavailable
        let entry = MetrikWidgetEntry(date: now, snapshot: snapshot)
        completion(Timeline(entries: [entry], policy: .after(now.addingTimeInterval(5 * 60))))
    }
}

struct MetrikProviderIcon: View {
    let agent: MetrikWidgetAgent
    let size: CGFloat

    var body: some View {
        Group {
            if let asset = agent.asset,
               let url = Bundle.main.url(forResource: asset.name, withExtension: asset.ext),
               let image = NSImage(contentsOf: url)
            {
                // 单色/透明桌面风格（macOS 15+ accented 渲染）默认把位图漂成白色，
                // 品牌图标必须保持全彩，否则只剩一个白方块。该修饰符是 WidgetKit 的
                // Image 扩展，必须先 resizable 再设置；macOS 14 没有它，走原版渲染。
                if #available(macOS 15.0, *) {
                    Image(nsImage: image)
                        .resizable()
                        .widgetAccentedRenderingMode(.fullColor)
                        .scaledToFit()
                } else {
                    Image(nsImage: image)
                        .resizable()
                        .scaledToFit()
                }
            } else {
                Image(systemName: "terminal")
                    .resizable()
                    .scaledToFit()
                    .padding(size * 0.22)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: size * 0.24, style: .continuous))
    }
}

private struct MetrikHeader: View {
    let snapshot: MetrikWidgetSnapshot
    var showsUpdated = true

    var body: some View {
        HStack(spacing: 7) {
            Text("Metrik")
                .font(.headline.weight(.semibold))
            Circle()
                .fill(self.hasFreshQuota ? Color.green : Color.orange)
                .frame(width: 7, height: 7)
                .accessibilityLabel(self.hasFreshQuota ? "数据已更新" : "部分数据可能过期")
            Spacer(minLength: 8)
            if self.showsUpdated {
                Text(self.updatedLabel)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var hasFreshQuota: Bool {
        snapshot.agents
            .compactMap(\.bindingWindow)
            .contains { !$0.stale }
    }

    private var updatedLabel: String {
        guard let date = ISO8601DateFormatter().date(from: snapshot.generatedAt) else { return "刚刚更新" }
        return date.formatted(.relative(presentation: .numeric, unitsStyle: .abbreviated))
    }
}

struct MetrikFocusView: View {
    @Environment(\.widgetFamily) private var family
    let entry: MetrikWidgetEntry

    var body: some View {
        let agent = self.focusAgent
        VStack(alignment: .leading, spacing: self.family == .systemSmall ? 8 : 10) {
            MetrikHeader(snapshot: entry.snapshot)
            if let agent {
                self.content(agent)
            } else {
                self.emptyState
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .containerBackground(.fill.tertiary, for: .widget)
    }

    private var focusAgent: MetrikWidgetAgent? {
        entry.snapshot.agents.first(where: { $0.bindingWindow != nil }) ?? entry.snapshot.agents.first
    }

    @ViewBuilder
    private func content(_ agent: MetrikWidgetAgent) -> some View {
        if self.family == .systemSmall {
            VStack(alignment: .leading, spacing: 7) {
                self.agentTitle(agent)
                self.quotaReading(agent)
                self.quotaProgress(agent)
                self.footer(agent)
            }
        } else {
            HStack(alignment: .center, spacing: 20) {
                VStack(alignment: .leading, spacing: 8) {
                    self.agentTitle(agent)
                    self.quotaReading(agent)
                }
                Divider()
                VStack(alignment: .leading, spacing: 9) {
                    self.quotaProgress(agent)
                    self.footer(agent)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: .infinity)
        }
    }

    private func agentTitle(_ agent: MetrikWidgetAgent) -> some View {
        HStack(spacing: 8) {
            MetrikProviderIcon(agent: agent, size: 28)
            VStack(alignment: .leading, spacing: 1) {
                Text(agent.label)
                    .font(.subheadline.weight(.semibold))
                Text(agent.bindingWindow?.label ?? "配额不可用")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private func quotaReading(_ agent: MetrikWidgetAgent) -> some View {
        if let quota = agent.bindingWindow {
            Text("\(quota.stale ? "~" : "")\(quota.roundedRemaining)%")
                .font(.system(size: self.family == .systemSmall ? 42 : 50, weight: .medium, design: .rounded))
                .monospacedDigit()
                .minimumScaleFactor(0.75)
                .lineLimit(1)
                .accessibilityLabel("剩余 \(quota.roundedRemaining) 百分比")
        } else {
            Text("--")
                .font(.system(size: 42, weight: .medium, design: .rounded))
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func quotaProgress(_ agent: MetrikWidgetAgent) -> some View {
        if let quota = agent.bindingWindow {
            VStack(alignment: .leading, spacing: 5) {
                ProgressView(value: quota.remainingPercent, total: 100)
                    .tint(quota.remainingPercent <= 15 ? .orange : .blue)
                if let reset = MetrikWidgetFormat.reset(quota.resetsInMinutes) {
                    Text(reset)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
        }
    }

    private func footer(_ agent: MetrikWidgetAgent) -> some View {
        HStack(spacing: 4) {
            Text("今日")
                .foregroundStyle(.secondary)
            Text(MetrikWidgetFormat.tokens(agent.tokens))
                .fontWeight(.semibold)
                .monospacedDigit()
            Text("tokens")
                .foregroundStyle(.secondary)
        }
        .font(.caption)
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text("打开 Metrik")
                .font(.body.weight(.semibold))
            Text("刷新后此处显示官方额度。")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

struct MetrikOverviewView: View {
    @Environment(\.widgetFamily) private var family
    let entry: MetrikWidgetEntry

    var body: some View {
        Group {
            if self.family == .systemLarge {
                MetrikDashboardView(entry: entry)
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    MetrikHeader(snapshot: entry.snapshot)
                    if self.visibleAgents.isEmpty {
                        Text("打开 Metrik 刷新 Agent 数据")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else {
                        VStack(spacing: 0) {
                            ForEach(self.visibleAgents) { agent in
                                MetrikOverviewRow(agent: agent, compact: true)
                                if agent.id != self.visibleAgents.last?.id {
                                    Divider()
                                }
                            }
                        }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .containerBackground(.fill.tertiary, for: .widget)
    }

    private var limit: Int { self.family == .systemLarge ? 6 : 3 }
    private var visibleAgents: [MetrikWidgetAgent] { Array(entry.snapshot.agents.prefix(self.limit)) }
}

/// The large family deliberately preserves the visual anatomy of the accepted
/// Metrik desktop-card concept. WidgetKit owns only the outer material, corners,
/// margins and tint; Metrik keeps its quota dial, today summary, Agent switcher,
/// and footer hierarchy inside that native container.
private struct MetrikDashboardView: View {
    let entry: MetrikWidgetEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            MetrikHeader(snapshot: entry.snapshot, showsUpdated: false)

            if let focusAgent {
                HStack(alignment: .center, spacing: 14) {
                    MetrikQuotaDial(agent: focusAgent)
                        .frame(width: 146, height: 132)
                    Divider()
                        .frame(height: 116)
                    MetrikTodaySummary(agent: focusAgent)
                        .frame(maxWidth: .infinity, maxHeight: 132, alignment: .center)
                }
                .frame(height: 132)
            }

            // 上下两个 Spacer 把 Agent 网格悬浮在中部、状态行压到底边：
            // Agent 数量由用户勾选决定，任何数量都不会在底部
            // 留出大块空白。
            Spacer(minLength: 0)

            MetrikDashboardAgentGrid(agents: self.visibleAgents)

            Spacer(minLength: 0)

            HStack(spacing: 6) {
                Circle()
                    .fill(self.hasFreshQuota ? Color.green : Color.orange)
                    .frame(width: 6, height: 6)
                Text(self.hasFreshQuota ? "刚刚更新" : "部分覆盖")
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
        }
    }

    private var focusAgent: MetrikWidgetAgent? {
        entry.snapshot.agents.first(where: { $0.bindingWindow != nil }) ?? entry.snapshot.agents.first
    }

    private var visibleAgents: [MetrikWidgetAgent] {
        // 快照已被宿主按用户的勾选过滤，不能再用固定上限截断选择。
        entry.snapshot.agents
    }

    private var hasFreshQuota: Bool {
        entry.snapshot.agents
            .compactMap(\.bindingWindow)
            .contains { !$0.stale }
    }
}

private struct MetrikQuotaDial: View {
    let agent: MetrikWidgetAgent

    var body: some View {
        let quota = agent.bindingWindow
        ZStack {
            Circle()
                .trim(from: 0.08, to: 0.92)
                .stroke(
                    Color.secondary.opacity(0.14),
                    style: StrokeStyle(lineWidth: 5.5, lineCap: .round))
                .rotationEffect(.degrees(90))

            if let quota {
                Circle()
                    .trim(
                        from: 0.08,
                        to: 0.08 + 0.84 * max(0, min(100, quota.remainingPercent)) / 100)
                    .stroke(
                        self.tint(quota),
                        style: StrokeStyle(lineWidth: 5.5, lineCap: .round))
                    .rotationEffect(.degrees(90))
            }

            VStack(spacing: 0) {
                Text(agent.label)
                    .font(.system(size: 10, weight: .semibold))
                    .lineLimit(1)
                Text(quota.map { "\($0.label) · 剩余" } ?? "配额不可用")
                    .font(.system(size: 8.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                if let quota {
                    Text("\(quota.stale ? "~" : "")\(quota.roundedRemaining)%")
                        .font(.system(size: 32, weight: .regular, design: .serif))
                        .monospacedDigit()
                        .minimumScaleFactor(0.8)
                        .lineLimit(1)
                } else {
                    Text("--")
                        .font(.system(size: 36, weight: .regular, design: .rounded))
                        .foregroundStyle(.secondary)
                }
                if let reset = MetrikWidgetFormat.reset(quota?.resetsInMinutes) {
                    Text(reset)
                        .font(.system(size: 8))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                }
            }
            .frame(width: 88)
            .offset(y: 6)
        }
        .frame(width: 128, height: 128)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(agent.label) \(quota?.label ?? "")")
        .accessibilityValue(quota.map { "剩余 \($0.roundedRemaining) 百分比" } ?? "配额不可用")
    }

    private func tint(_ quota: MetrikWidgetQuotaWindow?) -> Color {
        guard let quota else { return .secondary }
        return quota.remainingPercent <= 15 ? .orange : .blue
    }
}

private struct MetrikTodaySummary: View {
    let agent: MetrikWidgetAgent

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            MetrikProviderIcon(agent: agent, size: 34)
            Text("今日")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(MetrikWidgetFormat.tokens(agent.tokens))
                .font(.system(size: 34, weight: .regular, design: .serif))
                .monospacedDigit()
                .minimumScaleFactor(0.72)
                .lineLimit(1)
            Text("tokens")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct MetrikDashboardAgentGrid: View {
    let agents: [MetrikWidgetAgent]

    var body: some View {
        let columnCount = agents.count <= 2 ? 1 : 2
        let rowCount = max(1, Int(ceil(Double(agents.count) / Double(columnCount))))
        let cellHeight: CGFloat = if agents.count > 8 {
            24
        } else if agents.count > 6 {
            27
        } else if agents.count > 4 {
            31
        } else {
            38
        }
        let columns = Array(
            repeating: GridItem(.flexible(), spacing: 0),
            count: columnCount)

        LazyVGrid(columns: columns, alignment: .leading, spacing: 0) {
            ForEach(Array(agents.enumerated()), id: \.element.id) { index, agent in
                MetrikDashboardAgentCell(
                    agent: agent,
                    compact: agents.count > 4,
                    showsTrailingDivider: columnCount == 2 && index.isMultiple(of: 2),
                    showsBottomDivider: index < agents.count - columnCount)
                    .frame(height: cellHeight)
            }
        }
        .frame(height: cellHeight * CGFloat(rowCount))
        .background(
            Color.primary.opacity(0.035),
            in: RoundedRectangle(cornerRadius: 15, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 15, style: .continuous)
                .stroke(Color.primary.opacity(0.08), lineWidth: 0.75)
        }
        .clipShape(RoundedRectangle(cornerRadius: 15, style: .continuous))
    }
}

private struct MetrikDashboardAgentCell: View {
    let agent: MetrikWidgetAgent
    let compact: Bool
    let showsTrailingDivider: Bool
    let showsBottomDivider: Bool

    var body: some View {
        HStack(spacing: self.compact ? 5 : 7) {
            Capsule()
                .fill(self.accent)
                .frame(width: 3, height: self.compact ? 18 : 23)
            MetrikProviderIcon(agent: agent, size: self.compact ? 21 : 25)
            Text(agent.label)
                .font((self.compact ? Font.caption2 : Font.caption).weight(.semibold))
                .lineLimit(1)
                .minimumScaleFactor(0.75)
            Spacer(minLength: 3)
            Text(self.remainingText)
                .font(.caption2.weight(.semibold))
                .monospacedDigit()
        }
        .padding(.horizontal, self.compact ? 6 : 8)
        .overlay(alignment: .trailing) {
            if self.showsTrailingDivider {
                Rectangle()
                    .fill(Color.primary.opacity(0.08))
                    .frame(width: 0.5)
            }
        }
        .overlay(alignment: .bottom) {
            if self.showsBottomDivider {
                Rectangle()
                    .fill(Color.primary.opacity(0.08))
                    .frame(height: 0.5)
            }
        }
    }

    private var remainingText: String {
        guard let quota = agent.bindingWindow else { return "--" }
        return "\(quota.stale ? "~" : "")\(quota.roundedRemaining)%"
    }

    private var accent: Color {
        switch agent.id {
        case "codex": .blue
        case "claude": .orange
        case "zcode": .purple
        case "opencode": .mint
        case "kimi": .indigo
        case "antigravity": .cyan
        default: .gray
        }
    }
}

private struct MetrikOverviewRow: View {
    let agent: MetrikWidgetAgent
    let compact: Bool

    var body: some View {
        HStack(spacing: self.compact ? 8 : 10) {
            MetrikProviderIcon(agent: agent, size: self.compact ? 26 : 30)
            VStack(alignment: .leading, spacing: self.compact ? 2 : 4) {
                HStack(alignment: .firstTextBaseline) {
                    Text(agent.label)
                        .font(.subheadline.weight(.semibold))
                    if let label = agent.bindingWindow?.label {
                        Text(label)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 8)
                    Text(self.remainingText)
                        .font(.subheadline.weight(.semibold))
                        .monospacedDigit()
                }
                if let quota = agent.bindingWindow {
                    ProgressView(value: quota.remainingPercent, total: 100)
                        .tint(quota.remainingPercent <= 15 ? .orange : .blue)
                } else {
                    Text("等待官方配额")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .padding(.vertical, self.compact ? 2 : 5)
    }

    private var remainingText: String {
        guard let quota = agent.bindingWindow else { return "--" }
        return "\(quota.stale ? "~" : "")\(quota.roundedRemaining)%"
    }
}
