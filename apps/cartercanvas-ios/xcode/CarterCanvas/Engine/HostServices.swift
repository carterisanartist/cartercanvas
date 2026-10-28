import CarterCanvasEngine
import UIKit
import UniformTypeIdentifiers

/// The platform services the engine calls back into: document pickers, the pasteboard, haptics,
/// links and the share sheet. Engine callbacks arrive on the main thread from inside `cc_*` calls,
/// so anything that calls the engine back is deferred to the next main-queue turn.
final class HostServices: NSObject {
    private weak var controller: EngineViewController?
    /// Security-scoped URLs (picked files and folders) kept open so saves can write back.
    private var accessed: [String: URL] = [:]
    private var pendingPickers: [ObjectIdentifier: (id: UInt64, kind: UInt32)] = [:]
    private let impact = UIImpactFeedbackGenerator(style: .light)
    private let notify = UINotificationFeedbackGenerator()
    private static let bookmarksKey = "cartercanvas.folderBookmarks"

    init(controller: EngineViewController) {
        self.controller = controller
        super.init()
        restoreBookmarks()
    }

    func callbacks(userData: UnsafeMutableRawPointer) -> CCHostCallbacks {
        var c = CCHostCallbacks()
        c.user_data = userData
        c.file_dialog = { ud, id, kind, json in
            HostServices.of(ud)?.fileDialog(id: id, kind: kind, json: json.map { String(cString: $0) } ?? "{}")
        }
        c.open_url = { ud, url in
            guard let url, let services = HostServices.of(ud) else { return false }
            return services.openURL(String(cString: url))
        }
        c.clipboard_set_image = { ud, w, h, rgba, len in
            guard let rgba else { return false }
            return HostServices.of(ud)?.setPasteboardImage(width: Int(w), height: Int(h), rgba: UnsafeBufferPointer(start: rgba, count: len)) ?? false
        }
        c.clipboard_get_image = { ud, sink in
            HostServices.of(ud)?.pasteboardImage(into: sink)
        }
        c.clipboard_set_text = { _, text in
            guard let text else { return }
            UIPasteboard.general.string = String(cString: text)
        }
        c.set_keyboard_visible = { ud, visible in
            let services = HostServices.of(ud)
            DispatchQueue.main.async { services?.controller?.setKeyboardVisible(visible) }
        }
        c.haptic = { ud, kind in
            HostServices.of(ud)?.haptic(kind)
        }
        c.share_file = { ud, path in
            guard let path else { return }
            let file = String(cString: path)
            let services = HostServices.of(ud)
            DispatchQueue.main.async { services?.share(path: file) }
        }
        c.request_paste = { ud in
            let services = HostServices.of(ud)
            DispatchQueue.main.async {
                guard let engine = services?.controller?.engine, let text = UIPasteboard.general.string else { return }
                cc_paste(engine, text)
            }
        }
        return c
    }

    private static func of(_ userData: UnsafeMutableRawPointer?) -> HostServices? {
        guard let userData else { return nil }
        return Unmanaged<EngineViewController>.fromOpaque(userData).takeUnretainedValue().services
    }

    // MARK: Security-scoped access

    /// Starts (and keeps) access to a picked or handed-over URL; returns it when readable.
    @discardableResult
    func access(_ url: URL) -> URL? {
        let key = url.standardizedFileURL.path
        if accessed[key] != nil { return url }
        if url.startAccessingSecurityScopedResource() {
            accessed[key] = url
        }
        return FileManager.default.isReadableFile(atPath: url.path) || url.hasDirectoryPath ? url : nil
    }

    func stopAccessingAll() {
        for url in accessed.values { url.stopAccessingSecurityScopedResource() }
        accessed.removeAll()
    }

