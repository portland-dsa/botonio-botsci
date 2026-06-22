Feature: Verify wizard modal re-click dispatch
  When a moderator dismisses a modal without submitting, the buttons on the host message
  stay live. Pressing the button that opened the modal reopens it; pressing any other
  control - Skip, Stop, Override, or a fresh lookup - hands the press back to the wizard
  instead of reopening the modal. Cast: Ralsei is the moderator running the wizard.

  Scenario: Skip after dismissing the email modal hands off to the wizard
    Given Ralsei opened the email lookup modal
    And Ralsei dismissed the modal without submitting
    When Ralsei presses Skip
    Then the press is handed off to the wizard
    And the modal is not reopened

  Scenario: Stop after dismissing the email modal hands off to the wizard
    Given Ralsei opened the email lookup modal
    And Ralsei dismissed the modal without submitting
    When Ralsei presses Stop
    Then the press is handed off to the wizard
    And the modal is not reopened

  Scenario: Re-pressing Look up by email reopens the email modal
    Given Ralsei opened the email lookup modal
    And Ralsei dismissed the modal without submitting
    When Ralsei presses Look up by email
    Then the modal is reopened

  Scenario: Skip after dismissing the override modal hands off to the wizard
    Given Ralsei opened the override reason modal
    And Ralsei dismissed the modal without submitting
    When Ralsei presses Skip
    Then the press is handed off to the wizard
    And the modal is not reopened

  Scenario: Re-pressing Override and approve reopens the override modal
    Given Ralsei opened the override reason modal
    And Ralsei dismissed the modal without submitting
    When Ralsei presses Override and approve
    Then the modal is reopened
