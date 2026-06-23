Feature: A matched record with no usable standing is left for hand-override

  Scenario: Sonic verifies Rouge, whose record has no membership status
    Given Rouge is in our records linked to her Discord id, but her record has no membership status
    When Sonic verifies Rouge
    Then Rouge's record is reported as malformed
    And the malformed encounter is recorded in the audit log with method discord

  Scenario: Sonic verifies Rouge by email, and the record is still malformed
    Given Rouge is in our records linked to her Discord id, but her record has no membership status
    When Sonic verifies Rouge by email
    Then Rouge's record is reported as malformed
    And the malformed encounter is recorded in the audit log with method email