    /// Folders the user chose to save into stay writable across launches.
    private func rememberFolder(_ url: URL) {
        guard let data = try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil) else { return }
        var saved = UserDefaults.standard.array(forKey: Self.bookmarksKey) as? [Data] ?? []
        saved.append(data)
        UserDefaults.standard.set(Array(saved.suffix(16)), forKey: Self.bookmarksKey)
    }

    private func restoreBookmarks() {
        let saved = UserDefaults.standard.array(forKey: Self.bookmarksKey) as? [Data] ?? []
        for data in saved {
            var stale = false
            if let url = try? URL(resolvingBookmarkData: data, options: [], relativeTo: nil, bookmarkDataIsStale: &stale) {
                access(url)
            }
        }
    }

    // MARK: File dialogs

    private func answer(id: UInt64, kind: UInt32, paths: [String]?) {
        DispatchQueue.main.async { [weak self] in
            guard let engine = self?.controller?.engine else { return }
            if let paths, let data = try? JSONSerialization.data(withJSONObject: paths), let json = String(data: data, encoding: .utf8) {
                cc_file_dialog_answer(engine, id, kind, json)
            } else {
                cc_file_dialog_answer(engine, id, kind, nil)
            }
        }
    }

    private func fileDialog(id: UInt64, kind: UInt32, json: String) {
        let request = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] ?? [:]
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            switch Int(kind) {
            case Int(CC_DIALOG_SAVE):
                self.saveSheet(id: id, request: request)
            case Int(CC_DIALOG_FOLDER):
                self.picker(id: id, kind: kind, types: [.folder], multiple: false)
            default:
                let extensions = request["extensions"] as? [String] ?? []
                self.picker(id: id, kind: kind, types: Self.contentTypes(extensions), multiple: Int(kind) == Int(CC_DIALOG_OPEN_MANY))
            }
        }
    }

    static func contentTypes(_ extensions: [String]) -> [UTType] {
        var types = extensions.compactMap { UTType(filenameExtension: $0.trimmingCharacters(in: CharacterSet(charactersIn: ".*"))) }
        if types.isEmpty { types = [.image, .data] }
        return types
    }

    private func picker(id: UInt64, kind: UInt32, types: [UTType], multiple: Bool) {
        guard let presenter = topController() else {
            failDialog(id: id, message: "There's no window to show the file picker in.")
            return
        }
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: types, asCopy: false)
        picker.allowsMultipleSelection = multiple
        picker.shouldShowFileExtensions = true
        picker.directoryURL = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
        picker.delegate = self
        pendingPickers[ObjectIdentifier(picker)] = (id, kind)
        presenter.present(picker, animated: true)
    }

    private func failDialog(id: UInt64, message: String) {
        DispatchQueue.main.async { [weak self] in
            guard let engine = self?.controller?.engine else { return }
            cc_file_dialog_failed(engine, id, message)
        }
    }

    /// Save: a name, then the app's folder (shown in Files), the document's own folder, or any
    /// folder the user picks.
    private func saveSheet(id: UInt64, request: [String: Any]) {
        guard let presenter = topController() else {
            failDialog(id: id, message: "There's no window to show the save sheet in.")
            return
        }
        let suggested = request["suggested"] as? String ?? "Untitled.pcraft"
        let documents = URL(fileURLWithPath: request["documents"] as? String ?? NSTemporaryDirectory(), isDirectory: true)
        let folder = URL(fileURLWithPath: request["folder"] as? String ?? documents.path, isDirectory: true)
        let device = UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone"
        let alert = UIAlertController(title: "Save As", message: nil, preferredStyle: .alert)
        alert.addTextField { field in
            field.text = suggested
            field.clearButtonMode = .whileEditing
            field.autocorrectionType = .no
            field.autocapitalizationType = .none
            field.keyboardAppearance = .dark
        }
        let name: () -> String? = {
            let raw = alert.textFields?.first?.text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            let cleaned = raw.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: ":", with: "-")
            guard !cleaned.isEmpty, cleaned != ".", cleaned != ".." else { return nil }
            // Keep the format the engine asked for when the name lost its extension.
            let ext = (suggested as NSString).pathExtension
            return (cleaned as NSString).pathExtension.isEmpty && !ext.isEmpty ? "\(cleaned).\(ext)" : cleaned
        }
        let save: (URL) -> Void = { [weak self] dir in
            guard let self else { return }
            guard let file = name() else {
                self.answer(id: id, kind: UInt32(CC_DIALOG_SAVE), paths: nil)
                return
            }
            self.confirmReplace(dir.appendingPathComponent(file), id: id)
        }
        if folder.standardizedFileURL != documents.standardizedFileURL, FileManager.default.isWritableFile(atPath: folder.path) {
            alert.addAction(UIAlertAction(title: "Save Next to Original", style: .default) { _ in save(folder) })
        }
        alert.addAction(UIAlertAction(title: "Save On My \(device) › CarterCanvas", style: .default) { _ in save(documents) })
        alert.addAction(UIAlertAction(title: "Choose Folder…", style: .default) { [weak self] _ in
            guard let self else { return }
            guard let file = name() else {
                self.answer(id: id, kind: UInt32(CC_DIALOG_SAVE), paths: nil)
                return
            }
            self.pickSaveFolder(id: id, file: file)
        })
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel) { [weak self] _ in
            self?.answer(id: id, kind: UInt32(CC_DIALOG_SAVE), paths: nil)
        })
        presenter.present(alert, animated: true)
    }

    private var saveFolderFile: [ObjectIdentifier: String] = [:]

    private func pickSaveFolder(id: UInt64, file: String) {
        guard let presenter = topController() else {
            failDialog(id: id, message: "There's no window to show the folder picker in.")
            return
        }
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.folder], asCopy: false)
        picker.delegate = self
        pendingPickers[ObjectIdentifier(picker)] = (id, UInt32(CC_DIALOG_SAVE))
        saveFolderFile[ObjectIdentifier(picker)] = file
        presenter.present(picker, animated: true)
    }

    private func confirmReplace(_ target: URL, id: UInt64) {
        guard FileManager.default.fileExists(atPath: target.path), let presenter = topController() else {
            answer(id: id, kind: UInt32(CC_DIALOG_SAVE), paths: [target.path])
            return
        }
        let alert = UIAlertController(title: "Replace “\(target.lastPathComponent)”?", message: "A file with that name already exists in this folder.", preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: "Replace", style: .destructive) { [weak self] _ in
            self?.answer(id: id, kind: UInt32(CC_DIALOG_SAVE), paths: [target.path])
        })
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel) { [weak self] _ in
            self?.answer(id: id, kind: UInt32(CC_DIALOG_SAVE), paths: nil)
        })
        presenter.present(alert, animated: true)
    }

    private func topController() -> UIViewController? {
        var top: UIViewController? = controller
        while let next = top?.presentedViewController, !next.isBeingDismissed { top = next }
        return top
    }

    // MARK: Links, haptics, sharing

    private func openURL(_ string: String) -> Bool {
        guard let url = URL(string: string), let scheme = url.scheme?.lowercased(), ["http", "https", "mailto", "shareddocuments"].contains(scheme) else { return false }
        UIApplication.shared.open(url)
        return true
    }

    private func haptic(_ kind: UInt32) {
        switch Int(kind) {
        case Int(CC_HAPTIC_SUCCESS): notify.notificationOccurred(.success)
        case Int(CC_HAPTIC_WARNING): notify.notificationOccurred(.warning)
        default: impact.impactOccurred()
        }
    }

    func share(path: String) {
        guard let presenter = topController() else { return }
        let sheet = UIActivityViewController(activityItems: [URL(fileURLWithPath: path)], applicationActivities: nil)
        if let pop = sheet.popoverPresentationController, let view = controller?.view {
            pop.sourceView = view
            pop.sourceRect = CGRect(x: view.bounds.midX, y: view.bounds.minY + 44, width: 1, height: 1)
        }
        presenter.present(sheet, animated: true)
    }

    // MARK: Pasteboard

    private func setPasteboardImage(width: Int, height: Int, rgba: UnsafeBufferPointer<UInt8>) -> Bool {
        guard width > 0, height > 0, rgba.count == width * height * 4,
              let provider = CGDataProvider(data: Data(buffer: rgba) as CFData),
              let image = CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
                                  space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                                  provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent),
              let png = UIImage(cgImage: image).pngData()
        else { return false }
        UIPasteboard.general.setData(png, forPasteboardType: UTType.png.identifier)
        return true
    }

    private func pasteboardImage(into sink: UnsafeMutableRawPointer?) {
        guard let sink, UIPasteboard.general.hasImages, let image = UIPasteboard.general.image, let cg = Self.uprightCGImage(image) else { return }
        let w = cg.width, h = cg.height
        guard w > 0, h > 0, w <= 32768, h <= 32768 else { return }
        var pixels = [UInt8](repeating: 0, count: w * h * 4)
        let drawn = pixels.withUnsafeMutableBytes { buf -> Bool in
            guard let ctx = CGContext(data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                      space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        guard drawn else { return }
        // The engine takes straight (unpremultiplied) alpha.
        for i in stride(from: 0, to: pixels.count, by: 4) {
            let a = Int(pixels[i + 3])
            if a > 0, a < 255 {
                pixels[i] = UInt8(min(255, (Int(pixels[i]) * 255 + a / 2) / a))
                pixels[i + 1] = UInt8(min(255, (Int(pixels[i + 1]) * 255 + a / 2) / a))
                pixels[i + 2] = UInt8(min(255, (Int(pixels[i + 2]) * 255 + a / 2) / a))
            }
        }
        pixels.withUnsafeBufferPointer { cc_image_sink_put(sink, UInt32(w), UInt32(h), $0.baseAddress, $0.count) }
    }

    /// The image's pixels with its orientation applied.
    private static func uprightCGImage(_ image: UIImage) -> CGImage? {
        if image.imageOrientation == .up, let cg = image.cgImage { return cg }
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = false
        let size = CGSize(width: image.size.width * image.scale, height: image.size.height * image.scale)
        return UIGraphicsImageRenderer(size: size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }.cgImage
    }
}

extension HostServices: UIDocumentPickerDelegate {
    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        let key = ObjectIdentifier(controller)
        guard let (id, kind) = pendingPickers.removeValue(forKey: key) else { return }
        if Int(kind) == Int(CC_DIALOG_SAVE) {
            let file = saveFolderFile.removeValue(forKey: key) ?? "Untitled.pcraft"
            guard let folder = urls.first, access(folder) != nil else {
                failDialog(id: id, message: "CarterCanvas can't write to that folder.")
                return
            }
            rememberFolder(folder)
            confirmReplace(folder.appendingPathComponent(file), id: id)
            return
        }
        let paths = urls.compactMap { access($0)?.path }
        if Int(kind) == Int(CC_DIALOG_FOLDER), let folder = urls.first { rememberFolder(folder) }
        answer(id: id, kind: kind, paths: paths.isEmpty ? nil : paths)
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        let key = ObjectIdentifier(controller)
        saveFolderFile.removeValue(forKey: key)
        guard let (id, kind) = pendingPickers.removeValue(forKey: key) else { return }
        answer(id: id, kind: kind, paths: nil)
    }
}
