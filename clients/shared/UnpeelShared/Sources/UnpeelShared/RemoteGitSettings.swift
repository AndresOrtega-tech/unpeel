import Foundation

/// Metadata only. GitHub credentials remain in the selected Host's gh store.
public struct RemoteGitSettings: Codable, Equatable, Sendable {
    public let checking: Bool
    public let gitInstalled: Bool
    public let githubCliInstalled: Bool
    public let githubCliInstallCommand: String?
    public let name: String
    public let email: String
    public let github: GitHubAccount
    public let operation: Operation?
    public let error: String?

    public struct GitHubAccount: Codable, Equatable, Sendable {
        public let state: String
        public let login: String?
        public let environmentToken: Bool
        public let credentialStorage: String?
        public let gitConfigured: Bool
    }

    public struct Operation: Codable, Equatable, Sendable {
        public let id: String
        public let action: String
        public let state: String
        public let userCode: String?
        public let verificationURL: String?
        public let error: String?

        enum CodingKeys: String, CodingKey {
            case id, action, state, userCode, error
            case verificationURL = "verificationUrl"
        }

        public var isRunning: Bool { state == "running" || state == "awaitingAuthorization" }

        /// Device authorization only. Never open arbitrary URLs supplied by
        /// terminal output or a different Host's stale response.
        public var githubAuthorizationURL: URL? {
            guard isRunning, action == "connectGithub",
                  verificationURL == "https://github.com/login/device",
                  let userCode, userCode.utf8.count == 9,
                  userCode.utf8.enumerated().allSatisfy({ index, byte in
                      index == 4 ? byte == 45 : (65...90).contains(byte) || (48...57).contains(byte)
                  }) else { return nil }
            return URL(string: "https://github.com/login/device")
        }
    }
}

public struct RemoteGitSettingsAction: Encodable, Sendable {
    public let action: String
    public let requestID: String
    public let operationID: String?
    public let login: String?
    public let name: String?
    public let email: String?

    public init(action: String, requestID: String = UUID().uuidString.lowercased(), operationID: String? = nil,
                login: String? = nil, name: String? = nil, email: String? = nil) {
        self.action = action
        self.requestID = requestID
        self.operationID = operationID
        self.login = login
        self.name = name
        self.email = email
    }
}
