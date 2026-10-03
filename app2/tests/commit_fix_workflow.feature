Feature: Commit-fix workflow

  Scenario: Successful commit completes the workflow
    Given a running commit-fix workflow
    When git status succeeds
    And git diff succeeds
    And git commit succeeds
    Then the workflow is complete

  Scenario: Failed commit keeps the workflow active
    Given a running commit-fix workflow
    When git status succeeds
    And git diff succeeds
    And git commit fails
    Then the workflow remains active
