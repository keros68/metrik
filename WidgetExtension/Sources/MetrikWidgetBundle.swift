import SwiftUI
import WidgetKit

@main
struct MetrikWidgetBundle: WidgetBundle {
    var body: some Widget {
        MetrikFocusWidget()
        MetrikOverviewWidget()
    }
}

struct MetrikFocusWidget: Widget {
    private let kind = "MetrikFocusWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: self.kind, provider: MetrikTimelineProvider()) { entry in
            MetrikFocusView(entry: entry)
        }
        .configurationDisplayName("Metrik 配额")
        .description("查看当前最重要的官方配额窗口与重置时间。")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

struct MetrikOverviewWidget: Widget {
    private let kind = "MetrikOverviewWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: self.kind, provider: MetrikTimelineProvider()) { entry in
            MetrikOverviewView(entry: entry)
        }
        .configurationDisplayName("Metrik 桌面卡片")
        .description("保留 Metrik 额度环与今日统计，并显示最多六个 Agent。")
        .supportedFamilies([.systemMedium, .systemLarge])
    }
}
