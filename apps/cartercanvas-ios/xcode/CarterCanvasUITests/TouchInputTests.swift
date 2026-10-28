import XCTest

/// Real touches through UIKit into the engine: strokes, multi-finger taps, pinch, and taps on the
/// interface, checked through the control channel.
final class TouchInputTests: XCTestCase {
    private static let port: UInt16 = 47611
    private static let token = "cc" + String(repeating: "1", count: 62)

    private var app: XCUIApplication!
    private var control: ControlClient!

    override func setUpWithError() throws {
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchEnvironment["CARTERCANVAS_CONTROL_PORT"] = "\(Self.port)"
        app.launchEnvironment["CARTERCANVAS_CONTROL_TOKEN"] = Self.token
        app.launch()
        control = try ControlClient.connect(port: Self.port, token: Self.token, timeout: 60)
        try control.run("file.new", ["width": 1600, "height": 1200, "background": "white"])
        try control.call("ui.set", ["tool": "brush", "brushSize": 30, "fit": true])
        // Let the fit and the first frames settle.
        Thread.sleep(forTimeInterval: 0.5)
    }

    override func tearDownWithError() throws {
        control?.close()
        app?.terminate()
    }

    private var window: XCUIElement { app.windows.firstMatch }

    private func point(_ x: CGFloat, _ y: CGFloat) -> XCUICoordinate {
        window.coordinate(withNormalizedOffset: CGVector(dx: x, dy: y))
    }

    private func document() throws -> [String: Any] {
        try control.run("document.inspect")
    }

    private func history() throws -> [String] {
        try document()["history"] as? [String] ?? []
    }

    /// Polls `condition` until it holds or `timeout` passes.
    private func eventually(_ what: String, timeout: TimeInterval = 5, _ condition: () throws -> Bool) throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if try condition() { return }
            Thread.sleep(forTimeInterval: 0.1)
        }
        XCTFail("timed out waiting for \(what)")
    }

    /// The middle of the canvas area (between the toolbox and the panel dock) in window units.
    private let canvasCenter: (CGFloat, CGFloat) = (0.36, 0.5)

    private func drawStroke() throws {
        let before = try history().count
        let (cx, cy) = canvasCenter
        point(cx - 0.15, cy - 0.04).press(forDuration: 0.2, thenDragTo: point(cx + 0.1, cy + 0.05))
        try eventually("the stroke's history step") { try history().count == before + 1 }
        XCTAssertEqual(try history().last, "Brush Tool")
    }

    func testFingerStrokePaints() throws {
        try drawStroke()
        XCTAssertEqual(try document()["canUndo"] as? Bool, true)
    }

    func testTwoFingerTapUndoesAndThreeFingerTapRedoes() throws {
        try drawStroke()
        window.tap(withNumberOfTaps: 1, numberOfTouches: 2)
        try eventually("undo") { try document()["canRedo"] as? Bool == true }
        window.tap(withNumberOfTaps: 1, numberOfTouches: 3)
        try eventually("redo") { try document()["canRedo"] as? Bool == false }
    }

    func testPinchZoomsTheCanvasWithoutPainting() throws {
        let before = try history().count
        let zoom = { () throws -> Double in
            let views = try self.control.call("ui.inspect")["views"] as? [[String: Any]]
            return views?.first?["zoom"] as? Double ?? 0
        }
        let z0 = try zoom()
        window.pinch(withScale: 2.5, velocity: 3)
        try eventually("the zoom to grow") { try zoom() > z0 * 1.3 }
        XCTAssertEqual(try history().count, before, "a pinch must not leave a brush stroke")
    }

    /// A finger tap on the toolbox picks a tool (a quick tap lifts before the finger delay, so
    /// its press and release arrive together).
    func testTapOnToolboxPicksTheEraser() throws {
        guard UIDevice.current.userInterfaceIdiom == .pad else { throw XCTSkip("the toolbox position is the iPad layout's") }
        point(0.0182, 0.347).tap()
        try eventually("the Eraser tool") { (try self.control.call("ui.inspect")["tool"] as? String) == "Eraser" }
    }
}
