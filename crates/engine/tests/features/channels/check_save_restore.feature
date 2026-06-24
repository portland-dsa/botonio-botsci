@channels @snapshots
Feature: Tails inspects and rolls back channel permissions

  Around the lockdown pass, Tails has read-only and reversible tools: a check
  that flags channels whose permissions have drifted out of sync with their
  category, a save that snapshots every channel's current overwrites, and a
  restore that puts those overwrites back exactly.

  @check
  Scenario: Check flags a child channel out of sync with its category
    Given a category "staff" and a child channel "staff-chat" with different overwrites
    When Tails checks whether channels are synchronized
    Then "staff-chat" is reported as out of sync with its category
    And no channel is written

  @check
  Scenario: Check passes when children match their category
    Given a category "staff" and a child channel "staff-chat" with matching overwrites
    When Tails checks whether channels are synchronized
    Then no channel is reported as out of sync

  @save
  Scenario: Save snapshots every channel's overwrites
    Given the guild has channels "general" and "staff-chat" with overwrites
    When Tails saves a snapshot
    Then the snapshot records the overwrites of "general" and "staff-chat"

  @restore
  Scenario: Restore writes the snapshot's overwrites back
    Given a snapshot recording the overwrites for "general"
    When Tails restores from the snapshot
    Then "general" is written back to its snapshot overwrites
