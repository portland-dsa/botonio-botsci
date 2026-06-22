Feature: Bulk verify sweep and resumable miss queue

  Scenario: Sonic previews an unmanaged sweep
    Given Tails is in the roster holding no managed role, known to us as a Member
    And Shadow is in the roster holding no managed role, unknown to us
    And Knuckles is in the roster already holding the Member role
    When Sonic previews an unmanaged-only sweep
    Then the sweep scans 2 members
    And the sweep matches 1 member as Member
    And the sweep counts 1 miss

  Scenario: Sonic previews a whole-server sweep
    Given Tails is in the roster holding no managed role, known to us as a Member
    And Knuckles is in the roster already holding the Member role
    When Sonic previews a whole-server sweep
    Then the sweep scans 2 members

  Scenario: A bot account is never swept, even in a whole-server sweep
    Given Tails is in the roster holding no managed role, known to us as a Member
    And Metal is a bot in the roster
    When Sonic previews a whole-server sweep
    Then the sweep scans 1 member

  Scenario: Sonic walks the miss queue and finishes it
    Given a started session whose queue is Shadow then Silver
    When Sonic resumes the session
    Then the next pending member is Shadow
    When Sonic marks Shadow verified
    Then the next pending member is Silver
    When Sonic marks Silver skipped
    Then the queue has no pending member
    And the session can be completed

  Scenario: Starting over replaces the queue
    Given a started session whose queue is Shadow then Silver
    When Sonic starts the session over with only Tails
    Then the next pending member is Tails

  Scenario: A queued member already verified elsewhere is skipped on liveness
    Given Shadow is queued but has since been given the Member role
    Then the wizard skips Shadow on the liveness check
