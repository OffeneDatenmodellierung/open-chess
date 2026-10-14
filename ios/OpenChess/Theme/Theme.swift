import SwiftUI
import UIKit

// The esports site's theme: dark by default, light available. Colours are
// dynamic UIColors, so they follow the colour scheme SwiftUI applies
// (system, or the override chosen in Settings).

extension UIColor {
    convenience init(hex: UInt32) {
        let r = CGFloat((hex >> 16) & 0xff) / 255
        let g = CGFloat((hex >> 8) & 0xff) / 255
        let b = CGFloat(hex & 0xff) / 255
        self.init(red: r, green: g, blue: b, alpha: 1)
    }

    static func themed(dark: UInt32, light: UInt32) -> UIColor {
        UIColor { traits in
            traits.userInterfaceStyle == .light ? UIColor(hex: light) : UIColor(hex: dark)
        }
    }
}

enum Palette {
    static let paper = UIColor.themed(dark: 0x06114a, light: 0xeef2fb)
    static let panel = UIColor.themed(dark: 0x0d1e6a, light: 0xffffff)
    static let ink = UIColor.themed(dark: 0xffffff, light: 0x06114a)
    static let muted = UIColor.themed(dark: 0xa9bdee, light: 0x4d5f8b)
    static let rule = UIColor.themed(dark: 0x253f9c, light: 0xc9d4ee)
    static let sqLight = UIColor.themed(dark: 0xc9d4ee, light: 0xe3eafa)
    static let sqDark = UIColor.themed(dark: 0x5b77c9, light: 0x7b8bb0)
    static let amber = UIColor.themed(dark: 0xffc233, light: 0xb67d00)
    static let action = UIColor.themed(dark: 0xff7a1a, light: 0xd94a00)
    static let actionInk = UIColor.themed(dark: 0x06114a, light: 0xffffff)
    static let danger = UIColor.themed(dark: 0xff5577, light: 0xc4232d)
    static let wSeg = UIColor.themed(dark: 0xffffff, light: 0xffffff)
    static let bSeg = UIColor.themed(dark: 0x020825, light: 0x06114a)
    static let dSeg = UIColor.themed(dark: 0x8294c6, light: 0xa7b7dc)
}

extension Color {
    static let ocPaper = Color(uiColor: Palette.paper)
    static let ocPanel = Color(uiColor: Palette.panel)
    static let ocInk = Color(uiColor: Palette.ink)
    static let ocMuted = Color(uiColor: Palette.muted)
    static let ocRule = Color(uiColor: Palette.rule)
    static let ocSqLight = Color(uiColor: Palette.sqLight)
    static let ocSqDark = Color(uiColor: Palette.sqDark)
    static let ocAmber = Color(uiColor: Palette.amber)
    static let ocAction = Color(uiColor: Palette.action)
    static let ocActionInk = Color(uiColor: Palette.actionInk)
    static let ocDanger = Color(uiColor: Palette.danger)
    static let ocWSeg = Color(uiColor: Palette.wSeg)
    static let ocBSeg = Color(uiColor: Palette.bSeg)
    static let ocDSeg = Color(uiColor: Palette.dSeg)

    // Fixed colours, the same in both schemes.
    static let ocLastLight = Color(uiColor: UIColor(hex: 0xffd2a6))
    static let ocLastDark = Color(uiColor: UIColor(hex: 0xe8873a))
    static let ocBlackPiece = Color(uiColor: UIColor(hex: 0x020825))
}

/// Inter and JetBrains Mono when they're installed (they aren't bundled),
/// otherwise the system font with monospaced digits for numbers.
enum OCFont {
    static let interName: String? = {
        for n in ["Inter", "Inter-Regular", "InterVariable"] where UIFont(name: n, size: 12) != nil {
            return n
        }
        return nil
    }()

    static let monoName: String? = {
        for n in ["JetBrainsMono-Regular", "JetBrains Mono"] where UIFont(name: n, size: 12) != nil {
            return n
        }
        return nil
    }()

    static func text(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        if let name = interName {
            return Font.custom(name, size: size).weight(weight)
        }
        return Font.system(size: size, weight: weight)
    }

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        if let name = monoName {
            return Font.custom(name, size: size).weight(weight)
        }
        return Font.system(size: size, weight: weight).monospacedDigit()
    }
}

