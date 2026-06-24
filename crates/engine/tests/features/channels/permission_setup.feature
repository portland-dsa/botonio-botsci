@channels @permission_setup
Feature: Tails locks down channel permissions by role

  Tails runs permission-setup to make each public channel visible only to the
  right role. The pass classifies every channel into exactly one action, refuses
  outright if it would hide a verification channel from the Unverified role, and
  aborts before writing if the server drifted into a different set of writes than
  the confirmed preview.

  @classify
  Scenario: A public channel is swept to Member-only
    Given a public text channel named "general"
    And "general" is neither a dues-expired nor an unverified channel
    When Tails resolves the permission plan
    Then "general" is classified as Member-only

  @classify
  Scenario: A nominated dues-expired channel allows Dues Expired
    Given a public text channel named "dues-desk" nominated as a dues-expired channel
    When Tails resolves the permission plan
    Then "dues-desk" is classified as dues-expired-only

  @classify
  Scenario: A nominated unverified channel allows Unverified
    Given a public text channel named "welcome" nominated as an unverified channel
    When Tails resolves the permission plan
    Then "welcome" is classified as unverified-only

  @classify
  Scenario: An excluded channel is left untouched
    Given a public text channel named "rules" marked as excluded
    When Tails resolves the permission plan
    Then "rules" is classified as excluded

  @classify
  Scenario: A child channel is synced to its category's new permissions
    Given a category "staff" swept to Member-only
    And a child text channel "staff-chat" under "staff"
    When Tails resolves the permission plan
    Then "staff-chat" is classified as synced to its parent

  @classify
  Scenario: A private channel is left unchanged
    Given a private text channel named "secret"
    And "secret" is neither a dues-expired nor an unverified channel
    When Tails resolves the permission plan
    Then "secret" is classified as unchanged

  @classify
  Scenario: A nominated unverified channel that is already private still grants the Unverified role
    Given a private text channel named "welcome" nominated as an unverified channel
    When Tails resolves the permission plan
    Then "welcome" is classified as unverified-only
    And the Unverified role can view "welcome"

  @guard
  Scenario: The verification guard flags an unverified channel the plan would hide
    Given a frozen plan whose unverified channel is locked away from the Unverified role
    When Tails checks the verification guard
    Then the guard flags that unverified channel as a lock-out breach

  @guard
  Scenario: A resolved plan keeps every unverified channel viewable, raising no breach
    Given Tails resolves a plan that locks down a public channel and an unverified channel
    When Tails checks the verification guard
    Then the guard reports no lock-out breach

  @validation
  Scenario: Permission-setup requires at least one unverified channel
    Given Tails nominates a dues-expired channel but no unverified channel
    When Tails runs permission-setup
    Then permission-setup fails asking for an unverified channel

  @validation
  Scenario: A channel cannot be both dues-expired and unverified
    Given Tails nominates the same channel as both dues-expired and unverified
    When Tails runs permission-setup
    Then permission-setup fails because the channel sets overlap

  @gate
  Scenario: Apply aborts when the server drifted from the confirmed preview
    Given a plan that will write 3 channels
    When Tails applies a preview that no longer matches the server
    Then apply fails with a plan-changed error
