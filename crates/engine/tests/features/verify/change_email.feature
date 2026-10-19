Feature: Changing a member's email
  A member who signed up again has a second record under a new email. A moderator moves
  their Discord link onto that record so it decides their role from then on.

  Scenario: The link moves from the old record to the new one
    Given Amy is in our records linked to her Discord id
    And Amy has a new record under amy.rose@b.test
    When Sonic changes Amy's email to amy.rose@b.test
    Then the email change is reported as relinked
    And Amy is assigned the Member role
    And Amy's new record carries her Discord identity
    And Amy's old record no longer carries her Discord identity
    And Amy's new record is linked in the cache
    And the relinked email change is recorded in the audit log

  Scenario: A new record that already carries her id still takes the link from the old one
    Given Amy is in our records linked to her Discord id
    And Amy has a new record under amy.rose@b.test that already carries her Discord id
    When Sonic changes Amy's email to amy.rose@b.test
    Then the email change is reported as relinked
    And Amy's old record no longer carries her Discord identity

  Scenario: A new record with no membership status is linked but grants no role
    Given Amy is in our records linked to her Discord id
    And Amy has a new record under amy.rose@b.test with no membership status
    When Sonic changes Amy's email to amy.rose@b.test
    Then the email change is reported as malformed
    And Amy's new record carries her Discord identity
    And Amy is assigned no role

  Scenario: A failure part-way leaves her linked to both records, never neither
    Given Amy is in our records linked to her Discord id
    And Amy has a new record under amy.rose@b.test
    And clearing an old link is failing
    When Sonic changes Amy's email to amy.rose@b.test
    Then the email change fails with an error
    And Amy's new record carries her Discord identity

  Scenario Outline: An email change that cannot apply changes nothing
    Given <situation>
    When Sonic changes <member>'s email to <email>
    Then the email change is reported as <outcome>
    And nothing is written back to our records
    And the <outcome> email change is recorded in the audit log

    Examples:
      | situation                                            | member | email         | outcome        |
      | Eggman's handle is on record for a different account | Eggman | eggman@b.test | conflict       |
      | Shadow is not in our records                         | Shadow | shadow@b.test | not_found      |
      | Amy is in our records linked to her Discord id       | Amy    | amy@b.test    | already_linked |
