Feature: Bulk resync per member

  # Migrated from resync_tests in member.rs

  Scenario: Resyncing a member who already holds their correct role writes nothing
    Given Tails is in our records by handle with no Discord id
    And Tails also holds the Member role
    When Sonic resyncs Tails
    Then Tails is left unchanged at Member
    And no audit row is written for the resync

  Scenario: Resyncing a member whose role differs applies the correct role
    Given Tails is in our records by handle with no Discord id
    When Sonic resyncs Tails
    Then Tails is resynced to Member
    And the resync is audited once

  Scenario: Resyncing a malformed record assigns nothing and flags it
    Given Rouge is in our records linked to her Discord id, but her record has no membership status
    When Sonic resyncs Rouge
    Then Rouge's resync is reported as malformed
