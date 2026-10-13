import Foundation
import Testing
@testable import UnpeelShared

struct RemoteGitSettingsTests {
    @Test func deviceAuthorizationOnlyOpensTheExpectedGitHubPage() throws {
        for url in ["https://github.com/login/device", "https://evil.example/login", "http://github.com/login/device", "https://github.com/login/device?redirect=evil"] {
            let data = try JSONSerialization.data(withJSONObject: [
                "id": "attempt", "action": "connectGithub", "state": "awaitingAuthorization",
                "userCode": "ABCD-1234", "verificationUrl": url,
            ])
            let operation = try JSONDecoder().decode(RemoteGitSettings.Operation.self, from: data)
            #expect((operation.githubAuthorizationURL != nil) == (url == "https://github.com/login/device"))
        }
    }

    @Test func completedAndMalformedChallengesCannotOpenTheBrowser() throws {
        for (state, code) in [("complete", "ABCD-1234"), ("awaitingAuthorization", "not-a-code"), ("cancelled", "ABCD-1234")] {
            let data = try JSONSerialization.data(withJSONObject: [
                "id": "attempt", "action": "connectGithub", "state": state,
                "userCode": code, "verificationUrl": "https://github.com/login/device",
            ])
            let operation = try JSONDecoder().decode(RemoteGitSettings.Operation.self, from: data)
            #expect(operation.githubAuthorizationURL == nil)
        }
    }

    @Test func cancellationNamesTheOriginalLoginOperation() throws {
        let request = RemoteGitSettingsAction(action: "cancelGithub", operationID: "original-login")
        let wire = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
        #expect(wire["operationID"] as? String == "original-login")
        #expect(wire["requestID"] as? String != "original-login")
        #expect(wire["login"] == nil)
    }
}
