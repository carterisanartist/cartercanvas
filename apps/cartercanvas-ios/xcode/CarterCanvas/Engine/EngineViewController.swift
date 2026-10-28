import CarterCanvasEngine
import os
import UIKit

/// Owns the engine: creates it on the Metal view's layer, ticks it from a display link, and
/// forwards lifecycle, keyboard and file events to it.
final class EngineViewController: UIViewController {
    private(set) var engine: OpaquePointer?
    let canvasView = MetalCanvasView()
    let keyInput = KeyInputView(frame: .zero)
    private(set) lazy var services = HostServices(controller: self)

    private var displayLink: CADisplayLink?
    private var callbacks = CCHostCallbacks()
    private var pendingOpen: [URL] = []
    private var startFailed = false
    private var lastSize: (UInt32, UInt32, Float, UIEdgeInsets)?
    private var keyRepeat: KeyRepeat?

    override var prefersStatusBarHidden: Bool { true }
    override var prefersHomeIndicatorAutoHidden: Bool { true }
    override var preferredScreenEdgesDeferringSystemGestures: UIRectEdge { .all }
    override var canBecomeFirstResponder: Bool { true }

    override func loadView() {
        view = canvasView
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        canvasView.engine = { [weak self] in self?.engine }
        keyInput.engine = { [weak self] in self?.engine }
        keyInput.presses = { [weak self] presses, down in self?.handlePresses(presses, down: down) ?? presses }
        keyInput.isUserInteractionEnabled = false
        canvasView.addSubview(keyInput)
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardFrameChanged(_:)), name: UIResponder.keyboardWillChangeFrameNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardFrameChanged(_:)), name: UIResponder.keyboardWillHideNotification, object: nil)
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        becomeFirstResponder()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        if engine == nil {
            start()
        } else {
            resize()
        }
    }

    override func didReceiveMemoryWarning() {
        super.didReceiveMemoryWarning()
        if let engine { cc_memory_warning(engine) }
    }

    // MARK: Engine lifecycle

    private var scale: CGFloat {
        view.window?.windowScene?.screen.scale ?? traitCollection.displayScale
    }

    private func pixelSize() -> (UInt32, UInt32) {
        let s = scale
        return (UInt32(max(1, (canvasView.bounds.width * s).rounded())), UInt32(max(1, (canvasView.bounds.height * s).rounded())))
    }

    private func start() {
        guard !startFailed, canvasView.bounds.width > 0, canvasView.bounds.height > 0 else { return }
        let s = scale
        canvasView.contentScaleFactor = s
        canvasView.metalLayer.contentsScale = s
        let (w, h) = pixelSize()
        canvasView.metalLayer.drawableSize = CGSize(width: CGFloat(w), height: CGFloat(h))

        // The engine sizes its image caches from RAM; on iOS that's what this process may use
        // before the system ends it, not the device's total.
        let available = os_proc_available_memory()
        let ramMB = available > 0 ? available >> 20 : Int(ProcessInfo.processInfo.physicalMemory >> 21)
        setenv("PHOTOCRAFT_RAM_MB", String(ramMB), 0)

        callbacks = services.callbacks(userData: Unmanaged.passUnretained(self).toOpaque())
        let config = Self.directory(.applicationSupportDirectory).appendingPathComponent("CarterCanvas", isDirectory: true)
        let documents = Self.directory(.documentDirectory)
        let layer = Unmanaged.passUnretained(canvasView.metalLayer).toOpaque()
        engine = withUnsafePointer(to: &callbacks) { table in
            cc_create(layer, w, h, Float(s), config.path, documents.path, table)
        }
        guard let engine else {
            startFailed = true
            showStartFailure(log: config.appendingPathComponent("Logs/cartercanvas.log"))
            return
        }
        resize(force: true)
        let link = CADisplayLink(target: DisplayLinkProxy(self), selector: #selector(DisplayLinkProxy.tick(_:)))
        link.preferredFrameRateRange = CAFrameRateRange(minimum: 30, maximum: 120, preferred: 120)
        link.add(to: .main, forMode: .common)
        displayLink = link
        if !pendingOpen.isEmpty {
            open(urls: pendingOpen)
            pendingOpen.removeAll()
        }
        cc_set_focused(engine, UIApplication.shared.applicationState == .active)
    }

    private static func directory(_ kind: FileManager.SearchPathDirectory) -> URL {
        let url = FileManager.default.urls(for: kind, in: .userDomainMask).first ?? URL(fileURLWithPath: NSTemporaryDirectory())
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private var keyboardOverlap: CGFloat = 0

    private func resize(force: Bool = false) {
        guard let engine else { return }
        let s = scale
        let (w, h) = pixelSize()
        var insets = canvasView.safeAreaInsets
        insets.bottom = max(insets.bottom, keyboardOverlap)
        if !force, let last = lastSize, last.0 == w, last.1 == h, last.2 == Float(s), last.3 == insets { return }
        lastSize = (w, h, Float(s), insets)
        canvasView.contentScaleFactor = s
        canvasView.metalLayer.contentsScale = s
        cc_resize(engine, w, h, Float(s), Float(insets.top), Float(insets.left), Float(insets.bottom), Float(insets.right))
    }

    fileprivate func tick(_ link: CADisplayLink) {
        guard let engine else { return }
        _ = cc_frame(engine, CACurrentMediaTime())
    }

    func setFocused(_ focused: Bool) {
        if let engine { cc_set_focused(engine, focused) }
    }

    func enterBackground() {
        if let engine { cc_background(engine) }
    }

    func shutdown() {
        displayLink?.invalidate()
        displayLink = nil
        if let engine {
            cc_destroy(engine)
            self.engine = nil
        }
        services.stopAccessingAll()
    }

    /// Files from the Files app, share sheet or "Open in".
    func open(urls: [URL]) {
        guard !urls.isEmpty else { return }
        guard let engine else {
            pendingOpen.append(contentsOf: urls)
            return
        }
        let paths = urls.compactMap { services.access($0)?.path }
        guard !paths.isEmpty, let json = try? JSONSerialization.data(withJSONObject: paths), let text = String(data: json, encoding: .utf8) else { return }
        cc_open_paths(engine, text)
    }

    private func showStartFailure(log: URL) {
        let detail = (try? String(contentsOf: log, encoding: .utf8))?.split(separator: "\n").suffix(6).joined(separator: "\n") ?? ""
        let alert = UIAlertController(title: "CarterCanvas couldn't start", message: "The graphics engine failed to start on this device.\n\n\(detail)", preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: "OK", style: .default))
        DispatchQueue.main.async { [weak self] in self?.present(alert, animated: true) }
    }

    // MARK: Keyboard

    func setKeyboardVisible(_ visible: Bool) {
        if visible {
            if !keyInput.isFirstResponder { keyInput.becomeFirstResponder() }
        } else if keyInput.isFirstResponder {
            becomeFirstResponder()
        }
    }

    @objc private func keyboardFrameChanged(_ note: Notification) {
        var overlap: CGFloat = 0
        if note.name != UIResponder.keyboardWillHideNotification,
           let frame = (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue {
            let local = canvasView.convert(frame, from: nil)
            overlap = max(0, canvasView.bounds.maxY - local.minY)
        }
        if overlap != keyboardOverlap {
            keyboardOverlap = overlap
            resize()
        }
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let unhandled = handlePresses(presses, down: true)
        if !unhandled.isEmpty { super.pressesBegan(unhandled, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let unhandled = handlePresses(presses, down: false)
        if !unhandled.isEmpty { super.pressesEnded(unhandled, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let unhandled = handlePresses(presses, down: false)
        if !unhandled.isEmpty { super.pressesCancelled(unhandled, with: event) }
    }

    /// Sends hardware keys to the engine; returns the presses it didn't use, for UIKit. A used
    /// press never reaches UIKit's text input, so a key types once even while the on-screen
    /// keyboard's responder is active.
    func handlePresses(_ presses: Set<UIPress>, down: Bool) -> Set<UIPress> {
        var unhandled = Set<UIPress>()
        for press in presses {
            guard let key = press.key, let engine else {
                unhandled.insert(press)
                continue
            }
            let usage = UInt32(truncatingIfNeeded: key.keyCode.rawValue)
            let flags = UInt32(truncatingIfNeeded: key.modifierFlags.rawValue)
            if !down, keyRepeat?.usage == usage { stopRepeat() }
            if cc_key(engine, usage, down, flags, false, down ? key.characters : nil) {
                if down { startRepeat(usage: usage, flags: flags, text: key.characters) }
            } else {
                cc_modifiers(engine, flags)
                unhandled.insert(press)
            }
        }
        return unhandled
    }

    /// UIKit doesn't repeat held keys for `pressesBegan`; arrows, Delete and brush-size keys
    /// repeat like on a Mac.
    private func startRepeat(usage: UInt32, flags: UInt32, text: String) {
        stopRepeat()
        let modifierUsages: ClosedRange<UInt32> = 0xE0...0xE7
        guard !modifierUsages.contains(usage) else { return }
        let r = KeyRepeat(usage: usage, flags: flags, text: text)
        r.timer = Timer.scheduledTimer(withTimeInterval: 0.42, repeats: false) { [weak self] _ in
            guard let self, self.keyRepeat === r else { return }
            r.timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30.0, repeats: true) { [weak self] _ in
                guard let self, let engine = self.engine, self.keyRepeat === r else { return }
                _ = cc_key(engine, r.usage, true, r.flags, true, r.text)
            }
        }
        keyRepeat = r
    }

    private func stopRepeat() {
        keyRepeat?.timer?.invalidate()
        keyRepeat = nil
    }
}

private final class KeyRepeat {
    let usage: UInt32
    let flags: UInt32
    let text: String
    var timer: Timer?

    init(usage: UInt32, flags: UInt32, text: String) {
        self.usage = usage
        self.flags = flags
        self.text = text
    }
}

/// Breaks the display link's strong reference to its target.
private final class DisplayLinkProxy {
    weak var controller: EngineViewController?

    init(_ controller: EngineViewController) {
        self.controller = controller
    }

    @objc func tick(_ link: CADisplayLink) {
        guard let controller else {
            link.invalidate()
            return
        }
        controller.tick(link)
    }
}
