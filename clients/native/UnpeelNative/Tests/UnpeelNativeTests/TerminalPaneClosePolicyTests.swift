import XCTest
@testable import UnpeelNative

final class TerminalPaneClosePolicyTests: XCTestCase {
    func testLauncherDetachesImmediately() {
        XCTAssertEqual(
            terminalPaneCloseAction(
                for: .launcher(projectID: "project"),
                canStopSession: false,
                isStoppedResumable: false
            ),
            .detachPane
        )
    }

    func testPlainTerminalRemovesImmediately() {
        XCTAssertEqual(
            terminalPaneCloseAction(
                for: .session(id: "shell"),
                canStopSession: false,
                isStoppedResumable: false
            ),
            .removeSession("shell")
        )
    }

    func testLiveResumableAgentRequiresStopConfirmation() {
        XCTAssertEqual(
            terminalPaneCloseAction(
                for: .session(id: "agent"),
                canStopSession: true,
                isStoppedResumable: false
            ),
            .confirmStop("agent")
        )
    }

    func testStoppedResumableAgentRequiresRemoveConfirmation() {
        XCTAssertEqual(
            terminalPaneCloseAction(
                for: .session(id: "agent"),
                canStopSession: false,
                isStoppedResumable: true
            ),
            .confirmRemove("agent")
        )
    }
}
