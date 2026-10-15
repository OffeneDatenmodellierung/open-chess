import OpenChessCore
import SwiftUI

/// Sync and data: low data, the interval, photos, notifications, the outbox.
@MainActor
struct SyncSettingsView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var sync: SyncEngine
    @EnvironmentObject var network: NetworkMonitor
    @EnvironmentObject var push: PushManager

    var body: some View {
        Form {
            Section {
                Picker("Low data", selection: $settings.lowDataOverride) {
                    Text("Automatic").tag("auto")
                    Text("On").tag("on")
                    Text("Off").tag("off")
                }
                HStack {
                    Text("Now")
                    Spacer()
                    Text(sync.lowData ? "Low data" : "Normal")
                        .foregroundStyle(sync.lowData ? Color.ocAmber : Color.ocMuted)
                }
                HStack {
                    Text("Network")
                    Spacer()
                    Text(network.summary).foregroundStyle(Color.ocMuted)
                }
            } header: {
                SectionTitle(text: "Data")
            } footer: {
                Text("Automatic turns low data on when iOS says the network is constrained (Low Data Mode) or expensive (cellular, hotspot). Low data sends games only, at most 50 KB a sync, and plays correspondence games only.")
            }
            .listRowBackground(Color.ocPanel)

            Section {
                Picker("Sync", selection: $settings.syncMinutes) {
                    ForEach(AppSettings.syncChoices, id: \.self) { m in
                        Text(AppSettings.syncLabel(m)).tag(m)
                    }
                }
                .onChange(of: settings.syncMinutes) { _, _ in sync.intervalChanged() }
                Button {
                    Task { await sync.syncNow(reason: "button") }
                } label: {
                    HStack {
                        Text("Sync now")
                        Spacer()
                        if sync.syncing { ProgressView() }
                    }
                }
                .disabled(sync.syncing)
                HStack {
                    Text("Last sync")
                    Spacer()
                    Text(agoText(ms: settings.lastSyncMs)).foregroundStyle(Color.ocMuted)
                }
                if let e = sync.lastError {
                    Text(e).font(OCFont.text(13)).foregroundStyle(Color.ocDanger)
                }
                ForEach(sync.notices, id: \.self) { n in
                    Text(n).font(OCFont.text(13)).foregroundStyle(Color.ocMuted)
                }
                if !sync.notices.isEmpty {
                    Button("Clear messages") { sync.dismissNotices() }
                }
            } header: {
                SectionTitle(text: "Sync")
            } footer: {
                Text("While the app is open it syncs on this interval; in the background iOS decides when, usually not more than every 15 minutes.")
            }
            .listRowBackground(Color.ocPanel)

            Section {
                Toggle("Upload board photos", isOn: $settings.photoUploads)
                Toggle("Photos on Wi-Fi only", isOn: $settings.photosWifiOnly)
                    .disabled(!settings.photoUploads)
                Toggle("Tell me when it's my move", isOn: Binding(
                    get: { settings.notifyMyMove },
                    set: { push.setEnabled($0) }
                ))
                if !push.status.isEmpty {
                    Text(push.status).font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                }
            } header: {
                SectionTitle(text: "Photos and notifications")
            }
            .listRowBackground(Color.ocPanel)

            Section {
                HStack {
                    Text("Waiting")
                    Spacer()
                    Text("\(sync.totals.items) · \(byteText(sync.totals.bytes))")
                        .font(OCFont.mono(15))
                        .foregroundStyle(Color.ocMuted)
                }
                if sync.outbox.isEmpty {
                    Text("Nothing waiting to be sent.").foregroundStyle(Color.ocMuted)
                }
                ForEach(sync.outbox, id: \.key) { item in
                    HStack {
                        Image(systemName: icon(item.kind)).foregroundStyle(Color.ocAction)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(item.kind.capitalized).font(OCFont.text(14, weight: .semibold))
                            Text(agoText(ms: UInt64(max(0, item.createdMs))))
                                .font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                        }
                        Spacer()
                        Text(byteText(UInt64(item.payload.count)))
                            .font(OCFont.mono(13))
                            .foregroundStyle(Color.ocMuted)
                    }
                }
            } header: {
                SectionTitle(text: "Outbox")
            }
            .listRowBackground(Color.ocPanel)
        }
        .ocScreen()
        .navigationTitle("Sync and data")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { sync.refreshOutbox() }
    }

    private func icon(_ kind: String) -> String {
        switch kind {
        case "game": return "checkerboard.rectangle"
        case "photo": return "photo"
        case "confirm": return "checkmark.seal"
        default: return "tray"
        }
    }
}