enum OCMetrics {
    static let corner: CGFloat = 2
}

// MARK: - Cards and buttons

struct OCCard: ViewModifier {
    var padding: CGFloat = 14

    func body(content: Content) -> some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.ocPanel)
            .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: OCMetrics.corner, style: .continuous)
                    .stroke(Color.ocRule, lineWidth: 1)
            )
    }
}

extension View {
    func ocCard(padding: CGFloat = 14) -> some View {
        modifier(OCCard(padding: padding))
    }

    /// The page background used on every screen.
    func ocScreen() -> some View {
        self
            .scrollContentBackground(.hidden)
            .background(Color.ocPaper.ignoresSafeArea())
            .foregroundStyle(Color.ocInk)
    }
}

enum OCButtonKind {
    case primary, secondary, danger
}

struct OCButtonStyle: ButtonStyle {
    var kind: OCButtonKind = .secondary
    var compact = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(OCFont.text(compact ? 14 : 16, weight: .semibold))
            .padding(.vertical, compact ? 8 : 13)
            .padding(.horizontal, compact ? 10 : 16)
            .frame(maxWidth: compact ? nil : CGFloat.infinity)
            .foregroundStyle(foreground)
            .background(background)
            .clipShape(RoundedRectangle(cornerRadius: OCMetrics.corner, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: OCMetrics.corner, style: .continuous)
                    .stroke(border, lineWidth: 1)
            )
            .opacity(configuration.isPressed ? 0.75 : 1)
    }

    private var foreground: Color {
        switch kind {
        case .primary: return .ocActionInk
        case .secondary: return .ocInk
        case .danger: return .ocDanger
        }
    }

    private var background: Color {
        switch kind {
        case .primary: return .ocAction
        case .secondary, .danger: return .ocPanel
        }
    }

    private var border: Color {
        switch kind {
        case .primary: return .ocAction
        case .secondary: return .ocRule
        case .danger: return .ocDanger
        }
    }
}

/// A small square-cornered label, like "Hints on · unrated".
@MainActor
struct OCBadge: View {
    let text: String
    var color: Color = .ocAmber

    var body: some View {
        Text(text)
            .font(OCFont.text(12, weight: .semibold))
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .foregroundStyle(color)
            .overlay(
                RoundedRectangle(cornerRadius: OCMetrics.corner).stroke(color, lineWidth: 1)
            )
    }
}

@MainActor
struct SectionTitle: View {
    let text: String

    var body: some View {
        Text(text.uppercased())
            .font(OCFont.text(12, weight: .semibold))
            .foregroundStyle(Color.ocMuted)
            .tracking(0.8)
    }
}

@MainActor
enum TabBarStyle {
    /// Panel background, and a 2 pt top edge in `action`.
    static func apply() {
        let edge = UIGraphicsImageRenderer(size: CGSize(width: 1, height: 2)).image { ctx in
            UIColor.black.setFill()
            ctx.fill(CGRect(x: 0, y: 0, width: 1, height: 2))
        }.withRenderingMode(.alwaysTemplate)

        let a = UITabBarAppearance()
        a.configureWithOpaqueBackground()
        a.backgroundColor = Palette.panel
        a.shadowImage = edge
        a.shadowColor = Palette.action
        let item = a.stackedLayoutAppearance
        item.normal.iconColor = Palette.muted
        item.normal.titleTextAttributes = [.foregroundColor: Palette.muted]
        item.selected.iconColor = Palette.action
        item.selected.titleTextAttributes = [.foregroundColor: Palette.action]
        a.inlineLayoutAppearance = item
        a.compactInlineLayoutAppearance = item
        UITabBar.appearance().standardAppearance = a
        UITabBar.appearance().scrollEdgeAppearance = a

        let nav = UINavigationBarAppearance()
        nav.configureWithOpaqueBackground()
        nav.backgroundColor = Palette.paper
        nav.shadowColor = Palette.rule
        nav.titleTextAttributes = [.foregroundColor: Palette.ink]
        nav.largeTitleTextAttributes = [.foregroundColor: Palette.ink]
        UINavigationBar.appearance().standardAppearance = nav
        UINavigationBar.appearance().scrollEdgeAppearance = nav
        UINavigationBar.appearance().compactAppearance = nav
    }
}
