Feature: The scheduled scan plans role changes and guards against mass demotion

  Scenario: A mix of promotion and demotion proceeds
    Given Sonic is in the roster holding no role, known to us as a Member
    And Tails is in the roster holding the Member role, known to us as Dues Expired
    When the scan plans a pass
    Then the scan would change 2 members
    And the scan counts 1 demotion
    And the scan proceeds

  Scenario: An empty cache would demote everyone and aborts
    Given 10 members hold the Member role, none known to us
    When the scan plans a pass
    Then the scan scans 10 members
    And the scan counts 10 demotions
    And the scan counts 10 misses
    And the scan aborts

  Scenario: Demotions below the floor still proceed on a tiny guild
    Given 2 members hold a managed role, none known to us
    When the scan plans a pass
    Then the scan counts 2 demotions
    And the scan proceeds

  Scenario: A handle conflict is skipped, not demoted
    Given Ghost is bound in the cache to a different account but holds the Member role
    When the scan plans a pass
    Then the scan counts 1 conflict
    And the scan counts 0 demotions
    And the scan would change 0 members
    And the scan proceeds

  Scenario: A malformed record is left untouched and never counts as a demotion
    Given Rouge is in the roster holding the Member role, known to us but with no membership status
    When the scan plans a pass
    Then the scan counts 1 malformed record
    And the scan counts 0 demotions
    And the scan would change 0 members
    And the scan proceeds
