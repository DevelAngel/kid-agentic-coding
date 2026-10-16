Feature: Commit-fix workflow

  Scenario: Entry tool starts the commit-fix workflow
    Given a running programming workflow
    When the agent invokes the commit-fix entry tool with tldr, why, and what
    Then a commit-fix workflow starts
    And its prompt contains tldr, why, and what

  Scenario: Invalid entry parameters are rejected
    Given a running programming workflow
    When the agent invokes the commit-fix entry tool without why
    Then no transition happens
    And the failure is reported

  Scenario: Successful completion resumes the preserved caller
    Given a running commit-fix workflow
    When git commit succeeds
    Then the workflow is complete
    And the caller resumes with the result

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
