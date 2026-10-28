import UIKit

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        true
    }

    func application(_ application: UIApplication, configurationForConnecting connectingSceneSession: UISceneSession, options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let config = UISceneConfiguration(name: "Default", sessionRole: connectingSceneSession.role)
        config.delegateClass = SceneDelegate.self
        return config
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private var engine: EngineViewController?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options connectionOptions: UIScene.ConnectionOptions) {
        guard let windowScene = scene as? UIWindowScene else { return }
        let controller = EngineViewController()
        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = controller
        window.backgroundColor = Brand.canvas
        window.makeKeyAndVisible()
        self.window = window
        engine = controller
        controller.open(urls: connectionOptions.urlContexts.map(\.url))
    }

    func scene(_ scene: UIScene, openURLContexts contexts: Set<UIOpenURLContext>) {
        engine?.open(urls: contexts.map(\.url))
    }

    func sceneDidBecomeActive(_ scene: UIScene) {
        engine?.setFocused(true)
    }

    func sceneWillResignActive(_ scene: UIScene) {
        engine?.setFocused(false)
    }

    func sceneDidEnterBackground(_ scene: UIScene) {
        engine?.enterBackground()
    }

    func sceneDidDisconnect(_ scene: UIScene) {
        engine?.shutdown()
        engine = nil
    }
}

/// carterware brand colors.
enum Brand {
    static let accent = UIColor(red: 0xE4 / 255, green: 0x00 / 255, blue: 0x2B / 255, alpha: 1)
    static let canvas = UIColor(red: 0x09 / 255, green: 0x04 / 255, blue: 0x06 / 255, alpha: 1)
    static let text = UIColor(red: 0xFF / 255, green: 0xF5 / 255, blue: 0xF6 / 255, alpha: 1)
}
