Feature: ACP session lifecycle

  Scenario: A new session forwards MCP servers and scopes notifications
    Given a lorem-agent connection without session deletion support
    When I create a session with MCP server "integration-test-server"
    And I prompt the session
    Then registered MCP tools mention "integration-test-server"
    And every notification belongs to the created session
    And the connection is disconnected

  Scenario: A session can be closed and deleted when supported
    Given a lorem-agent connection with session deletion support
    When I create a session
    And I close and delete the session
    Then the session is deleted
    And the connection is disconnected

  Scenario: Delete is rejected when the agent does not advertise support
    Given a lorem-agent connection without session deletion support
    When I create a session
    And I close the session
    And I try to delete the closed session
    Then deleting the session is rejected
    And the connection is disconnected

  Scenario: One connection can own multiple sessions
    Given a lorem-agent connection without session deletion support
    When I create two sessions
    Then the session IDs are distinct
    And the connection is disconnected
