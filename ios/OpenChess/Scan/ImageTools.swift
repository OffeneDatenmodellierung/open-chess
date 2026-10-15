import UIKit

enum ImageTools {
    /// The image drawn upright (camera photos carry an orientation flag).
    static func upright(_ image: UIImage) -> UIImage {
        if image.imageOrientation == .up { return image }
        let format = UIGraphicsImageRendererFormat()
        format.scale = image.scale
        return UIGraphicsImageRenderer(size: image.size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: image.size))
        }
    }

    /// A centred square crop scaled to `side` pixels. Re-rendering leaves
    /// all metadata (EXIF, location) behind.
    static func squareCrop(_ image: UIImage, side: Int) -> UIImage {
        let img = upright(image)
        let w = img.size.width * img.scale
        let h = img.size.height * img.scale
        let s = min(w, h)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        let target = CGFloat(max(1, side))
        return UIGraphicsImageRenderer(size: CGSize(width: target, height: target), format: format).image { _ in
            let scale = target / s
            let drawW = w * scale
            let drawH = h * scale
            img.draw(in: CGRect(x: (target - drawW) / 2, y: (target - drawH) / 2, width: drawW, height: drawH))
        }
    }

    /// The image scaled so its longer side is at most `maxSide`, aspect kept.
    static func fit(_ image: UIImage, maxSide: Int) -> UIImage {
        let img = upright(image)
        let w = img.size.width * img.scale
        let h = img.size.height * img.scale
        let scale = min(1, CGFloat(maxSide) / max(w, h))
        let size = CGSize(width: max(1, (w * scale).rounded()), height: max(1, (h * scale).rounded()))
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        return UIGraphicsImageRenderer(size: size, format: format).image { _ in
            img.draw(in: CGRect(origin: .zero, size: size))
        }
    }

    /// RGBA8 pixels, row by row, for the board reader.
    static func rgba(_ image: UIImage) -> (data: Data, width: Int, height: Int)? {
        guard let cg = image.cgImage else { return nil }
        let width = cg.width
        let height = cg.height
        let bytesPerRow = width * 4
        var buffer = Data(count: bytesPerRow * height)
        let ok: Bool = buffer.withUnsafeMutableBytes { raw -> Bool in
            guard let base = raw.baseAddress,
                  let ctx = CGContext(
                      data: base,
                      width: width,
                      height: height,
                      bitsPerComponent: 8,
                      bytesPerRow: bytesPerRow,
                      space: CGColorSpaceCreateDeviceRGB(),
                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
                  ) else { return false }
            ctx.draw(cg, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard ok else { return nil }
        return (data: buffer, width: width, height: height)
    }

    /// A JPEG for upload: square, at most 768 px, no metadata.
    static func uploadJPEG(_ image: UIImage) -> Data? {
        let img = squareCrop(image, side: 768)
        return img.jpegData(compressionQuality: 0.72)
    }
}
