import CarterCanvasEngine
import UIKit

/// The view the engine draws into: a `CAMetalLayer` that forwards touches, Apple Pencil samples,
/// trackpad/mouse pointer, scrolling and pinching to the engine.
final class MetalCanvasView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }

    var metalLayer: CAMetalLayer {
        // `layerClass` makes this always a CAMetalLayer.
        layer as! CAMetalLayer // swiftlint:disable:this force_cast
    }

    /// The engine handle, once it exists.
    var engine: () -> OpaquePointer? = { nil }

    private var lastPinchScale: CGFloat = 1

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        isOpaque = true
        backgroundColor = Brand.canvas
        metalLayer.isOpaque = true
        metalLayer.framebufferOnly = false
        metalLayer.presentsWithTransaction = false

        let hover = UIHoverGestureRecognizer(target: self, action: #selector(hovered(_:)))
        addGestureRecognizer(hover)

        // Trackpad and mouse-wheel scrolling only: direct touches stay ours.
        let scroll = UIPanGestureRecognizer(target: self, action: #selector(scrolled(_:)))
        scroll.allowedScrollTypesMask = .all
        scroll.allowedTouchTypes = []
        scroll.cancelsTouchesInView = false
        addGestureRecognizer(scroll)

        let pinch = UIPinchGestureRecognizer(target: self, action: #selector(pinched(_:)))
        pinch.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.indirectPointer.rawValue)]
        pinch.cancelsTouchesInView = false
        addGestureRecognizer(pinch)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    // MARK: Touches and Apple Pencil

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        forward(touches, event, phase: UInt32(CC_TOUCH_BEGAN))
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        forward(touches, event, phase: UInt32(CC_TOUCH_MOVED))
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        forward(touches, event, phase: UInt32(CC_TOUCH_ENDED))
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        forward(touches, event, phase: UInt32(CC_TOUCH_CANCELLED))
    }

    private func touchID(_ touch: UITouch) -> UInt64 {
        UInt64(UInt(bitPattern: ObjectIdentifier(touch).hashValue))
    }

    private func sample(_ touch: UITouch, id: UInt64, phase: UInt32) -> CCTouch {
        let p = touch.location(in: self)
        var t = CCTouch()
        t.id = id
        t.phase = phase
        t.x = Float(p.x)
        t.y = Float(p.y)
        t.timestamp = touch.timestamp
        if touch.type == .pencil {
            t.kind = UInt32(CC_TOUCH_PENCIL)
            let maxForce = touch.maximumPossibleForce
            t.pressure = maxForce > 0 ? Float(min(max(touch.force / maxForce, 0), 1)) : 0.5
            t.altitude = Float(touch.altitudeAngle)
            t.azimuth = Float(touch.azimuthAngle(in: self))
            if #available(iOS 17.5, *) {
                t.roll = Float(touch.rollAngle)
            }
        } else {
            t.kind = UInt32(CC_TOUCH_FINGER)
            t.pressure = 1
            t.altitude = .pi / 2
        }
        return t
    }

    private func forward(_ touches: Set<UITouch>, _ event: UIEvent?, phase: UInt32) {
        guard let engine = engine() else { return }
        var samples: [CCTouch] = []
        samples.reserveCapacity(touches.count * 4)
        for touch in touches.sorted(by: { $0.timestamp < $1.timestamp }) {
            if touch.type == .indirectPointer {
                pointer(touch, event, phase: phase, engine: engine)
                continue
            }
            let id = touchID(touch)
            // Every sample since the last event, so fast strokes keep their shape and pressure.
            let coalesced = phase == UInt32(CC_TOUCH_MOVED) ? (event?.coalescedTouches(for: touch) ?? []) : []
            if coalesced.isEmpty {
                samples.append(sample(touch, id: id, phase: phase))
            } else {
                samples.append(contentsOf: coalesced.map { sample($0, id: id, phase: phase) })
            }
        }
        if !samples.isEmpty {
            samples.withUnsafeBufferPointer { cc_touches(engine, $0.baseAddress, $0.count) }
        }
    }

    /// A trackpad or mouse click/drag: a pointer with buttons, no touch delay.
    private func pointer(_ touch: UITouch, _ event: UIEvent?, phase: UInt32, engine: OpaquePointer) {
        let p = touch.location(in: self)
        let button: UInt32 = event?.buttonMask.contains(.secondary) == true ? 1 : 0
        switch Int(phase) {
        case Int(CC_TOUCH_BEGAN):
            cc_pointer_button(engine, Float(p.x), Float(p.y), button, true)
        case Int(CC_TOUCH_MOVED):
            cc_hover(engine, Float(p.x), Float(p.y), true)
        default:
            cc_pointer_button(engine, Float(p.x), Float(p.y), button, false)
        }
    }

    // MARK: Pointer hover, scrolling, pinching

    @objc private func hovered(_ g: UIHoverGestureRecognizer) {
        guard let engine = engine() else { return }
        let p = g.location(in: self)
        let active = g.state == .began || g.state == .changed
        cc_hover(engine, Float(p.x), Float(p.y), active)
    }

    @objc private func scrolled(_ g: UIPanGestureRecognizer) {
        guard let engine = engine() else { return }
        let d = g.translation(in: self)
        g.setTranslation(.zero, in: self)
        if d != .zero {
            cc_scroll(engine, Float(d.x), Float(d.y))
        }
    }

    @objc private func pinched(_ g: UIPinchGestureRecognizer) {
        guard let engine = engine() else { return }
        switch g.state {
        case .began:
            lastPinchScale = 1
        case .changed:
            let p = g.location(in: self)
            let factor = g.scale / max(lastPinchScale, 0.0001)
            lastPinchScale = g.scale
            cc_pinch(engine, Float(p.x), Float(p.y), Float(factor))
        default:
            lastPinchScale = 1
        }
    }
}

/// Takes the on-screen keyboard's text while a text field in the engine has focus.
final class KeyInputView: UIView, UIKeyInput {
    var engine: () -> OpaquePointer? = { nil }
    /// Hardware keys go to the engine first; returns the presses it didn't use.
    var presses: (Set<UIPress>, Bool) -> Set<UIPress> = { p, _ in p }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let rest = self.presses(presses, true)
        if !rest.isEmpty { super.pressesBegan(rest, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let rest = self.presses(presses, false)
        if !rest.isEmpty { super.pressesEnded(rest, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        let rest = self.presses(presses, false)
        if !rest.isEmpty { super.pressesCancelled(rest, with: event) }
    }

    var hasText: Bool { true }
    override var canBecomeFirstResponder: Bool { true }

    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
    var keyboardType: UIKeyboardType = .default
    var keyboardAppearance: UIKeyboardAppearance = .dark
    var returnKeyType: UIReturnKeyType = .done

    func insertText(_ text: String) {
        guard let engine = engine() else { return }
        cc_text(engine, text)
    }

    func deleteBackward() {
        guard let engine = engine() else { return }
        cc_delete_backward(engine)
    }
}
