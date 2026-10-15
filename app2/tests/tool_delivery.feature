Feature: Tool event delivery

  Scenario: A tool call reaches the owning session with its input
    Given a lorem-agent connection
    And a routed session "alpha"
    When I prompt session "alpha"
    Then session "alpha" received a call of tool "generate_lorem_ipsum" with its input

  Scenario: A completed tool is delivered as a successful result
    Given a lorem-agent connection
    And a routed session "alpha"
    When I prompt session "alpha"
    Then session "alpha" received a successful result of tool "generate_lorem_ipsum"

  Scenario: A failed tool is delivered as a failed result
    Given a lorem-agent connection whose first tool call fails
    And a routed session "alpha"
    When I prompt session "alpha"
    Then session "alpha" received a failed result of tool "generate_lorem_ipsum"

  Scenario: Updates are routed per session
    Given a lorem-agent connection
    And a routed session "alpha"
    And a routed session "beta"
    When I prompt session "alpha"
    Then session "alpha" received a successful result of tool "generate_lorem_ipsum"
    And session "beta" received no tool events
