import SwiftUI
import UIKit

/// Copies of streams recorded on this phone (Documents, .mp4).
enum Recordings {
    static var directory: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
    }

    static func newURL() -> URL {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyyMMdd-HHmmss"
        return directory.appendingPathComponent("open-chess-\(f.string(from: Date())).mp4")
    }

    static func list() -> [URL] {
        let keys: [URLResourceKey] = [.contentModificationDateKey, .fileSizeKey]
        let urls = (try? FileManager.default.contentsOfDirectory(
            at: directory, includingPropertiesForKeys: keys, options: [.skipsHiddenFiles]
        )) ?? []
        return urls
            .filter { $0.pathExtension.lowercased() == "mp4" }
            .sorted { modified($0) > modified($1) }
    }

    static func modified(_ url: URL) -> Date {
        (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
    }

    static func size(_ url: URL) -> UInt64 {
        let n = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        return UInt64(max(0, n))
    }
}

/// Streaming settings: the service, the stream key (Keychain), bitrate,
/// a recorded copy and portrait pictures.
@MainActor
struct StreamSettingsView: View {
    @EnvironmentObject var settings: AppSettings
    @State var keyDraft = ""
    @State var hasKey = false
    @State var note: String?

    var body: some View {
        Form {
            Section {
                Picker("Service", selection: $settings.streamService) {
                    Text("Twitch").tag("twitch")
                    Text("YouTube").tag("youtube")
                    Text("Custom").tag("custom")
                }
                if settings.streamService == "custom" {
                    TextField("rtmp://server/app", text: $settings.streamURL)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } else {
                    Text(StreamTarget.base(service: settings.streamService, custom: settings.streamURL))
                        .font(OCFont.mono(13))
                        .foregroundStyle(Color.ocMuted)
                }
                if hasKey {
                    HStack {
                        Text("Stream key")
                        Spacer()
                        Text("Saved").foregroundStyle(Color.ocMuted)
                    }
                    Button("Remove the stream key", role: .destructive) {
                        Keychain.set(AppSettings.streamKeyAccount, nil)
                        hasKey = false
                    }
                } else {
                    SecureField("Stream key", text: $keyDraft)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Save the stream key") {
                        Keychain.set(AppSettings.streamKeyAccount, keyDraft.trimmed)
                        keyDraft = ""
                        hasKey = Keychain.get(AppSettings.streamKeyAccount) != nil
                    }
                    .disabled(keyDraft.trimmed.isEmpty)
                }
            } header: {
                SectionTitle(text: "Where to")
            } footer: {
                Text("Find the stream key in Twitch's Creator Dashboard (Settings → Stream) or YouTube Studio (Go live → Stream). It stays in this device's Keychain.")
            }
            .listRowBackground(Color.ocPanel)

            Section {
                Picker("Video bitrate", selection: $settings.streamKbps) {
                    ForEach(AppSettings.streamBitrates, id: \.self) { k in
                        Text(String(format: "%.1f Mbps", Double(k) / 1000)).tag(k)
                    }
                }
                Toggle("Portrait picture (720×1280)", isOn: $settings.streamPortrait)
                Toggle("Record a copy on this phone", isOn: $settings.streamRecord)
                NavigationLink("Recordings") { RecordingsView() }
            } header: {
                SectionTitle(text: "Picture")
            } footer: {
                Text("Landscape 1280×720 by default. 4 Mbps suits most home connections; streaming uses a lot of data on cellular.")
            }
            .listRowBackground(Color.ocPanel)
        }
        .ocScreen()
        .navigationTitle("Streaming")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            hasKey = Keychain.get(AppSettings.streamKeyAccount) != nil
        }
    }
}

/// Recorded copies: share or delete.
@MainActor
struct RecordingsView: View {
    @State var files: [URL] = []

    var body: some View {
        List {
            if files.isEmpty {
                Text("No recordings yet. Turn on \"Record a copy on this phone\" before streaming.")
                    .foregroundStyle(Color.ocMuted)
            }
            ForEach(files, id: \.self) { url in
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(url.lastPathComponent).font(OCFont.text(14, weight: .semibold))
                        Text(byteText(Recordings.size(url)))
                            .font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                    }
                    Spacer()
                    ShareLink(item: url) {
                        Image(systemName: "square.and.arrow.up")
                    }
                }
            }
            .onDelete { idx in
                for i in idx {
                    try? FileManager.default.removeItem(at: files[i])
                }
                files = Recordings.list()
            }
            .listRowBackground(Color.ocPanel)
        }
        .ocScreen()
        .navigationTitle("Recordings")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { files = Recordings.list() }
    }
}

/// The stream's cutout: the board, the players and the last five moves,
/// on a semi-opaque panel with rounded corners. Rendered to an image with
/// `ImageRenderer`, so it's drawn at a fixed width in pixels.
@MainActor
struct StreamCutoutView: View {
    let placement: Placement
    let lastMove: UCIMove?
    let check: Int?
    let white: String
    let black: String
    let lines: [MoveLine]
    let result: String?
    let width: CGFloat

    var body: some View {
        let boardSide = (width - 16) * 0.86
        VStack(alignment: .leading, spacing: 6) {
            player(black, white: false)
            BoardView(placement: placement, lastMove: lastMove, check: check)
                .frame(width: boardSide, height: boardSide)
                .frame(maxWidth: .infinity)
            player(white, white: true)
            VStack(alignment: .leading, spacing: 2) {
                ForEach(lines) { line in
                    Text(line.text)
                        .font(OCFont.mono(15, weight: line.current ? .bold : .regular))
                        .foregroundStyle(line.current ? Color.ocAction : Color.ocInk)
                }
            }
            if let result {
                Text(result)
                    .font(OCFont.text(15, weight: .bold))
                    .foregroundStyle(Color.ocAmber)
            }
        }
        .padding(8)
        .frame(width: width, alignment: .leading)
        .background(Color.ocPanel.opacity(0.82))
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .foregroundStyle(Color.ocInk)
        .environment(\.colorScheme, .dark)
    }

    private func player(_ name: String, white: Bool) -> some View {
        HStack(spacing: 6) {
            Circle()
                .fill(white ? Color.ocWSeg : Color.ocBSeg)
                .overlay(Circle().stroke(Color.ocRule, lineWidth: 1))
                .frame(width: 12, height: 12)
            Text(name)
                .font(OCFont.text(15, weight: .semibold))
                .lineLimit(1)
        }
    }

    /// The cutout as an image (pixels = points).
    static func render(_ view: StreamCutoutView) -> CGImage? {
        let renderer = ImageRenderer(content: view)
        renderer.scale = 1
        renderer.isOpaque = false
        return renderer.cgImage
    }
}
