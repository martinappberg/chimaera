// WKWebView scroll harness: load a URL, run a setup JS file (it sets
// window.__ready = true when the page is prepared), then feed REAL wheel
// events (trackpad-style gesture phases, optional momentum tail) through
// WKWebView.scrollWheel(with:) — WebKit's async scrolling path, the same one a
// trackpad drives — and finally print window.__out after an analyze JS file.
//
// usage: wkscroll <url> <setup.js> <analyze.js> <scenario> [x y]
//   scenario: comma list of  up:<steps>:<px> | down:<steps>:<px> |
//             flingup:<px> | flingdown:<px> | wait:<ms> | js:<expr>
import AppKit
import CoreGraphics
import WebKit

let args = CommandLine.arguments
let url = URL(string: args[1])!
let setup = try! String(contentsOfFile: args[2], encoding: .utf8)
let analyze = try! String(contentsOfFile: args[3], encoding: .utf8)
let scenario = args[4].split(separator: ",").map(String.init)
let px = args.count > 6 ? Double(args[5])! : 520
let py = args.count > 6 ? Double(args[6])! : 350

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let window = NSWindow(
  contentRect: NSRect(x: 40, y: 40, width: 1000, height: 760),
  styleMask: [.titled], backing: .buffered, defer: false)
window.level = .floating
let webView = WKWebView(frame: window.contentView!.bounds, configuration: WKWebViewConfiguration())
webView.autoresizingMask = [.width, .height]
window.contentView!.addSubview(webView)
window.orderFrontRegardless()

func wheel(_ dy: Double, phase: CGScrollPhase?, momentum: CGMomentumScrollPhase) {
  guard
    let ev = CGEvent(
      scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1,
      wheel1: Int32(dy.rounded()), wheel2: 0, wheel3: 0)
  else { return }
  ev.setDoubleValueField(.scrollWheelEventPointDeltaAxis1, value: dy)
  ev.setDoubleValueField(.scrollWheelEventFixedPtDeltaAxis1, value: dy)
  ev.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
  ev.setIntegerValueField(.scrollWheelEventScrollPhase, value: Int64(phase?.rawValue ?? 0))
  ev.setIntegerValueField(.scrollWheelEventMomentumPhase, value: Int64(momentum.rawValue))
  // Location in window coordinates (bottom-left origin) → screen (CG, top-left).
  let inWindow = NSPoint(x: px, y: window.contentView!.bounds.height - py)
  let onScreen = window.convertPoint(toScreen: inWindow)
  let screenH = NSScreen.screens[0].frame.height
  ev.location = CGPoint(x: onScreen.x, y: screenH - onScreen.y)
  ev.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window.windowNumber))
  ev.setIntegerValueField(
    .mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(window.windowNumber))
  guard let ns = NSEvent(cgEvent: ev) else { return }
  webView.scrollWheel(with: ns)
}

var steps: [(Double, () -> Void)] = []  // (delay seconds before, action)
func add(_ delay: Double, _ action: @escaping () -> Void) { steps.append((delay, action)) }

for item in scenario {
  let p = item.split(separator: ":").map(String.init)
  switch p[0] {
  case "up", "down":
    // A trackpad drag: began, changed×n, ended. Positive delta = content down (scroll up).
    let n = Int(p[1])!, d = Double(p[2])! * (p[0] == "up" ? 1 : -1)
    add(0.016) { wheel(0, phase: .began, momentum: .none) }
    for _ in 0..<n { add(0.016) { wheel(d, phase: .changed, momentum: .none) } }
    add(0.016) { wheel(0, phase: .ended, momentum: .none) }
  case "flingup", "flingdown":
    // A flick: short drag, then a decaying momentum tail (~1.5 s).
    let v0 = Double(p[1])! * (p[0] == "flingup" ? 1 : -1)
    add(0.016) { wheel(0, phase: .began, momentum: .none) }
    for _ in 0..<4 { add(0.016) { wheel(v0, phase: .changed, momentum: .none) } }
    add(0.016) { wheel(0, phase: .ended, momentum: .none) }
    add(0.016) { wheel(v0, phase: nil, momentum: .begin) }
    var v = v0
    while abs(v) > 1 {
      v *= 0.95
      let dv = v
      add(0.016) { wheel(dv, phase: nil, momentum: .continuous) }
    }
    add(0.016) { wheel(0, phase: nil, momentum: .end) }
  case "wait":
    add(Double(p[1])! / 1000) {}
  case "js":
    let expr = p.dropFirst().joined(separator: ":")
    add(0.016) { webView.evaluateJavaScript(expr) { _, _ in } }
  default:
    print("bad scenario item \(item)")
    exit(2)
  }
}

func run(_ i: Int) {
  if i >= steps.count {
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
      webView.evaluateJavaScript(analyze) { _, _ in }
      pollOut()
    }
    return
  }
  let (delay, action) = steps[i]
  DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
    action()
    run(i + 1)
  }
}

func pollOut() {
  webView.evaluateJavaScript("window.__out ?? null") { result, _ in
    if let s = result as? String {
      print(s)
      exit(0)
    }
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { pollOut() }
  }
}

func pollReady() {
  webView.evaluateJavaScript("window.__ready === true") { result, _ in
    if (result as? Bool) == true {
      run(0)
      return
    }
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { pollReady() }
  }
}

class Delegate: NSObject, WKNavigationDelegate {
  var started = false
  func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
    if started { return }
    started = true
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
      webView.evaluateJavaScript(setup) { _, _ in }
      pollReady()
    }
  }
}
let delegate = Delegate()
webView.navigationDelegate = delegate
webView.load(URLRequest(url: url))
DispatchQueue.main.asyncAfter(deadline: .now() + 120) {
  webView.evaluateJavaScript("JSON.stringify({timeout:true, log: (window.__diag||[]).slice(-20)})") { r, _ in
    print(r ?? "timeout")
    exit(1)
  }
}
app.run()
