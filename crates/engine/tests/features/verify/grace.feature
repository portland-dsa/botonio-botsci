Feature: Grace override holds a member at Member regardless of lapsed dues

  Scenario: Rouge has lapsed dues but an active grace - she verifies as Member
    Given Rouge is in our records linked to her Discord id, and her dues have lapsed
    And Rouge has an active grace override
    When Sonic verifies Rouge
    Then Rouge is assigned the Member role
    And the verification is recorded in the audit log

  Scenario: Rouge has lapsed dues and her grace has expired - she verifies as Dues Expired
    Given Rouge is in our records linked to her Discord id, and her dues have lapsed
    And Rouge's grace override has expired
    When Sonic verifies Rouge
    Then Rouge is assigned the DuesExpired role
    And the verification is recorded in the audit log
